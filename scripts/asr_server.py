# -*- coding: utf-8 -*-
"""
Faster-Whisper CUDA INT8 WebSocket Streaming ASR Server
Listens on ws://127.0.0.1:18088/asr
"""
import asyncio
import json
import logging
import os
import sys

# Keep a dependency-free executable contract check for the portable launcher.
# It is intentionally handled before optional ML packages are imported so setup
# and behavior tests can verify the path/env contract without model fixtures.
if "--validate-runtime-contract" in sys.argv:
    _contract_keys = ("RUNTIME_ROOT", "SETTINGS_PATH", "MODELS_DIR", "CACHE_DIR")
    try:
        _contract = {
            key: os.path.abspath(os.environ[key]) for key in _contract_keys
        }
    except KeyError as error:
        print(f"missing required runtime environment variable: {error}", file=sys.stderr)
        raise SystemExit(2)
    print(json.dumps(_contract, sort_keys=True))
    raise SystemExit(0)

import numpy as np
import websockets
from faster_whisper import WhisperModel

import torch
from sentence_transformers import SentenceTransformer

# 不要な内部詳細ログを抑制
logging.basicConfig(level=logging.WARNING, format="[%(asctime)s] [%(levelname)s] %(message)s")
logging.getLogger("faster_whisper").setLevel(logging.WARNING)
logging.getLogger("websockets").setLevel(logging.WARNING)
logging.getLogger("sentence_transformers").setLevel(logging.WARNING)

logger = logging.getLogger("ASR-Server")
logger.setLevel(logging.INFO)

# Rust passes the complete portable runtime contract explicitly.  Do not
# consult inherited settings or model/cache settings: those can point outside
# the EXE directory or vary with the process CWD.
RUNTIME_ROOT = os.path.abspath(os.environ["RUNTIME_ROOT"])
SETTINGS_PATH = os.path.abspath(os.environ["SETTINGS_PATH"])
MODELS_DIR = os.path.abspath(os.environ["MODELS_DIR"])
HF_CACHE_DIR = os.path.abspath(os.environ["CACHE_DIR"])
PORT = 18088
# VAD contract: inspect short utterances early enough for a wake word, emit
# revised transcripts as partials, and promote one stable transcript to final
# after silence. Keep these values explicit so the portable server contract
# can be checked without loading the optional ML stack.
MIN_AUDIO_SECONDS = 0.25
PARTIAL_POLL_INTERVAL_SECONDS = 0.08
SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS = 0.75
MAX_AUDIO_BUFFER_SECONDS = 3.0


def remember_transcript(previous: str, candidate: str) -> str:
    """Keep the most complete transcript seen for the current VAD window.

    Whisper is run against a moving audio window. Once the window drops the
    beginning of a long utterance, a later inference can contain only a tail
    (or an unrelated correction) even though an earlier partial contained the
    complete sentence. Finalization must not replace that complete sentence
    with the shorter tail. Containment handles normal incremental revisions;
    for unrelated revisions, length is a conservative proxy for completeness.
    """

    previous = previous.strip()
    candidate = candidate.strip()
    if not candidate:
        return previous
    if not previous:
        return candidate
    if candidate == previous:
        return previous
    if candidate in previous:
        return previous
    if previous in candidate:
        return candidate
    return candidate if len(candidate) > len(previous) else previous


# Keep any library-managed cache beside the portable EXE as well.  Required
# models are downloaded by GameAssistant's Models Manager before this server
# starts; there is deliberately no user-profile/HF-cache fallback.
os.makedirs(HF_CACHE_DIR, exist_ok=True)
os.environ["HF_HOME"] = HF_CACHE_DIR
os.environ["TRANSFORMERS_CACHE"] = HF_CACHE_DIR
os.environ["HF_HUB_CACHE"] = HF_CACHE_DIR
os.environ["HUGGINGFACE_HUB_CACHE"] = HF_CACHE_DIR
os.environ["SENTENCE_TRANSFORMERS_HOME"] = HF_CACHE_DIR

# 1. Faster-Whisper ASR モデルロード
local_whisper_path = os.path.join(MODELS_DIR, "kotoba-whisper-v2.0-faster")
if os.path.exists(local_whisper_path) and (os.path.exists(os.path.join(local_whisper_path, "model.bin")) or os.path.exists(os.path.join(local_whisper_path, "model.safetensors"))):
    whisper_model_source = local_whisper_path
    logger.info(f"Loading local Faster-Whisper model from: {whisper_model_source} (CUDA INT8)...")
else:
    raise RuntimeError(
        f"Required Kotoba-Whisper model is missing at {local_whisper_path}; "
        "complete first-launch setup before starting ASR."
    )

try:
    whisper_model = WhisperModel(whisper_model_source, device="cuda", compute_type="int8")
    logger.info("Faster-Whisper model successfully loaded on CUDA (INT8)!")
except Exception as e:
    logger.warning(f"Failed to load on CUDA: {e}. Falling back to CPU INT8...")
    whisper_model = WhisperModel(whisper_model_source, device="cpu", compute_type="int8")

# 2. GLuCoSE-base-ja ローカル Embedding モデル
_embedding_model = None
_embedding_model_error_logged = False

def get_embedding_model():
    global _embedding_model
    global _embedding_model_error_logged
    if _embedding_model is None:
        local_path = os.path.join(MODELS_DIR, "GLuCoSE-base-ja")
        device = "cuda" if torch.cuda.is_available() else "cpu"
        if os.path.exists(local_path):
            model_name = local_path
            logger.info(f"Loading local embedding model from: {model_name} ({device})...")
        else:
            if not _embedding_model_error_logged:
                logger.error(
                    f"Required GLuCoSE-base-ja model is missing at {local_path}; "
                    "complete first-launch setup before using embeddings."
                )
                _embedding_model_error_logged = True
            return None
        try:
            _embedding_model = SentenceTransformer(model_name, device=device)
            logger.info(f"GLuCoSE-base-ja embedding model successfully loaded on {device}!")
        except Exception as err:
            if not _embedding_model_error_logged:
                logger.error(f"Failed to load embedding model: {err}")
                _embedding_model_error_logged = True
    return _embedding_model

# 3. VRAM 事前確保 (Preallocation) バッファ管理
_vram_preallocate_buffer = None

def set_vram_preallocation(enable: bool) -> bool:
    global _vram_preallocate_buffer
    if enable:
        if _vram_preallocate_buffer is not None:
            return True
        try:
            if torch.cuda.is_available():
                # 1024MB (1GB) の VRAM を PyTorch アロケータで確保
                _vram_preallocate_buffer = torch.zeros((1024, 1024, 256), dtype=torch.float32, device='cuda')
                logger.info("Preallocated 1GB VRAM buffer on CUDA to prevent fragmentation.")
                return True
        except Exception as e:
            logger.warning(f"Failed to preallocate VRAM: {e}")
            _vram_preallocate_buffer = None
            return False
    else:
        _vram_preallocate_buffer = None
        if torch.cuda.is_available():
            torch.cuda.empty_cache()
        logger.info("Freed preallocated VRAM buffer.")
        return True

# 起動時の設定読み込み
try:
    with open(SETTINGS_PATH, "r", encoding="utf-8") as f:
        st = json.load(f)
        if st.get("preallocate_vram", False):
            set_vram_preallocation(True)
except Exception:
    pass


async def asr_handler(websocket):
    sample_rate = 16000
    loop = asyncio.get_running_loop()
    active_stream = "mic"

    def new_stream_state():
        return {
            "audio_buffer": np.array([], dtype=np.float32),
            "last_partial_text": "",
            "silence_start_time": None,
            "audio_started_at": None,
            "last_audio_at": None,
        }

    # Each input stream gets an independent VAD transcript and silence clock.
    # Legacy clients that send raw binary frames without an audio_stream
    # control message continue to use the mic stream by default.
    stream_states = {"mic": new_stream_state(), "discord": new_stream_state()}

    send_queue = asyncio.Queue()

    async def sender():
        try:
            while True:
                msg = await send_queue.get()
                await websocket.send(json.dumps(msg, ensure_ascii=False))
        except Exception:
            pass

    sender_task = asyncio.create_task(sender())

    async def transcribe_buffer(audio_buffer, allow_short=False):
        """Transcribe one VAD window and return text plus inference latency.

        Polling intentionally waits for ``MIN_AUDIO_SECONDS`` so the normal
        partial path does not invoke Whisper for every tiny audio packet.  An
        explicit flush is allowed to transcribe a shorter window: callers use
        that path when VAD produced no partial before the utterance ended.
        """
        if len(audio_buffer) == 0:
            return "", 0.0
        if not allow_short and len(audio_buffer) < sample_rate * MIN_AUDIO_SECONDS:
            return "", 0.0

        buf_copy = audio_buffer.copy()

        def _run_transcribe():
            segments, _ = whisper_model.transcribe(
                buf_copy,
                language="ja",
                beam_size=1,
                vad_filter=True,
                without_timestamps=True,
            )
            return "".join([s.text for s in segments]).strip()

        t0 = loop.time()
        current_text = await loop.run_in_executor(None, _run_transcribe)
        return current_text, (loop.time() - t0) * 1000.0

    def reset_stream_state(state):
        state["audio_buffer"] = np.array([], dtype=np.float32)
        state["last_partial_text"] = ""
        state["silence_start_time"] = None
        state["audio_started_at"] = None
        state["last_audio_at"] = None

    async def inference_loop():
        while True:
            try:
                await asyncio.sleep(PARTIAL_POLL_INTERVAL_SECONDS)

                # Iterate over a snapshot because an explicit stream tag may
                # add a new, non-mic stream while inference is suspended.
                for stream_name, state in list(stream_states.items()):
                    audio_buffer = state["audio_buffer"]
                    now = loop.time()
                    if len(audio_buffer) == 0:
                        continue

                    # A short utterance can end before the normal polling
                    # threshold. Once the input has been quiet for the same
                    # short-utterance timeout, run one final-only inference
                    # instead of leaving the buffer stranded forever.
                    if (
                        len(audio_buffer) < sample_rate * MIN_AUDIO_SECONDS
                        and state["last_audio_at"] is not None
                        and now - state["last_audio_at"]
                        >= SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS
                    ):
                        final_text, final_latency_ms = await transcribe_buffer(
                            audio_buffer, allow_short=True
                        )
                        if final_text:
                            await send_queue.put({
                                "text": final_text,
                                "is_final": True,
                                "stream": stream_name,
                                "latency_ms": round(final_latency_ms, 1),
                            })
                        else:
                            logger.info(
                                "VAD reset[%s]: partial=false final=false "
                                "reason=no_transcript samples=%d",
                                stream_name,
                                len(audio_buffer),
                            )
                        reset_stream_state(state)
                        continue

                    if len(audio_buffer) < sample_rate * MIN_AUDIO_SECONDS:
                        continue

                    current_text, latency_ms = await transcribe_buffer(audio_buffer)

                    last_partial_text = state["last_partial_text"]
                    silence_start_time = state["silence_start_time"]
                    now = loop.time()

                    if current_text:
                        # Whisper's moving VAD window can regress from a full
                        # sentence to a tail once the utterance exceeds the
                        # bounded audio window. Keep the most complete partial
                        # as the candidate that will be promoted to final.
                        stable_text = remember_transcript(
                            last_partial_text, current_text
                        )
                        if stable_text != last_partial_text:
                            await send_queue.put({
                                "text": stable_text,
                                "is_final": False,
                                "stream": stream_name,
                                "latency_ms": round(latency_ms, 1),
                            })
                            state["last_partial_text"] = stable_text
                            state["silence_start_time"] = now
                        elif silence_start_time is None:
                            state["silence_start_time"] = now
                    elif silence_start_time is None:
                        state["silence_start_time"] = now

                    # Bound each VAD stream independently. Keeping the newest
                    # window lets a short wake word be recognized even when an
                    # unrelated stream has a long-running buffer.
                    max_samples = int(sample_rate * MAX_AUDIO_BUFFER_SECONDS)
                    if len(state["audio_buffer"]) > max_samples:
                        state["audio_buffer"] = state["audio_buffer"][-max_samples:]

                    silence_start_time = state["silence_start_time"]
                    if state["last_partial_text"] and silence_start_time:
                        char_count = len(state["last_partial_text"])
                        timeout = (
                            SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS
                            if char_count <= 4
                            else (1.0 if char_count <= 15 else 1.3)
                        )

                        if (now - silence_start_time) >= timeout:
                            final_text = state["last_partial_text"]
                            logger.info(
                                f"Finalize[{stream_name}]: '{final_text}' "
                                f"(latency: {latency_ms:.1f}ms)"
                            )
                            await send_queue.put({
                                "text": final_text,
                                "is_final": True,
                                "stream": stream_name,
                                "latency_ms": round(latency_ms, 1),
                            })
                            state["last_partial_text"] = ""
                            reset_stream_state(state)
                    elif (
                        len(state["audio_buffer"]) > 0
                        and not state["last_partial_text"]
                        and state["last_audio_at"] is not None
                        and now - state["last_audio_at"]
                            >= SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS
                    ):
                        final_text, final_latency_ms = await transcribe_buffer(
                            state["audio_buffer"], allow_short=True
                        )
                        if final_text:
                            await send_queue.put({
                                "text": final_text,
                                "is_final": True,
                                "stream": stream_name,
                                "latency_ms": round(final_latency_ms, 1),
                            })
                        else:
                            logger.info(
                                "VAD reset[%s]: partial=false final=false "
                                "reason=no_transcript samples=%d",
                                stream_name,
                                len(state["audio_buffer"]),
                            )
                        reset_stream_state(state)
                    elif (
                        len(state["audio_buffer"]) > 0
                        and state["audio_started_at"] is not None
                        and now - state["audio_started_at"] >= MAX_AUDIO_BUFFER_SECONDS
                    ):
                        logger.info(
                            "VAD reset[%s]: partial=false final=false "
                            "reason=no_transcript samples=%d",
                            stream_name,
                            len(state["audio_buffer"]),
                        )
                        reset_stream_state(state)

            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Error in inference loop: {e}")
                await asyncio.sleep(0.3)

    inf_task = asyncio.create_task(inference_loop())

    try:
        async for message in websocket:
            if isinstance(message, bytes):
                samples = np.frombuffer(message, dtype=np.float32)
                state = stream_states.setdefault(active_stream, new_stream_state())
                if len(samples) > 0 and state["audio_started_at"] is None:
                    state["audio_started_at"] = loop.time()
                if len(samples) > 0:
                    state["last_audio_at"] = loop.time()
                state["audio_buffer"] = np.concatenate([state["audio_buffer"], samples])
            elif isinstance(message, str):
                try:
                    data = json.loads(message)
                    cmd = data.get("cmd")
                    if cmd == "reset":
                        for state in stream_states.values():
                            reset_stream_state(state)
                        active_stream = "mic"
                    elif cmd == "audio_stream":
                        stream_name = data.get("stream")
                        if isinstance(stream_name, str) and stream_name.strip():
                            active_stream = stream_name.strip()
                            stream_states.setdefault(active_stream, new_stream_state())
                    elif cmd == "flush":
                        # A caller may have VAD audio but no partial delivery
                        # (for example, a short buffer crossing a reconnect).
                        # Expose an explicit finalization path so native code
                        # can preserve the final-only contract instead of
                        # dropping the buffered transcript.
                        stream_name = data.get("stream", active_stream)
                        if not isinstance(stream_name, str) or not stream_name.strip():
                            stream_name = active_stream
                        else:
                            stream_name = stream_name.strip()
                        state = stream_states.get(stream_name)
                        if state and (
                            state["last_partial_text"]
                            or len(state["audio_buffer"]) > 0
                        ):
                            final_text = state["last_partial_text"]
                            final_latency_ms = 0.0
                            if not final_text and len(state["audio_buffer"]) > 0:
                                final_text, final_latency_ms = await transcribe_buffer(
                                    state["audio_buffer"], allow_short=True
                                )
                            if final_text:
                                await send_queue.put({
                                    "text": final_text,
                                    "is_final": True,
                                    "stream": stream_name,
                                    "latency_ms": round(final_latency_ms, 1),
                                })
                            else:
                                logger.info(
                                    "VAD flush[%s]: partial=false final=false "
                                    "reason=no_transcript samples=%d",
                                    stream_name,
                                    len(state["audio_buffer"]),
                                )
                            reset_stream_state(state)
                    elif cmd == "ping":
                        await send_queue.put({"status": "pong"})
                    elif cmd == "embed":
                        req_id = data.get("id", "")
                        texts = data.get("texts", [])
                        if isinstance(texts, str):
                            texts = [texts]

                        def _do_embed():
                            emb_model = get_embedding_model()
                            if emb_model is not None:
                                return emb_model.encode(texts, show_progress_bar=False).tolist()
                            # Do not manufacture zero vectors when the
                            # tokenizer/model is unavailable. An empty
                            # response lets the native client preserve the
                            # raw/summary text while treating the vector as
                            # optional and retryable.
                            return []

                        vectors = await loop.run_in_executor(None, _do_embed)
                        await send_queue.put({
                            "type": "embed_res",
                            "id": req_id,
                            "vectors": vectors,
                        })
                    elif cmd == "preallocate_vram":
                        enable = data.get("enable", True)
                        success = set_vram_preallocation(enable)
                        await send_queue.put({
                            "type": "preallocate_res",
                            "success": success,
                            "enabled": enable,
                        })
                except Exception as err:
                    logger.error(f"Error handling json message: {err}")
    except websockets.exceptions.ConnectionClosed:
        pass
    finally:
        sender_task.cancel()
        inf_task.cancel()


def kill_port_owner(port):
    if sys.platform == "win32":
        try:
            import subprocess
            out = subprocess.check_output(f"netstat -ano -p tcp | findstr :{port}", shell=True).decode()
            my_pid = os.getpid()
            for line in out.strip().split("\n"):
                parts = line.split()
                if len(parts) >= 5 and parts[1].endswith(f":{port}"):
                    pid = int(parts[-1])
                    if pid != my_pid and pid > 0:
                        logger.info(f"Terminating lingering process (PID {pid}) on port {port}...")
                        subprocess.run(f"taskkill /F /T /PID {pid}", shell=True, capture_output=True)
        except Exception:
            pass


async def main():
    for attempt in range(5):
        try:
            async with websockets.serve(asr_handler, "127.0.0.1", PORT):
                logger.info(f"ASR WebSocket Server running at ws://127.0.0.1:{PORT}/asr")
                await asyncio.Future()
            break
        except OSError as e:
            if attempt < 4:
                logger.warning(f"Port {PORT} in use, terminating lingering process and retrying in 1s (attempt {attempt+1}/5)...")
                kill_port_owner(PORT)
                await asyncio.sleep(1)
            else:
                logger.error(f"Failed to bind to port {PORT}: {e}")
                raise


if __name__ == "__main__":
    asyncio.run(main())

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

# モデル保存先ディレクトリの取得
def get_models_dir() -> str:
    # 1. 環境変数
    if "MODELS_DIR" in os.environ and os.path.exists(os.environ["MODELS_DIR"]):
        return os.environ["MODELS_DIR"]
    # 2. settings.json
    try:
        if os.path.exists("settings.json"):
            with open("settings.json", "r", encoding="utf-8") as f:
                st = json.load(f)
                if "models_dir" in st and os.path.exists(st["models_dir"]):
                    return st["models_dir"]
    except Exception:
        pass
    # 3. デフォルト
    return "./models"

MODELS_DIR = get_models_dir()
PORT = 18088

# 1. Faster-Whisper ASR モデルロード
local_whisper_path = os.path.join(MODELS_DIR, "kotoba-whisper-v2.0-faster")
if os.path.exists(local_whisper_path) and (os.path.exists(os.path.join(local_whisper_path, "model.bin")) or os.path.exists(os.path.join(local_whisper_path, "model.safetensors"))):
    whisper_model_source = local_whisper_path
    logger.info(f"Loading local Faster-Whisper model from: {whisper_model_source} (CUDA INT8)...")
else:
    whisper_model_source = "kotoba-tech/kotoba-whisper-v2.0-faster"
    logger.info(f"Loading Faster-Whisper model from Hugging Face: {whisper_model_source} (CUDA INT8)...")

try:
    whisper_model = WhisperModel(whisper_model_source, device="cuda", compute_type="int8")
    logger.info("Faster-Whisper model successfully loaded on CUDA (INT8)!")
except Exception as e:
    logger.warning(f"Failed to load on CUDA: {e}. Falling back to CPU INT8...")
    whisper_model = WhisperModel(whisper_model_source, device="cpu", compute_type="int8")

# 2. GLuCoSE-base-ja ローカル Embedding モデル
_embedding_model = None

def get_embedding_model():
    global _embedding_model
    if _embedding_model is None:
        local_path = os.path.join(get_models_dir(), "GLuCoSE-base-ja")
        device = "cuda" if torch.cuda.is_available() else "cpu"
        if os.path.exists(local_path):
            model_name = local_path
            logger.info(f"Loading local embedding model from: {model_name} ({device})...")
        else:
            model_name = "pkshatech/GLuCoSE-base-ja"
            logger.info(f"Loading embedding model from Hugging Face: {model_name} ({device})...")
        try:
            _embedding_model = SentenceTransformer(model_name, device=device)
            logger.info(f"GLuCoSE-base-ja embedding model successfully loaded on {device}!")
        except Exception as err:
            logger.error(f"Failed to load embedding model: {err}")
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
    with open("settings.json", "r", encoding="utf-8") as f:
        st = json.load(f)
        if st.get("preallocate_vram", False):
            set_vram_preallocation(True)
except Exception:
    pass


async def asr_handler(websocket):
    audio_buffer = np.array([], dtype=np.float32)
    last_partial_text = ""
    silence_start_time = None
    sample_rate = 16000
    loop = asyncio.get_running_loop()

    send_queue = asyncio.Queue()

    async def sender():
        try:
            while True:
                msg = await send_queue.get()
                await websocket.send(json.dumps(msg, ensure_ascii=False))
        except Exception:
            pass

    sender_task = asyncio.create_task(sender())

    async def inference_loop():
        nonlocal audio_buffer, last_partial_text, silence_start_time
        while True:
            try:
                await asyncio.sleep(0.08)

                if len(audio_buffer) < sample_rate * 0.4:
                    continue

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
                latency_ms = (loop.time() - t0) * 1000.0

                if current_text:
                    if current_text != last_partial_text:
                        await send_queue.put({
                            "text": current_text,
                            "is_final": False,
                            "latency_ms": round(latency_ms, 1),
                        })
                        last_partial_text = current_text
                        silence_start_time = loop.time()
                    else:
                        if silence_start_time is None:
                            silence_start_time = loop.time()
                else:
                    if silence_start_time is None:
                        silence_start_time = loop.time()

                    if len(audio_buffer) > sample_rate * 3:
                        audio_buffer = audio_buffer[-int(sample_rate * 0.5):]

                if last_partial_text and silence_start_time:
                    char_count = len(last_partial_text)
                    timeout = 0.75 if char_count <= 4 else (1.0 if char_count <= 15 else 1.3)

                    if (loop.time() - silence_start_time) >= timeout:
                        logger.info(f"Finalize: '{last_partial_text}' (latency: {latency_ms:.1f}ms)")
                        await send_queue.put({
                            "text": last_partial_text,
                            "is_final": True,
                            "latency_ms": round(latency_ms, 1),
                        })
                        last_partial_text = ""
                        audio_buffer = np.array([], dtype=np.float32)
                        silence_start_time = None

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
                audio_buffer = np.concatenate([audio_buffer, samples])
            elif isinstance(message, str):
                try:
                    data = json.loads(message)
                    cmd = data.get("cmd")
                    if cmd == "reset":
                        audio_buffer = np.array([], dtype=np.float32)
                        last_partial_text = ""
                        silence_start_time = None
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
                            return [[0.0] * 768 for _ in texts]

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

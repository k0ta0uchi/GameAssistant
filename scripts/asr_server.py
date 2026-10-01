"""
Faster-Whisper CUDA INT8 WebSocket Streaming ASR Server
Listens on ws://127.0.0.1:18088/asr
"""

import asyncio
import json
import logging
import os
import sys
import threading
import time
import traceback
from datetime import datetime

# Keep a dependency-free executable contract check for the portable launcher.
# It is intentionally handled before optional ML packages are imported so setup
# and behavior tests can verify the path/env contract without model fixtures.
if "--validate-runtime-contract" in sys.argv:
    _contract_keys = ("RUNTIME_ROOT", "SETTINGS_PATH", "MODELS_DIR", "CACHE_DIR")
    try:
        _contract = {key: os.path.abspath(os.environ[key]) for key in _contract_keys}
    except KeyError as error:
        print(
            f"missing required runtime environment variable: {error}", file=sys.stderr
        )
        raise SystemExit(2)
    print(json.dumps(_contract, sort_keys=True))
    raise SystemExit(0)

import numpy as np
import torch
import websockets
from faster_whisper import WhisperModel
from sentence_transformers import SentenceTransformer

# 不要な内部詳細ログを抑制
logging.basicConfig(
    level=logging.WARNING, format="[%(asctime)s] [%(levelname)s] %(message)s"
)
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
SAMPLE_RATE = 16000
MIN_AUDIO_SECONDS = 0.25
PRE_ROLL_SECONDS = 0.20
SPEECH_START_CONSECUTIVE_FRAMES = 3  # 60ms of speech (3 x 20ms frames)
SPEECH_END_SILENCE_SECONDS = 0.65  # 650ms of real audio silence for speech-end
MAX_UTTERANCE_SECONDS = 30.0  # Full utterance safe bound (do not truncate at 3s)
VAD_ENERGY_THRESHOLD = 0.012  # RMS threshold for speech/silence detection
VAD_FRAME_SIZE = 320  # 20ms @ 16kHz

# Partial inference optimization intervals
PARTIAL_POLL_INTERVAL_SECONDS = 0.08
PARTIAL_INTERVAL_GPU_SECONDS = 0.35  # GPU: 300-400ms (350ms)
PARTIAL_INTERVAL_CPU_SECONDS = 0.65  # CPU: 500-800ms (650ms)
MIN_PARTIAL_INCREMENT_SECONDS = 0.20  # Minimum new audio before re-inference
SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS = 0.75
MAX_AUDIO_BUFFER_SECONDS = 30.0  # Legacy compatibility constant



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
try:
    os.makedirs(HF_CACHE_DIR, exist_ok=True)
except OSError as cache_dir_err:
    logger.warning(f"Failed to create HF cache dir {HF_CACHE_DIR}: {cache_dir_err}")
os.environ["HF_HOME"] = HF_CACHE_DIR
os.environ["TRANSFORMERS_CACHE"] = HF_CACHE_DIR
os.environ["HF_HUB_CACHE"] = HF_CACHE_DIR
os.environ["HUGGINGFACE_HUB_CACHE"] = HF_CACHE_DIR
os.environ["SENTENCE_TRANSFORMERS_HOME"] = HF_CACHE_DIR

# 1. Faster-Whisper ASR モデルロード
local_whisper_path = os.path.join(MODELS_DIR, "kotoba-whisper-v2.0-faster")
if os.path.exists(local_whisper_path) and (
    os.path.exists(os.path.join(local_whisper_path, "model.bin"))
    or os.path.exists(os.path.join(local_whisper_path, "model.safetensors"))
):
    whisper_model_source = local_whisper_path
else:
    raise RuntimeError(
        f"Required Kotoba-Whisper model is missing at {local_whisper_path}; "
        "complete first-launch setup before starting ASR."
    )

def get_arg_or_env(arg_names, env_names, default=None, argv=None, environ=None):
    if argv is None:
        argv = sys.argv
    if environ is None:
        environ = os.environ
    for arg in arg_names:
        if arg in argv:
            idx = argv.index(arg)
            if idx + 1 < len(argv):
                return argv[idx + 1]
    for env in env_names:
        if env in environ:
            return environ[env]
    return default


forced_device = get_arg_or_env(
    ["--device", "--force-device"], ["ASR_DEVICE", "FORCE_DEVICE"]
)
if forced_device:
    forced_device = forced_device.lower()

current_device = "cpu" if forced_device == "cpu" else "cuda"

model_preset = get_arg_or_env(
    ["--model-preset", "--preset"], ["ASR_MODEL_PRESET", "MODEL_PRESET"]
)
if model_preset:
    model_preset = model_preset.lower()


def resolve_compute_types(argv=None, environ=None) -> tuple[str, str]:
    """Resolve (gpu_compute_type, cpu_compute_type) respecting device overrides and common fallback."""
    common = get_arg_or_env(
        ["--compute-type"],
        ["ASR_COMPUTE_TYPE"],
        default=None,
        argv=argv,
        environ=environ,
    )
    gpu = get_arg_or_env(
        ["--gpu-compute-type"],
        ["ASR_GPU_COMPUTE_TYPE"],
        default=common or "int8",
        argv=argv,
        environ=environ,
    )
    cpu = get_arg_or_env(
        ["--cpu-compute-type"],
        ["ASR_CPU_COMPUTE_TYPE"],
        default=common or "int8_float32",
        argv=argv,
        environ=environ,
    )
    return gpu, cpu


gpu_compute_type, cpu_compute_type = resolve_compute_types()

cpu_threads_raw = get_arg_or_env(["--cpu-threads"], ["ASR_CPU_THREADS"], default=None)
current_cpu_threads = (
    int(cpu_threads_raw)
    if (cpu_threads_raw and cpu_threads_raw.isdigit())
    else min(4, os.cpu_count() or 4)
)

current_model_name = ""
current_compute_type = gpu_compute_type if current_device == "cuda" else cpu_compute_type


def get_model_spec(preset: str | None, device: str) -> tuple[str, str]:
    small_path = os.path.join(MODELS_DIR, "faster-whisper-small")
    has_small = os.path.exists(small_path) and (
        os.path.exists(os.path.join(small_path, "model.bin"))
        or os.path.exists(os.path.join(small_path, "model.safetensors"))
    )
    if preset == "fast":
        if has_small:
            return small_path, "faster-whisper-small"
        else:
            logger.warning(
                "Fast preset requested but faster-whisper-small not found; "
                "falling back to Kotoba-Whisper."
            )
            return whisper_model_source, "kotoba-whisper-v2.0-faster"
    if preset == "quality":
        return whisper_model_source, "kotoba-whisper-v2.0-faster"

    # preset未指定時: CPUでsmallがあれば既存動作互換でsmall、なければkotoba。GPUならkotoba
    if device == "cpu" and has_small:
        return small_path, "faster-whisper-small"
    return whisper_model_source, "kotoba-whisper-v2.0-faster"


def create_cpu_whisper_model(
    preset=model_preset, threads=current_cpu_threads, compute_type=cpu_compute_type
):
    """CPU モデルを初期化。preset (quality / fast) や compute_type, threads を明示指定可能。"""
    global current_model_name, current_compute_type, current_cpu_threads
    model_source, model_name = get_model_spec(preset, "cpu")
    current_model_name = model_name
    current_compute_type = compute_type
    current_cpu_threads = threads

    logger.info(
        f"Initializing CPU Faster-Whisper model from {model_name} "
        f"(compute_type={compute_type}, cpu_threads={threads})..."
    )
    try:
        return WhisperModel(
            model_source,
            device="cpu",
            compute_type=compute_type,
            cpu_threads=threads,
            num_workers=1,
        )
    except Exception as e:
        logger.warning(
            f"Failed to load CPU model with {compute_type}: {e}. Retrying with float32..."
        )
        current_compute_type = "float32"
        return WhisperModel(
            model_source,
            device="cpu",
            compute_type="float32",
            cpu_threads=threads,
            num_workers=1,
        )


if current_device == "cuda":
    model_source, model_name = get_model_spec(model_preset or "quality", "cuda")
    current_model_name = model_name
    current_compute_type = gpu_compute_type
    logger.info(
        f"Loading local Faster-Whisper model from: {model_source} (CUDA {gpu_compute_type})..."
    )
    try:
        whisper_model = WhisperModel(
            model_source, device="cuda", compute_type=gpu_compute_type
        )
        logger.info(f"Faster-Whisper model successfully loaded on CUDA ({gpu_compute_type})!")
    except Exception as e:
        logger.warning(f"Failed to load on CUDA: {e}. Falling back to CPU...")
        whisper_model = create_cpu_whisper_model()
        current_device = "cpu"
else:
    whisper_model = create_cpu_whisper_model()
    current_device = "cpu"


REASON_GPU_OOM = "gpu_oom"
REASON_INFERENCE_TIMEOUT = "inference_timeout"
REASON_CUDA_ERROR = "cuda_error"

# 連続推論タイムアウトがこの回数に達したら、プロセス内でのモデル切替ではなく
# 自身を終了し、外側の監督処理 (Rust ASR エンジンの watchdog / child-exit 検知)
# に子プロセスの終了・再生成を委ねる。
INFERENCE_STALL_EXIT_THRESHOLD = 3

FATAL_CUDA_PATTERNS = (
    "device-side assert",
    "illegal memory access",
    "an illegal instruction was encountered",
)

_switch_diagnostics: list = []
MAX_SWITCH_DIAGNOSTICS = 20
last_switch_reason = None
inference_stall_streak = 0

# The admission lock below covers queued/native inference AND model replacement.
# A timed-out caller must not release it while the executor still owns the job.
INFERENCE_IN_FLIGHT_EXIT_SECONDS = 30.0
in_flight_since = None
_in_flight_skip_log_armed = True


def is_cuda_oom_error(exc: BaseException) -> bool:
    """真のVRAM枯渇のみを検出する。

    文字列判定は確認済みのメモリ不足メッセージに限定する。API名
    ('cudamalloc' など) や 'cuda' という語の部分一致では原因を確定できない
    (VRAM空きがある一時的エラーまで恒久CPU切替していた過去事故の教訓)。
    CTranslate2 は torch.cuda.OutOfMemoryError を送出しないためメッセージで判定する
    (依存なしで単体テスト可能)。
    """
    text = str(exc).lower()
    return (
        "out of memory" in text
        or "out_of_memory" in text
        or "cuda memory allocation failed" in text
    )


def vram_snapshot() -> dict:
    """取得時刻付きの空き/合計VRAM (取得不可なら available=False)。"""
    try:
        if torch.cuda.is_available():
            free_b, total_b = torch.cuda.mem_get_info()
            return {
                "available": True,
                "free_mib": round(free_b / (1024**2)),
                "total_mib": round(total_b / (1024**2)),
                "captured_at": datetime.now().isoformat(timespec="milliseconds"),
            }
    except Exception as snapshot_err:
        logger.debug(f"VRAM snapshot unavailable: {snapshot_err}")
    return {"available": False}


def vram_status_text() -> str:
    """診断用: 現在の空き/合計VRAMを短い文字列で返す (CUDA不可なら 'n/a')。"""
    snap = vram_snapshot()
    if snap.get("available"):
        return f"{snap['free_mib']}MiB free / {snap['total_mib']}MiB total"
    return "n/a"


def is_fatal_cuda_error(exc: BaseException) -> bool:
    """CUDAコンテキスト自体の破壊 (回復不能) を検出する。プロセス再起動対象。"""
    text = str(exc).lower()
    return any(pattern in text for pattern in FATAL_CUDA_PATTERNS)


def classify_inference_error(exc: BaseException) -> str:
    """推論例外を gpu_oom / cuda_error / input_error に分類する。"""
    if is_cuda_oom_error(exc) or isinstance(exc, torch.cuda.OutOfMemoryError):
        return REASON_GPU_OOM
    text = str(exc).lower()
    if "cuda" in text or "cudnn" in text or "cublas" in text:
        return REASON_CUDA_ERROR
    return "input_error"


def record_switch_diagnostic(reason, exc=None, elapsed_ms=None, extra=None):
    """デバイス切替/スタールの確定証跡を記録する (リングバッファ + ERROR ログ)。

    例外の型・全文・スタック、処理時間、取得時刻付きVRAM空き容量、切替を
    決めた条件を 1 レコードにまとめる。「4GB空いていた」という観測だけでは
    確定できなかった誤判定/実確保失敗の切り分け (例外経路 vs タイムアウト経路
    の判別を含む) を可能にする。
    """
    entry = {
        "timestamp": datetime.now().isoformat(timespec="milliseconds"),
        "reason": reason,
        "device_before": current_device,
        "exception_type": type(exc).__name__ if exc is not None else None,
        "exception": str(exc) if exc is not None else None,
        "traceback": traceback.format_exc(limit=10) if exc is not None else None,
        "elapsed_ms": round(elapsed_ms, 1) if elapsed_ms is not None else None,
        "vram": vram_snapshot(),
    }
    if extra:
        entry.update(extra)
    _switch_diagnostics.append(entry)
    del _switch_diagnostics[:-MAX_SWITCH_DIAGNOSTICS]
    logger.error(
        "device_switch_diagnostic %s",
        json.dumps(entry, ensure_ascii=False, default=str),
    )
    return entry


def record_inference_stall(elapsed_ms: float) -> int:
    """推論タイムアウトを記録する。モデルは切替えない (実行中スレッド保護)。

    連続スタールが閾値に達したらプロセスを終了し、復旧は外側の監督処理が
    行う子プロセスの再生成に委ねる。
    """
    global inference_stall_streak
    inference_stall_streak += 1
    record_switch_diagnostic(
        REASON_INFERENCE_TIMEOUT,
        elapsed_ms=elapsed_ms,
        extra={"streak": inference_stall_streak},
    )
    if inference_stall_streak >= INFERENCE_STALL_EXIT_THRESHOLD:
        logger.critical(
            f"GPU inference stalled {inference_stall_streak} consecutive times "
            f"(reason={REASON_INFERENCE_TIMEOUT}). Exiting so the supervisor can "
            "restart the ASR server process."
        )
        os._exit(87)
    return inference_stall_streak


def reset_inference_stall_streak() -> None:
    global inference_stall_streak
    inference_stall_streak = 0


def note_in_flight_gate() -> bool:
    """in-flight 状態が許容時間を超えたかを判定する (超過時はプロセス終了対象)。"""
    global in_flight_since
    now = time.monotonic()
    if in_flight_since is None:
        in_flight_since = now
        return False
    return (now - in_flight_since) >= INFERENCE_IN_FLIGHT_EXIT_SECONDS


def clear_in_flight_gate() -> None:
    global in_flight_since, _in_flight_skip_log_armed
    in_flight_since = None
    _in_flight_skip_log_armed = True


def note_in_flight_skip_log() -> bool:
    """スタール期間で最初にブロックされたポーリングのみ True (ログのレート制限)。"""
    global _in_flight_skip_log_armed
    if _in_flight_skip_log_armed:
        _in_flight_skip_log_armed = False
        return True
    return False


class _CpuFallbackRequest(Exception):
    """真のOOM等でCPUモデルへの切替を要求する内部シグナル。

    Native inference has unwound before the wrapper handles this signal.
    The wrapper retains exclusive admission through model replacement and retry.
    """

    def __init__(self, reason: str, exc: BaseException, elapsed_ms: float):
        super().__init__(reason)
        self.reason = reason
        self.exc = exc
        self.elapsed_ms = elapsed_ms


# Acquired before executor submission; released by that worker after all native
# work, including CPU fallback, finishes. Admission uses nonblocking acquisition
# so the WebSocket event loop never waits synchronously for a model load.
_device_switch_lock = threading.Lock()


def _perform_cpu_switch(reason: str, exc=None, elapsed_ms=None) -> None:
    """GPUモデルを破棄してCPUモデルへ切替える (_device_switch_lock 下で呼ぶ)。

    The failed native inference must have returned; admission remains locked.
    """
    global whisper_model, current_device, last_switch_reason
    record_switch_diagnostic(reason, exc=exc, elapsed_ms=elapsed_ms)
    last_switch_reason = reason
    logger.warning(
        f"Switching ASR from GPU to CPU (reason={reason}) "
        f"[VRAM: {vram_status_text()}]..."
    )
    try:
        del whisper_model
        if torch.cuda.is_available():
            torch.cuda.empty_cache()
    except Exception as err:
        logger.debug(f"Error clearing CUDA cache during fallback: {err}")
    try:
        loaded_cpu_model = create_cpu_whisper_model()
        whisper_model = loaded_cpu_model
    except Exception as load_err:
        # CPU モデルのロードに失敗: whisper_model が未束納のまま残り、以降の
        # 推論が全て失敗する。プロセス内での自己修復は不可能なため、監視側の
        # 子プロセス再生成に委ねて終了する。
        record_switch_diagnostic(
            f"{reason}_load_failed",
            exc=load_err,
            elapsed_ms=elapsed_ms,
            extra={"fatal": True},
        )
        logger.critical(
            f"CPU model load failed after fallback ({load_err}); exiting for "
            "supervised process restart."
        )
        os._exit(87)
    current_device = "cpu"
    logger.info("Successfully switched Faster-Whisper model to CPU!")


# 2. GLuCoSE-base-ja ローカル Embedding モデル
_embedding_model = None
_embedding_model_error_logged = False


def get_embedding_model():
    global _embedding_model
    global _embedding_model_error_logged
    if _embedding_model is None:
        local_path = os.path.join(MODELS_DIR, "GLuCoSE-base-ja")
        # GLuCoSE を CPU に固定: whisper (CTranslate2) と同じ CUDA で並走すると
        # 推論が 2 秒超えでスタールすることがある (GPU 競合)。埋め込み対象は
        # 短文が主で CPU でも十分な速度。
        device = "cpu"
        if os.path.exists(local_path):
            model_name = local_path
            logger.info(
                f"Loading local embedding model from: {model_name} ({device})..."
            )
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
            logger.info(
                f"GLuCoSE-base-ja embedding model successfully loaded on {device}!"
            )
        except Exception as err:
            if not _embedding_model_error_logged:
                logger.error(f"Failed to load embedding model: {err}")
                _embedding_model_error_logged = True
    return _embedding_model


# 3. VRAM 事前確保 (Preallocation) の無力化（不要な1GBダミー確保を廃止）
_vram_preallocate_buffer = None


def set_vram_preallocation(enable: bool) -> bool:
    # 廃止: PyTorch 1GB 確保は CTranslate2 で再利用できず、VRAMを圧迫するため無効化
    logger.info(
        "VRAM preallocation request handled (no-op: CTranslate2 manages its own workspace memory)."
    )
    return True


def new_stream_state():
    return {
        # Audio Buffers: full utterance PCM for complete final transcription
        "full_utterance_pcm": np.array([], dtype=np.float32),
        "pre_roll_pcm": np.array([], dtype=np.float32),
        "vad_pending_pcm": np.array([], dtype=np.float32),
        "audio_buffer": np.array([], dtype=np.float32),
        "last_partial_text": "",
        "last_partial_pcm_len": 0,
        "last_partial_time": 0.0,
        # VAD & Endpointing State
        "is_speaking": False,
        "speech_start_time": None,
        "last_voice_at": None,
        "speech_frame_count": 0,
        "silence_frame_count": 0,
        "force_endpoint": False,
        # Diagnostic Metrics
        "partial_count": 0,
        "partial_latencies": [],
        "utterance_started_monotonic": None,
        "audio_started_at": None,
        "last_audio_at": None,
    }


def reset_stream_state(state):
    state["full_utterance_pcm"] = np.array([], dtype=np.float32)
    state["pre_roll_pcm"] = np.array([], dtype=np.float32)
    state["vad_pending_pcm"] = np.array([], dtype=np.float32)
    state["audio_buffer"] = np.array([], dtype=np.float32)
    state["last_partial_text"] = ""
    state["last_partial_pcm_len"] = 0
    state["last_partial_time"] = 0.0
    state["is_speaking"] = False
    state["speech_start_time"] = None
    state["last_voice_at"] = None
    state["speech_frame_count"] = 0
    state["silence_frame_count"] = 0
    state["force_endpoint"] = False
    state["partial_count"] = 0
    state["partial_latencies"] = []
    state["utterance_started_monotonic"] = None
    state["audio_started_at"] = None
    state["last_audio_at"] = None


def update_vad_and_buffers(
    state,
    samples,
    now,
    sample_rate=SAMPLE_RATE,
    monotonic_time=None,
):
    """Update VAD state, voice timestamps, and PCM buffers with new audio samples."""
    if len(samples) == 0:
        return

    if monotonic_time is None:
        monotonic_time = time.monotonic()

    # 前回の20ms未満の端数PCMと結合して320 samples (20ms) 単位でVAD判定を実行
    pending = state.get("vad_pending_pcm")
    if pending is not None and len(pending) > 0:
        combined = np.concatenate([pending, samples])
    else:
        combined = samples

    frame_size = VAD_FRAME_SIZE
    num_frames = len(combined) // frame_size
    consumed_samples = num_frames * frame_size

    # 20ms フレームごとに RMS を計算して VAD 状態を更新
    for i in range(num_frames):
        frame = combined[i * frame_size : (i + 1) * frame_size]
        rms = float(np.sqrt(np.mean(frame**2)))

        if rms >= VAD_ENERGY_THRESHOLD:
            state["speech_frame_count"] += 1
            state["silence_frame_count"] = 0
            state["last_voice_at"] = now

            if not state["is_speaking"]:
                if state["speech_frame_count"] >= SPEECH_START_CONSECUTIVE_FRAMES:
                    state["is_speaking"] = True
                    state["speech_start_time"] = now
                    state["utterance_started_monotonic"] = monotonic_time
                    state["partial_count"] = 0
                    state["partial_latencies"] = []
                    # pre-roll (直前200ms) を発話全体バッファの先頭に付加して文頭欠落を防止
                    state["full_utterance_pcm"] = state["pre_roll_pcm"].copy()
                    state["last_partial_pcm_len"] = 0
        else:
            state["silence_frame_count"] += 1
            state["speech_frame_count"] = 0

    # 20ms未満の残余サンプルを次回へ持ち越す（任意のchunk boundary耐性を保証）
    state["vad_pending_pcm"] = combined[consumed_samples:]

    # PCM 蓄積 (全サンプルを確実にバッファへ蓄積)
    if state["is_speaking"]:
        state["full_utterance_pcm"] = np.concatenate(
            [state["full_utterance_pcm"], samples]
        )
        state["audio_buffer"] = state["full_utterance_pcm"]
        max_samples = int(sample_rate * MAX_UTTERANCE_SECONDS)
        if len(state["full_utterance_pcm"]) > max_samples:
            state["force_endpoint"] = True
    else:
        state["pre_roll_pcm"] = np.concatenate([state["pre_roll_pcm"], samples])
        pre_roll_max = int(sample_rate * PRE_ROLL_SECONDS)
        if len(state["pre_roll_pcm"]) > pre_roll_max:
            state["pre_roll_pcm"] = state["pre_roll_pcm"][-pre_roll_max:]
        state["audio_buffer"] = state["pre_roll_pcm"]


async def asr_handler(websocket):
    sample_rate = 16000
    loop = asyncio.get_running_loop()
    active_stream = "mic"

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
        except Exception as sender_err:
            logger.debug(f"Sender task terminated: {sender_err}")

    sender_task = asyncio.create_task(sender())
    await send_queue.put({"type": "device_status", "device": current_device})

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

        if not _device_switch_lock.acquire(blocking=False):
            # 前回タイムアウトした推論がまだ実行中: 旧モデルを使うスレッドが残って
            # いるため新規推論を投入しない (並行競合防止)。窓はスキップし、in-flight
            # が許容時間を超えて滞留する場合はプロセス終了して監視側の再生成に委ねる。
            # 80ms ポーリング毎のログはスパムになるため、スタール期間の最初の
            # 1回だけ記録する。
            if note_in_flight_gate():
                record_switch_diagnostic(
                    REASON_INFERENCE_TIMEOUT,
                    extra={"stall": "in_flight_timeout"},
                )
                logger.critical(
                    "GPU inference stayed in flight for over "
                    f"{INFERENCE_IN_FLIGHT_EXIT_SECONDS:.0f}s; exiting so the "
                    "supervisor can restart the ASR server process."
                )
                os._exit(87)
            if note_in_flight_skip_log():
                logger.warning(
                    "Skipping inference window: previous inference still in flight "
                    "(further skip logs suppressed until it finishes)."
                )
            return "", 0.0

        clear_in_flight_gate()

        def _run_transcribe():
            # This worker owns the reservation made before submission, even if
            # its caller times out or disconnects. No other inference or switch
            # can enter until the native call, model load and retry have ended.
            try:
                try:
                    return _run_transcribe_inner()
                except _CpuFallbackRequest as request:
                    _perform_cpu_switch(
                        request.reason, exc=request.exc, elapsed_ms=request.elapsed_ms
                    )
                    return _run_transcribe_inner()
            finally:
                _device_switch_lock.release()

        def _run_transcribe_inner():
            nonlocal buf_copy
            if (
                "--simulate-hang" in sys.argv
                or os.environ.get("SIMULATE_ASR_HANG") == "1"
            ):
                # 故障注入 (診断用フラグ): ネイティブ推論のハングを再現する。
                _fault_injection_hang(60)

            def _transcribe_text():
                segments, _ = whisper_model.transcribe(
                    buf_copy,
                    language="ja",
                    beam_size=1,
                    vad_filter=True,
                    without_timestamps=True,
                    condition_on_previous_text=False,  # 幻覚ループ（「ありがとう」連鎖）を完全遮断
                    no_speech_threshold=0.6,  # 無音時の幻覚テキスト出力を抑止
                    compression_ratio_threshold=2.4,  # 反復ループ幻覚を破棄
                    hallucination_silence_threshold=0.5,  # 無音区間の幻覚を除去
                )
                return "".join([s.text for s in segments]).strip()

            call_started = time.monotonic()
            try:
                return _transcribe_text()
            except Exception as e:
                if current_device != "cuda":
                    raise
                elapsed_ms = (time.monotonic() - call_started) * 1000.0
                kind = classify_inference_error(e)

                if kind == REASON_GPU_OOM:
                    # Unwind native inference before switching under admission.
                    raise _CpuFallbackRequest(REASON_GPU_OOM, e, elapsed_ms) from e

                if is_fatal_cuda_error(e):
                    # CUDAコンテキスト破壊系 (device-side assert 等):
                    # プロセス内復旧は不可能。外側の監督処理に子プロセスの
                    # 再生成を委ねるため終了する。
                    record_switch_diagnostic(
                        REASON_CUDA_ERROR,
                        exc=e,
                        elapsed_ms=elapsed_ms,
                        extra={"fatal": True},
                    )
                    logger.critical(
                        f"Fatal CUDA fault during inference ({e}); exiting for "
                        "supervised process restart."
                    )
                    os._exit(87)

                if kind == REASON_CUDA_ERROR:
                    # 回復可能な一時的CUDA障害のみ再試行する (入力不正などは
                    # そのまま報告)。キャッシュ解放後にGPUで1回だけ再試行し、
                    # 再失敗時のみCPUへ切替。
                    logger.warning(
                        f"Transient CUDA error during inference ({e}) "
                        f"[VRAM: {vram_status_text()}]. Clearing CUDA cache and "
                        "retrying once on GPU..."
                    )
                    try:
                        torch.cuda.empty_cache()
                    except Exception as cache_err:
                        logger.debug(f"Failed to clear CUDA cache: {cache_err}")
                    try:
                        return _transcribe_text()
                    except Exception as retry_err:
                        logger.warning(
                            f"GPU retry after transient CUDA error failed "
                            f"({retry_err}). Falling back to CPU..."
                        )
                        # Unwind native inference before switching under admission.
                        raise _CpuFallbackRequest(
                            REASON_CUDA_ERROR, retry_err, elapsed_ms
                        ) from retry_err

                # 入力不正などCUDA以外の例外: 切替も再試行もせずそのまま報告する
                raise

        prev_dev = current_device
        t0 = loop.time()
        current_text = ""
        try:
            inference = loop.run_in_executor(None, _run_transcribe)
        except BaseException:
            # No worker accepted ownership, so the submitting caller releases it.
            _device_switch_lock.release()
            raise

        def observe_completion(future):
            # Retrieve late failures even when the caller has timed out/disconnected.
            if not future.cancelled():
                error = future.exception()
                if error is not None:
                    logger.error(
                        "ASR inference worker failed: %s",
                        error,
                        exc_info=(type(error), error, error.__traceback__),
                    )

        inference.add_done_callback(observe_completion)
        try:
            current_text = await asyncio.wait_for(
                # Cancellation must not cancel a queued job: its finally owns
                # releasing admission, and it must run even without a waiter.
                asyncio.shield(inference),
                timeout=3.0,
            )
        except asyncio.TimeoutError:
            # タイムアウトはVRAM枯渇 (OOM) とは無関係。wait_for 打切後も実行中の
            # 推論スレッドが旧モデルを使い続けるため、ここでモデルを切替えては
            # いけない。窓を1つスキップして継続し、連続スタールが閾値に達したら
            # record_inference_stall がプロセスを終了し、外側の監督処理 (Rust
            # ASR エンジン) が子プロセスを再生成する。
            streak = record_inference_stall((loop.time() - t0) * 1000.0)
            logger.warning(
                f"Inference exceeded 3.0s (reason={REASON_INFERENCE_TIMEOUT}, "
                f"streak={streak}/{INFERENCE_STALL_EXIT_THRESHOLD}, "
                f"VRAM: {vram_status_text()}). Skipping this window; "
                "the GPU model is kept in place."
            )
            current_text = ""
        else:
            reset_inference_stall_streak()

        if current_device != prev_dev:
            await send_queue.put(
                {
                    "type": "device_changed",
                    "device": current_device,
                    "reason": last_switch_reason or REASON_GPU_OOM,
                }
            )

        return current_text, (loop.time() - t0) * 1000.0

    async def inference_loop():
        while True:
            try:
                poll_interval = 0.05 if current_device == "cuda" else 0.08
                await asyncio.sleep(poll_interval)
                now = loop.time()

                partial_interval = (
                    PARTIAL_INTERVAL_GPU_SECONDS
                    if current_device == "cuda"
                    else PARTIAL_INTERVAL_CPU_SECONDS
                )

                for stream_name, state in list(stream_states.items()):
                    pcm = state["full_utterance_pcm"]
                    pcm_len = len(pcm)
                    pcm_duration_s = pcm_len / SAMPLE_RATE

                    # 短時間発話のフォールバック (VAD未発火で音声入力停止時)
                    if (
                        len(state["audio_buffer"]) < sample_rate * MIN_AUDIO_SECONDS
                        and state["last_audio_at"] is not None
                        and now - state["last_audio_at"]
                        >= SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS
                        and len(state["audio_buffer"]) > 0
                    ):
                        final_text, final_latency_ms = await transcribe_buffer(
                            state["audio_buffer"], allow_short=True
                        )
                        if final_text:
                            await send_queue.put(
                                {
                                    "text": final_text,
                                    "is_final": True,
                                    "stream": stream_name,
                                    "latency_ms": round(final_latency_ms, 1),
                                }
                            )
                        else:
                            logger.info(
                                "VAD reset[%s]: partial=false final=false "
                                "reason=no_transcript samples=%d",
                                stream_name,
                                len(state["audio_buffer"]),
                            )
                        reset_stream_state(state)
                        continue

                    # 1. 発話終了 (Endpointing) 判定: 実音声の無音時間または最大発話長に基づく
                    should_finalize = False
                    finalize_reason = ""

                    if state["is_speaking"]:
                        if (
                            state["last_voice_at"] is not None
                            and (now - state["last_voice_at"]) >= SPEECH_END_SILENCE_SECONDS
                        ):
                            should_finalize = True
                            finalize_reason = "silence_timeout"
                        elif pcm_duration_s >= MAX_UTTERANCE_SECONDS or state["force_endpoint"]:
                            should_finalize = True
                            finalize_reason = "max_duration"

                    if should_finalize:
                        if pcm_len >= int(SAMPLE_RATE * MIN_AUDIO_SECONDS):
                            # 発話全体PCMを使って1回final推論を実行
                            final_text, final_latency_ms = await transcribe_buffer(
                                pcm, allow_short=True
                            )
                            total_time_ms = (
                                (time.monotonic() - state["utterance_started_monotonic"]) * 1000.0
                                if state["utterance_started_monotonic"]
                                else final_latency_ms
                            )
                            avg_partial_lat = (
                                sum(state["partial_latencies"]) / len(state["partial_latencies"])
                                if state["partial_latencies"]
                                else 0.0
                            )
                            max_partial_lat = (
                                max(state["partial_latencies"])
                                if state["partial_latencies"]
                                else 0.0
                            )
                            rtf = (final_latency_ms / 1000.0) / max(0.001, pcm_duration_s)

                            diagnostic_summary = {
                                "event": "asr_utterance_summary",
                                "stream": stream_name,
                                "reason": finalize_reason,
                                "audio_duration_s": round(pcm_duration_s, 2),
                                "speech_duration_s": round(
                                    max(0.0, (state["last_voice_at"] or now) - (state["speech_start_time"] or now)), 2
                                ),
                                "partial_count": state["partial_count"],
                                "avg_partial_latency_ms": round(avg_partial_lat, 1),
                                "max_partial_latency_ms": round(max_partial_lat, 1),
                                "final_latency_ms": round(final_latency_ms, 1),
                                "total_latency_ms": round(total_time_ms, 1),
                                "rtf": round(rtf, 3),
                                "device": current_device,
                                "model": current_model_name,
                                "compute_type": current_compute_type,
                                "text": final_text,
                            }
                            logger.info(
                                f"ASR Utterance Finalized: {json.dumps(diagnostic_summary, ensure_ascii=False)}"
                            )

                            output_text = final_text or state["last_partial_text"]
                            if output_text:
                                await send_queue.put(
                                    {
                                        "text": output_text,
                                        "is_final": True,
                                        "stream": stream_name,
                                        "latency_ms": round(final_latency_ms, 1),
                                    }
                                )
                            else:
                                logger.info(
                                    "VAD reset[%s]: partial=false final=false "
                                    "reason=no_transcript samples=%d",
                                    stream_name,
                                    pcm_len,
                                )
                        reset_stream_state(state)
                        continue

                    # 2. Partial 推論: 発話中かつ十分な新規PCMが到着しインターバルを満たしている場合のみ実行
                    if not state["is_speaking"]:
                        continue

                    if (now - state["last_partial_time"]) < partial_interval:
                        continue

                    added_samples = pcm_len - state["last_partial_pcm_len"]
                    if added_samples < int(SAMPLE_RATE * MIN_PARTIAL_INCREMENT_SECONDS):
                        continue

                    if pcm_len < int(SAMPLE_RATE * MIN_AUDIO_SECONDS):
                        continue

                    # Bounded partial window (最大10秒で推論負荷を制限)
                    partial_pcm = pcm
                    if len(pcm) > int(SAMPLE_RATE * 10.0):
                        partial_pcm = pcm[-int(SAMPLE_RATE * 10.0):]

                    current_text, latency_ms = await transcribe_buffer(partial_pcm)
                    state["last_partial_time"] = now
                    state["last_partial_pcm_len"] = pcm_len
                    state["partial_count"] += 1
                    state["partial_latencies"].append(latency_ms)

                    if current_text:
                        stable_text = remember_transcript(
                            state["last_partial_text"], current_text
                        )
                        if stable_text != state["last_partial_text"]:
                            await send_queue.put(
                                {
                                    "text": stable_text,
                                    "is_final": False,
                                    "stream": stream_name,
                                    "latency_ms": round(latency_ms, 1),
                                }
                            )
                            state["last_partial_text"] = stable_text

            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Error in inference loop: {e}", exc_info=True)
                await asyncio.sleep(0.3)

    inf_task = asyncio.create_task(inference_loop())

    try:
        async for message in websocket:
            if isinstance(message, bytes):
                state = stream_states.setdefault(active_stream, new_stream_state())
                now = loop.time()
                samples = np.frombuffer(message, dtype=np.float32)
                if len(samples) == 0:
                    continue
                if len(samples) > 0 and state["audio_started_at"] is None:
                    state["audio_started_at"] = loop.time()
                if len(samples) > 0:
                    state["last_audio_at"] = loop.time()

                update_vad_and_buffers(state, samples, now, sample_rate)

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
                        # 明示的フラッシュ時: 発話PCMが存在すればfinal推論を実行
                        stream_name = data.get("stream", active_stream)
                        if not isinstance(stream_name, str) or not stream_name.strip():
                            stream_name = active_stream
                        else:
                            stream_name = stream_name.strip()
                        state = stream_states.get(stream_name)
                        if state and (
                            state["last_partial_text"] or len(state["audio_buffer"]) > 0
                        ):
                            final_text = state["last_partial_text"]
                            final_latency_ms = 0.0
                            if not final_text and len(state["audio_buffer"]) > 0:
                                final_text, final_latency_ms = await transcribe_buffer(
                                    state["audio_buffer"], allow_short=True
                                )
                            elif len(state["audio_buffer"]) > 0:
                                full_pcm = (
                                    state["full_utterance_pcm"]
                                    if len(state["full_utterance_pcm"]) > 0
                                    else state["audio_buffer"]
                                )
                                res_text, res_lat = await transcribe_buffer(
                                    full_pcm, allow_short=True
                                )
                                if res_text:
                                    final_text = res_text
                                    final_latency_ms = res_lat

                            if final_text:
                                await send_queue.put(
                                    {
                                        "text": final_text,
                                        "is_final": True,
                                        "stream": stream_name,
                                        "latency_ms": round(final_latency_ms, 1),
                                    }
                                )
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

                        def _do_embed(texts=texts):
                            emb_model = get_embedding_model()
                            if emb_model is not None:
                                return emb_model.encode(
                                    texts, show_progress_bar=False
                                ).tolist()
                            # Do not manufacture zero vectors when the
                            # tokenizer/model is unavailable. An empty
                            # response lets the native client preserve the
                            # raw/summary text while treating the vector as
                            # optional and retryable.
                            return []

                        vectors = await loop.run_in_executor(None, _do_embed)
                        await send_queue.put(
                            {
                                "type": "embed_res",
                                "id": req_id,
                                "vectors": vectors,
                            }
                        )
                    elif cmd == "preallocate_vram":
                        enable = data.get("enable", True)
                        success = set_vram_preallocation(enable)
                        await send_queue.put(
                            {
                                "type": "preallocate_res",
                                "success": success,
                                "enabled": enable,
                            }
                        )
                except Exception as err:
                    logger.error(f"Error handling json message: {err}")
    except websockets.exceptions.ConnectionClosed:
        logger.debug("WebSocket connection closed by client")
    finally:
        sender_task.cancel()
        inf_task.cancel()


def _fault_injection_hang(seconds: float) -> None:
    """診断用の故障注入: ネイティブ推論のハングを再現する
    (--simulate-hang / SIMULATE_ASR_HANG で有効化)。
    """
    time.sleep(seconds)


def kill_port_owner(port):
    if sys.platform == "win32":
        try:
            import subprocess

            # shell=True の文字列補間はコマンド注入シンクになるため、引数はリスト
            # で渡しポートフィルタリングは Python 側で行う。
            out = subprocess.check_output(["netstat", "-ano", "-p", "tcp"], text=True)
            my_pid = os.getpid()
            for line in out.splitlines():
                parts = line.split()
                if len(parts) >= 5 and parts[1].endswith(f":{port}"):
                    pid = int(parts[-1])
                    if pid != my_pid and pid > 0:
                        logger.info(
                            f"Terminating lingering process (PID {pid}) on port {port}..."
                        )
                        subprocess.run(
                            ["taskkill", "/F", "/T", "/PID", str(pid)],
                            capture_output=True,
                        )
        except Exception as port_cleanup_err:
            logger.debug(f"Port cleanup skipped: {port_cleanup_err}")


async def main():
    for attempt in range(5):
        try:
            async with websockets.serve(asr_handler, "127.0.0.1", PORT):
                logger.info(
                    f"ASR WebSocket Server running at ws://127.0.0.1:{PORT}/asr"
                )
                await asyncio.Future()
            break
        except OSError as e:
            if attempt < 4:
                logger.warning(
                    f"Port {PORT} in use, terminating lingering process and retrying in 1s (attempt {attempt+1}/5)..."
                )
                kill_port_owner(PORT)
                await asyncio.sleep(1)
            else:
                logger.error(f"Failed to bind to port {PORT}: {e}")
                raise


if __name__ == "__main__":
    asyncio.run(main())

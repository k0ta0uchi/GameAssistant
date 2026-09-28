# -*- coding: utf-8 -*-
"""
GameAssistant - CPU Whisper ASR Realtime Tester (TUI)
Terminal User Interface for real-time microphone capture and CPU Whisper testing.
Features:
- Real-time microphone capture & VU meter
- CPU Faster-Whisper streaming inference (Partial & Final)
- Background streaming MP3 recording of the session
- Timestamped transcript logging & JSON metadata
- History review commands (--view-logs, --view-session, --open-logs)
- Hotkeys: [Q] Quit, [C] Clear, [↑/↓] Sens, [T] Threads, [H] Anti-Halluc, [O] Open Folder
"""

import argparse
import datetime
import json
import math
import msvcrt
import os
import queue
import sys
import threading
import time
from typing import Dict, List, Optional

import numpy as np
import sounddevice as sd
import soundfile as sf
from faster_whisper import WhisperModel
from rich import box
from rich.align import Align
from rich.console import Console
from rich.layout import Layout
from rich.live import Live
from rich.panel import Panel
from rich.table import Table
from rich.text import Text

# UTF-8 出力エンコード設定 (Windows cmd/PowerShell 文字化け防止)
try:
    if sys.stdout.encoding and sys.stdout.encoding.lower() != "utf-8":
        sys.stdout.reconfigure(encoding="utf-8")
    if sys.stderr.encoding and sys.stderr.encoding.lower() != "utf-8":
        sys.stderr.reconfigure(encoding="utf-8")
except Exception:
    pass

# 基本ディレクトリ設定
BASE_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SMALL_MODEL_PATH = os.path.join(BASE_DIR, "models", "faster-whisper-small")
KOTOBA_MODEL_PATH = os.path.join(BASE_DIR, "models", "kotoba-whisper-v2.0-faster")
# 小型・高速な faster-whisper-small があればデフォルトとして優先使用
DEFAULT_MODEL_PATH = SMALL_MODEL_PATH if os.path.exists(SMALL_MODEL_PATH) else KOTOBA_MODEL_PATH
SESSIONS_LOG_DIR = os.path.join(BASE_DIR, "logs", "asr_sessions")

SAMPLE_RATE = 16000
BLOCK_SIZE = 800  # 50ms per chunk at 16kHz
THREAD_CHOICES = [1, 2, 4, 6, 8]


class AudioRecorder:
    """マイク入力音声をバックグラウンドでリアルタイム MP3 録音するクラス"""

    def __init__(self, output_path: str, sample_rate: int = SAMPLE_RATE):
        self.output_path = output_path
        self.sample_rate = sample_rate
        self.record_queue: queue.Queue = queue.Queue()
        self.is_recording = False
        self.worker_thread: Optional[threading.Thread] = None
        self.total_samples = 0
        self.file_handle: Optional[sf.SoundFile] = None
        self.lock = threading.Lock()
        self.start_time = time.time()

    def start(self):
        self.is_recording = True
        self.start_time = time.time()
        os.makedirs(os.path.dirname(self.output_path), exist_ok=True)
        try:
            self.file_handle = sf.SoundFile(
                self.output_path,
                mode="w",
                samplerate=self.sample_rate,
                channels=1,
                format="MP3",
            )
        except Exception as e:
            # MP3 形式で失敗した場合は WAV にフォールバック
            alt_path = self.output_path.replace(".mp3", ".wav")
            self.output_path = alt_path
            self.file_handle = sf.SoundFile(
                self.output_path,
                mode="w",
                samplerate=self.sample_rate,
                channels=1,
                format="WAV",
            )

        self.worker_thread = threading.Thread(target=self._write_loop, daemon=True)
        self.worker_thread.start()

    def push_chunk(self, chunk: np.ndarray):
        if self.is_recording:
            self.record_queue.put(chunk)

    def _write_loop(self):
        while self.is_recording or not self.record_queue.empty():
            try:
                chunk = self.record_queue.get(timeout=0.08)
                with self.lock:
                    if self.file_handle is not None and not self.file_handle.closed:
                        self.file_handle.write(chunk)
                        self.total_samples += len(chunk)
            except queue.Empty:
                continue
            except Exception:
                pass

    def stop(self):
        self.is_recording = False
        if self.worker_thread and self.worker_thread.is_alive():
            self.worker_thread.join(timeout=2.0)
        with self.lock:
            if self.file_handle is not None and not self.file_handle.closed:
                try:
                    self.file_handle.flush()
                    self.file_handle.close()
                except Exception:
                    pass

    def get_stats(self) -> dict:
        duration_sec = self.total_samples / self.sample_rate
        file_size = 0
        if os.path.exists(self.output_path):
            file_size = os.path.getsize(self.output_path)
        return {
            "duration_sec": duration_sec,
            "samples": self.total_samples,
            "file_size_bytes": file_size,
            "file_path": self.output_path,
            "filename": os.path.basename(self.output_path),
        }


class SessionLogger:
    """セッションの文字起こしログ（transcripts.log）とメタデータを保存・管理するクラス"""

    def __init__(
        self,
        base_dir: str,
        session_id: str,
        device_info: dict,
        model_name: str,
        threads: int,
    ):
        self.session_id = session_id
        self.session_dir = os.path.join(base_dir, f"session_{session_id}")
        os.makedirs(self.session_dir, exist_ok=True)

        self.audio_path = os.path.join(self.session_dir, "recording.mp3")
        self.log_path = os.path.join(self.session_dir, "transcripts.log")
        self.meta_path = os.path.join(self.session_dir, "session_metadata.json")

        self.start_dt = datetime.datetime.now()
        self.utterances: List[Dict] = []
        self.lock = threading.Lock()

        # transcripts.log の初期化ヘッダーを書き込み
        dev_idx = device_info.get("index", "Def")
        dev_name = device_info.get("name", "Default Microphone")
        with open(self.log_path, "w", encoding="utf-8") as f:
            f.write("=" * 80 + "\n")
            f.write("GameAssistant - CPU Whisper ASR Session Transcript Log\n")
            f.write(f"Session ID  : {self.session_id}\n")
            f.write(f"Started At  : {self.start_dt.strftime('%Y-%m-%d %H:%M:%S')}\n")
            f.write(f"Microphone  : [{dev_idx}] {dev_name} (16kHz Mono)\n")
            f.write(f"Model       : {model_name} (CPU {threads} threads, INT8)\n")
            f.write(f"Audio File  : {os.path.basename(self.audio_path)}\n")
            f.write("=" * 80 + "\n\n")

    def log_utterance(self, time_str: str, text: str, latency_ms: float, duration_sec: float):
        entry = {
            "time": time_str,
            "text": text,
            "latency_ms": latency_ms,
            "duration_sec": duration_sec,
            "logged_at": datetime.datetime.now().isoformat(),
        }
        with self.lock:
            self.utterances.append(entry)
            with open(self.log_path, "a", encoding="utf-8") as f:
                f.write(
                    f"[{time_str}] (Latency: {latency_ms:4.0f}ms | Duration: {duration_sec:4.1f}s)  {text}\n"
                )
                f.flush()

    def finalize(self, recorder_stats: dict):
        end_dt = datetime.datetime.now()
        total_time_sec = (end_dt - self.start_dt).total_seconds()
        avg_latency = (
            sum(u["latency_ms"] for u in self.utterances) / len(self.utterances)
            if self.utterances
            else 0.0
        )

        with self.lock:
            # transcripts.log に終了サマリーを追記
            with open(self.log_path, "a", encoding="utf-8") as f:
                f.write("\n" + "=" * 80 + "\n")
                f.write("Session Summary\n")
                f.write(f"Ended At         : {end_dt.strftime('%Y-%m-%d %H:%M:%S')}\n")
                f.write(f"Session Duration : {datetime.timedelta(seconds=int(total_time_sec))}\n")
                f.write(
                    f"Recorded Audio   : {recorder_stats['duration_sec']:.1f}s ({recorder_stats['file_size_bytes'] / 1024:.1f} KB)\n"
                )
                f.write(f"Total Utterances : {len(self.utterances)}\n")
                f.write(f"Average Latency  : {avg_latency:.1f} ms\n")
                f.write("=" * 80 + "\n")

            # session_metadata.json を保存
            metadata = {
                "session_id": self.session_id,
                "session_dir": os.path.abspath(self.session_dir),
                "audio_file": os.path.abspath(recorder_stats["file_path"]),
                "log_file": os.path.abspath(self.log_path),
                "started_at": self.start_dt.isoformat(),
                "ended_at": end_dt.isoformat(),
                "session_duration_sec": total_time_sec,
                "audio_duration_sec": recorder_stats["duration_sec"],
                "audio_size_bytes": recorder_stats["file_size_bytes"],
                "total_utterances": len(self.utterances),
                "average_latency_ms": avg_latency,
                "utterances": self.utterances,
            }
            with open(self.meta_path, "w", encoding="utf-8") as f:
                json.dump(metadata, f, ensure_ascii=False, indent=2)


class AudioCapture:
    """sounddevice によるマイク入力キャプチャクラス"""

    def __init__(self, device_index: Optional[int] = None, recorder: Optional[AudioRecorder] = None):
        self.device_index = device_index
        self.device_info = sd.query_devices(device_index, "input")
        self.stream: Optional[sd.InputStream] = None
        self.audio_queue: queue.Queue = queue.Queue()
        self.recorder = recorder
        self.current_rms = 0.0
        self.peak_rms = 0.0
        self.last_peak_time = time.time()
        self.is_running = False

    def audio_callback(self, indata, frames, time_info, status):
        mono_data = indata[:, 0].copy()

        # RMS 計算
        rms = float(np.sqrt(np.mean(mono_data**2)))
        self.current_rms = rms

        now = time.time()
        if rms > self.peak_rms or (now - self.last_peak_time > 1.2):
            self.peak_rms = rms
            self.last_peak_time = now
        else:
            self.peak_rms = max(rms, self.peak_rms * 0.95)

        # ASR 推論キューにプッシュ
        self.audio_queue.put(mono_data)

        # MP3 録音レコーダーにプッシュ
        if self.recorder:
            self.recorder.push_chunk(mono_data)

    def start(self):
        self.is_running = True
        self.stream = sd.InputStream(
            device=self.device_index,
            channels=1,
            samplerate=SAMPLE_RATE,
            blocksize=BLOCK_SIZE,
            dtype="float32",
            callback=self.audio_callback,
        )
        self.stream.start()

    def stop(self):
        self.is_running = False
        if self.stream:
            try:
                self.stream.stop()
                self.stream.close()
            except Exception:
                pass


class ASRController:
    """CPU Whisper推論およびVAD発話制御ワーカー"""

    def __init__(
        self,
        model_path: str,
        session_logger: SessionLogger,
        cpu_threads: int = 4,
        vad_threshold: float = 0.015,
        silence_timeout: float = 0.65,
        anti_hallucination: bool = True,
    ):
        self.model_path = model_path
        self.session_logger = session_logger
        self.cpu_threads = cpu_threads
        self.vad_threshold = vad_threshold
        self.silence_timeout = silence_timeout
        self.anti_hallucination = anti_hallucination

        self.model: Optional[WhisperModel] = None
        self.model_load_status = "Loading..."
        self.model_lock = threading.Lock()

        # 発話バッファと状態
        self.is_speaking = False
        self.speech_buffer: List[np.ndarray] = []
        self.silence_start_time: Optional[float] = None
        self.speech_start_time: Optional[float] = None

        # 推論結果共有ステート
        self.current_partial = ""
        self.last_latency_ms = 0.0
        self.is_inferring = False
        self.history: List[Dict] = []
        self.total_processed_utterances = 0

        # スレッド制御
        self.is_running = False
        self.worker_thread: Optional[threading.Thread] = None

        # 推論タスクの直列化ワーカー
        self.task_lock = threading.Lock()
        self.pending_final_task = None
        self.pending_partial_task = None
        self.task_event = threading.Event()
        self.infer_worker_thread: Optional[threading.Thread] = None

        # UI 通知メッセージ (操作フィードバック)
        self.flash_message = ""
        self.flash_expire_time = 0.0

    def set_flash_message(self, msg: str, duration: float = 2.5):
        self.flash_message = msg
        self.flash_expire_time = time.time() + duration

    def get_flash_message(self) -> str:
        if time.time() < self.flash_expire_time:
            return self.flash_message
        return ""

    def load_model(self):
        with self.model_lock:
            self.model_load_status = f"Loading CPU Whisper (threads={self.cpu_threads})..."
            t0 = time.time()
            try:
                self.model = WhisperModel(
                    self.model_path,
                    device="cpu",
                    compute_type="int8_float32",
                    cpu_threads=self.cpu_threads,
                    num_workers=1,
                )
                elapsed = time.time() - t0
                self.model_load_status = f"Ready (INT8, {elapsed:.1f}s)"
            except Exception as e:
                try:
                    self.model = WhisperModel(
                        self.model_path,
                        device="cpu",
                        compute_type="float32",
                        cpu_threads=self.cpu_threads,
                        num_workers=1,
                    )
                    elapsed = time.time() - t0
                    self.model_load_status = f"Ready (Float32, {elapsed:.1f}s)"
                except Exception as e2:
                    self.model_load_status = f"Error: {e2}"

    def set_threads(self, threads: int):
        if threads == self.cpu_threads:
            return
        self.cpu_threads = threads
        self.set_flash_message(f"⚙️ CPU Threads changed to: {threads} (Reloading model...)")
        threading.Thread(target=self.load_model, daemon=True).start()

    def switch_model(self):
        """Small モデルと Kotoba モデルを即座にトグル切り替え"""
        curr_name = os.path.basename(self.model_path).lower()
        if "small" in curr_name:
            if os.path.exists(KOTOBA_MODEL_PATH):
                self.model_path = KOTOBA_MODEL_PATH
            else:
                self.set_flash_message("⚠️ Kotoba model directory not found.")
                return
        else:
            if os.path.exists(SMALL_MODEL_PATH):
                self.model_path = SMALL_MODEL_PATH
            else:
                self.model_path = "small"

        model_name = os.path.basename(self.model_path)
        self.set_flash_message(f"🔄 Switching model to: {model_name}...")
        threading.Thread(target=self.load_model, daemon=True).start()

    def toggle_anti_hallucination(self):
        self.anti_hallucination = not self.anti_hallucination
        state_str = "ENABLED (Secure/No-Hallucination)" if self.anti_hallucination else "DISABLED (Previous-Text ON)"
        self.set_flash_message(f"🛡️ Anti-Hallucination Filters: {state_str}")

    def adjust_threshold(self, delta: float):
        old_th = self.vad_threshold
        self.vad_threshold = max(0.002, min(0.100, self.vad_threshold + delta))
        self.set_flash_message(f"🔊 VAD Sensitivity: {old_th:.3f} -> {self.vad_threshold:.3f}")

    def clear_history(self):
        self.history.clear()
        self.current_partial = ""
        self.set_flash_message("🗑️ Display history cleared (Log file preserved).")

    def _run_whisper_inference(self, audio: np.ndarray) -> str:
        with self.model_lock:
            if self.model is None:
                return ""
            model = self.model

        kwargs = {
            "language": "ja",
            "beam_size": 1,
            "vad_filter": True,
            "without_timestamps": True,
        }

        if self.anti_hallucination:
            kwargs.update(
                {
                    "condition_on_previous_text": False,
                    "no_speech_threshold": 0.6,
                    "compression_ratio_threshold": 2.4,
                    "hallucination_silence_threshold": 0.5,
                }
            )
        else:
            kwargs.update(
                {
                    "condition_on_previous_text": True,
                    "no_speech_threshold": 0.1,
                    "compression_ratio_threshold": 9.0,
                    "hallucination_silence_threshold": None,
                }
            )

        try:
            segments, _ = model.transcribe(audio, **kwargs)
            return "".join([s.text for s in segments]).strip()
        except Exception as e:
            return f"[Error: {e}]"

    def _infer_worker_loop(self):
        """推論を直列に1つずつ実行し、スレッド競合と遅延の雪だるま式蓄積を完全防止する"""
        while self.is_running:
            self.task_event.wait(timeout=0.05)
            self.task_event.clear()

            task = None
            with self.task_lock:
                if self.pending_final_task is not None:
                    task = self.pending_final_task
                    self.pending_final_task = None
                    self.pending_partial_task = None
                elif self.pending_partial_task is not None:
                    task = self.pending_partial_task
                    self.pending_partial_task = None

            if task is None:
                continue

            task_type, audio_data, duration, time_str = task
            self.is_inferring = True
            t0 = time.time()
            text = self._run_whisper_inference(audio_data)
            lat = (time.time() - t0) * 1000.0
            self.is_inferring = False

            if task_type == "partial":
                if text:
                    self.current_partial = text
                    self.last_latency_ms = lat
            elif task_type == "final":
                if text:
                    self.total_processed_utterances += 1
                    self.last_latency_ms = lat
                    self.history.append(
                        {
                            "time": time_str,
                            "text": text,
                            "latency_ms": lat,
                            "duration": duration,
                        }
                    )
                    self.session_logger.log_utterance(time_str, text, lat, duration)
                    if len(self.history) > 50:
                        self.history.pop(0)
                self.current_partial = ""

    def process_loop(self, audio_capture: AudioCapture):
        self.is_running = True
        last_partial_infer_time = 0.0

        while self.is_running:
            chunks = []
            while not audio_capture.audio_queue.empty():
                try:
                    chunks.append(audio_capture.audio_queue.get_nowait())
                except queue.Empty:
                    break

            if not chunks:
                time.sleep(0.02)
                continue

            current_rms = audio_capture.current_rms
            now = time.time()

            # 発話状態判定 (Energy VAD)
            if current_rms >= self.vad_threshold:
                if not self.is_speaking:
                    self.is_speaking = True
                    self.speech_start_time = now
                    self.speech_buffer = []
                self.silence_start_time = None
            else:
                if self.is_speaking and self.silence_start_time is None:
                    self.silence_start_time = now

            # 発話中のチャンク蓄積
            if self.is_speaking:
                self.speech_buffer.extend(chunks)
                total_samples = sum(len(c) for c in self.speech_buffer)
                audio_dur = total_samples / SAMPLE_RATE

                # 0.5s 以上の音声があり、直近推論から1.0s以上経過かつ現在推論中でなければ Partial 要求
                if (
                    audio_dur >= 0.50
                    and not self.is_inferring
                    and (now - last_partial_infer_time) >= 1.0
                ):
                    last_partial_infer_time = now
                    max_samples = int(SAMPLE_RATE * 3.0)
                    concat_audio = np.concatenate(self.speech_buffer)
                    if len(concat_audio) > max_samples:
                        concat_audio = concat_audio[-max_samples:]

                    with self.task_lock:
                        if self.pending_final_task is None:
                            self.pending_partial_task = ("partial", concat_audio, audio_dur, "")
                            self.task_event.set()

                # 無音が一定時間継続した場合、発話確定 (Finalize)
                if (
                    self.silence_start_time is not None
                    and (now - self.silence_start_time) >= self.silence_timeout
                ):
                    concat_audio = (
                        np.concatenate(self.speech_buffer)
                        if self.speech_buffer
                        else np.array([], dtype=np.float32)
                    )
                    utterance_duration = (
                        (now - self.speech_start_time) if self.speech_start_time else 0.0
                    )

                    self.is_speaking = False
                    self.speech_buffer = []
                    self.silence_start_time = None
                    self.speech_start_time = None

                    if len(concat_audio) >= int(SAMPLE_RATE * 0.25):
                        now_str = datetime.datetime.now().strftime("%H:%M:%S")
                        with self.task_lock:
                            # 確定を最優先にセット（未実行のpartialは破棄）
                            self.pending_partial_task = None
                            self.pending_final_task = ("final", concat_audio, utterance_duration, now_str)
                            self.task_event.set()
                    else:
                        self.current_partial = ""

    def start(self, audio_capture: AudioCapture):
        self.is_running = True
        self.infer_worker_thread = threading.Thread(
            target=self._infer_worker_loop, daemon=True
        )
        self.infer_worker_thread.start()
        self.worker_thread = threading.Thread(
            target=self.process_loop, args=(audio_capture,), daemon=True
        )
        self.worker_thread.start()

    def stop(self):
        self.is_running = False
        self.task_event.set()
        if self.infer_worker_thread and self.infer_worker_thread.is_alive():
            self.infer_worker_thread.join(timeout=1.5)
        if self.worker_thread and self.worker_thread.is_alive():
            self.worker_thread.join(timeout=1.5)


def rms_to_db(rms: float) -> float:
    """RMS値を dBFS (-100 ~ 0 dB) に変換"""
    if rms <= 1e-5:
        return -100.0
    db = 20.0 * math.log10(rms)
    return max(-100.0, min(0.0, db))


def render_vu_bar(db: float, threshold_rms: float, width: int = 40) -> Text:
    """Rich 用のリアルタイム VU メーターバーを生成"""
    min_db = -60.0
    max_db = 0.0
    clamped_db = max(min_db, min(max_db, db))
    ratio = (clamped_db - min_db) / (max_db - min_db)
    filled_len = int(ratio * width)

    th_db = rms_to_db(threshold_rms)
    th_clamped = max(min_db, min(max_db, th_db))
    th_idx = int(((th_clamped - min_db) / (max_db - min_db)) * width)
    th_idx = max(0, min(width - 1, th_idx))

    text = Text()
    text.append("[", style="dim white")
    for i in range(width):
        is_filled = i < filled_len
        is_threshold = i == th_idx
        char = "█" if is_filled else "░"

        norm_pos = i / width
        if norm_pos < 0.60:
            color = "green" if is_filled else "dim green"
        elif norm_pos < 0.85:
            color = "yellow" if is_filled else "dim yellow"
        else:
            color = "bold red" if is_filled else "dim red"

        if is_threshold:
            text.append("▼" if not is_filled else "┃", style="bold cyan")
        else:
            text.append(char, style=color)

    text.append("] ", style="dim white")
    text.append(f"{db:5.1f} dB ", style="bold white")
    return text


def build_tui_layout(
    audio_cap: AudioCapture,
    asr: ASRController,
    recorder: AudioRecorder,
    session_logger: SessionLogger,
    console_width: int,
) -> Layout:
    """TUI の全画面レイアウトを構築"""
    layout = Layout()
    layout.split_column(
        Layout(name="header", size=4),
        Layout(name="audio_status", size=5),
        Layout(name="live_view", size=4),
        Layout(name="history", ratio=1),
        Layout(name="footer", size=4),
    )

    rec_stats = recorder.get_stats()
    dur_str = str(datetime.timedelta(seconds=int(rec_stats["duration_sec"])))
    size_kb = rec_stats["file_size_bytes"] / 1024.0

    # 1. Header (モデル・マイク情報 ＆ MP3録音中インジケータ)
    dev_name = audio_cap.device_info.get("name", "Default Mic")
    dev_idx = audio_cap.device_index if audio_cap.device_index is not None else "Default"

    header_text = Text()
    header_text.append("🎙️  GameAssistant - CPU Whisper ASR Realtime Tester  ", style="bold bright_cyan")
    header_text.append("🔴 REC ", style="bold bright_red blink")
    header_text.append(f"[{rec_stats['filename']} {dur_str} / {size_kb:.0f} KB]\n", style="bold white")

    header_text.append("Model: ", style="bold white")
    header_text.append(f"{os.path.basename(asr.model_path)} ", style="cyan")
    header_text.append(f"[{asr.model_load_status}]  |  ", style="dim green" if "Ready" in asr.model_load_status else "yellow")
    header_text.append("Mic: ", style="bold white")
    header_text.append(f"[{dev_idx}] {dev_name}  |  ", style="bright_blue")
    header_text.append("Folder: ", style="bold white")
    header_text.append(f"session_{session_logger.session_id}", style="dim yellow")

    layout["header"].update(Panel(header_text, box=box.ROUNDED, style="cyan"))

    # 2. Audio & VAD Status Panel
    cur_db = rms_to_db(audio_cap.current_rms)
    vu_bar = render_vu_bar(cur_db, asr.vad_threshold, width=min(45, max(20, console_width - 45)))

    audio_text = Text()
    audio_text.append("Level: ")
    audio_text.append_text(vu_bar)
    audio_text.append(f" RMS: {audio_cap.current_rms:.4f} (Peak: {audio_cap.peak_rms:.4f})\n", style="dim white")

    audio_text.append("VAD Status: ")
    if asr.is_speaking:
        buf_samples = sum(len(c) for c in asr.speech_buffer)
        buf_sec = buf_samples / SAMPLE_RATE
        audio_text.append("● SPEAKING DETECTED ", style="bold bright_green")
        audio_text.append(f"(Buffer: {buf_sec:.2f}s / {buf_samples:,} samples)", style="green")
    else:
        audio_text.append("○ LISTENING / IDLE   ", style="dim cyan")
        audio_text.append("(Waiting for speech above threshold)", style="dim white")

    audio_text.append("  |  Threshold: ")
    audio_text.append(f"{asr.vad_threshold:.3f} ", style="bold bright_yellow")
    audio_text.append("[▲/▼ keys]", style="dim yellow")

    layout["audio_status"].update(
        Panel(audio_text, title="[bold]Audio Input & Voice Activity Detection[/bold]", box=box.ROUNDED, style="bright_blue")
    )

    # 3. Live Recognition (Partial)
    live_text = Text()
    if asr.is_inferring:
        live_text.append("⚡ [INFERRING...] ", style="bold bright_magenta")
    elif asr.is_speaking:
        live_text.append("🎙️ [LISTENING...] ", style="bold bright_green")
    else:
        live_text.append("💤 [READY]        ", style="dim white")

    if asr.current_partial:
        live_text.append(f"> {asr.current_partial}", style="bold bright_yellow")
    else:
        live_text.append("(Waiting for speech...)", style="dim white")

    live_text.append(f"\nLast Latency: {asr.last_latency_ms:.0f} ms", style="dim cyan")
    if asr.last_latency_ms > 0:
        if asr.last_latency_ms < 300:
            live_text.append(" (Ultra Fast ⚡)", style="green")
        elif asr.last_latency_ms < 800:
            live_text.append(" (Normal 速度)", style="yellow")
        else:
            live_text.append(" (Slow 負荷高)", style="red")

    layout["live_view"].update(
        Panel(live_text, title="[bold]Live Streaming Transcript (Partial)[/bold]", box=box.ROUNDED, style="yellow")
    )

    # 4. Finalized History Table
    table = Table(box=box.SIMPLE_HEAVY, expand=True, show_edge=False)
    table.add_column("Time", style="cyan", width=10, no_wrap=True)
    table.add_column("Latency", style="green", width=12, no_wrap=True)
    table.add_column("Utterance", style="blue", width=10, no_wrap=True)
    table.add_column("Recognized Text", style="bold white", ratio=1)

    if not asr.history:
        table.add_row("--:--:--", "-- ms", "-- s", Text("(No finalized transcript yet. Speak into the microphone!)", style="dim italic white"))
    else:
        for item in asr.history[-8:]:
            table.add_row(
                item["time"],
                f"{item['latency_ms']:.0f} ms",
                f"{item['duration']:.1f} s",
                item["text"],
            )

    history_title = f"[bold]Finalized Transcripts History ({len(asr.history)} session total) - Auto logged to transcripts.log[/bold]"
    layout["history"].update(Panel(table, title=history_title, box=box.ROUNDED, style="white"))

    # 5. Footer (Hotkeys & Notifications)
    flash_msg = asr.get_flash_message()
    footer_text = Text()
    footer_text.append("Controls: ", style="bold white")
    footer_text.append("[Q]", style="bold bright_red")
    footer_text.append(" Quit  ", style="dim white")
    footer_text.append("[C]", style="bold bright_yellow")
    footer_text.append(" Clear  ", style="dim white")
    footer_text.append("[↑/↓]", style="bold bright_cyan")
    footer_text.append(" Sens  ", style="dim white")
    footer_text.append("[T]", style="bold bright_magenta")
    footer_text.append(f" Threads({asr.cpu_threads})  ", style="dim white")
    curr_model_tag = "Small" if "small" in os.path.basename(asr.model_path).lower() else "Kotoba"
    footer_text.append("[M]", style="bold bright_cyan")
    footer_text.append(f" Model({curr_model_tag})  ", style="dim white")
    footer_text.append("[H]", style="bold green" if asr.anti_hallucination else "bold red")
    footer_text.append(f" Anti-Halluc: {'ON' if asr.anti_hallucination else 'OFF'}  ", style="dim white")
    footer_text.append("[O]", style="bold bright_blue")
    footer_text.append(" Open Folder", style="dim white")

    if flash_msg:
        footer_text.append(f"\n🔔 {flash_msg}", style="bold bright_yellow")
    else:
        footer_text.append(f"\n📂 Logs & MP3 are recorded to: {session_logger.session_dir}", style="dim cyan")

    layout["footer"].update(Panel(footer_text, box=box.ROUNDED, style="dim white"))

    return layout


def list_audio_devices():
    """利用可能な入力オーディオデバイス一覧を表示"""
    console = Console()
    devices = sd.query_devices()
    default_in = sd.default.device[0]

    table = Table(title="Available Audio Input Devices", box=box.ROUNDED)
    table.add_column("Index", style="cyan", width=6)
    table.add_column("Device Name", style="bold white")
    table.add_column("Channels", style="green", width=10)
    table.add_column("Default", style="yellow", width=10)

    for idx, dev in enumerate(devices):
        if dev["max_input_channels"] > 0:
            is_def = "★ DEFAULT" if idx == default_in else ""
            table.add_row(
                str(idx),
                dev["name"],
                str(dev["max_input_channels"]),
                is_def,
            )
    console.print(table)


def view_saved_sessions(base_dir: str = SESSIONS_LOG_DIR):
    """保存された全セッション一覧を Rich Table で表示"""
    console = Console()
    if not os.path.exists(base_dir):
        console.print("[dim yellow]No recorded sessions found yet.[/dim yellow]")
        return

    sessions = []
    for entry in sorted(os.listdir(base_dir), reverse=True):
        entry_path = os.path.join(base_dir, entry)
        if os.path.isdir(entry_path):
            meta_path = os.path.join(entry_path, "session_metadata.json")
            mp3_path = os.path.join(entry_path, "recording.mp3")
            wav_path = os.path.join(entry_path, "recording.wav")
            audio_path = mp3_path if os.path.exists(mp3_path) else wav_path

            meta = {}
            if os.path.exists(meta_path):
                try:
                    with open(meta_path, "r", encoding="utf-8") as f:
                        meta = json.load(f)
                except Exception:
                    pass

            audio_size = os.path.getsize(audio_path) if os.path.exists(audio_path) else 0
            audio_dur = meta.get("audio_duration_sec", 0.0)
            sessions.append(
                {
                    "dir_name": entry,
                    "path": entry_path,
                    "start": meta.get("started_at", entry.replace("session_", "")),
                    "duration": f"{audio_dur:.1f}s",
                    "utterances": str(meta.get("total_utterances", "0")),
                    "avg_lat": f"{meta.get('average_latency_ms', 0.0):.0f} ms" if meta.get("average_latency_ms") else "--",
                    "audio_size": f"{audio_size / 1024:.1f} KB" if audio_size else "--",
                    "format": "MP3" if os.path.exists(mp3_path) else ("WAV" if os.path.exists(wav_path) else "None"),
                }
            )

    if not sessions:
        console.print("[dim yellow]No recorded sessions found yet.[/dim yellow]")
        return

    table = Table(title="📁 Recorded ASR Test Sessions", box=box.ROUNDED)
    table.add_column("No", style="cyan", width=4)
    table.add_column("Session ID", style="bold white")
    table.add_column("Date / Time", style="blue")
    table.add_column("Audio Len", style="green")
    table.add_column("File Size", style="dim green")
    table.add_column("Format", style="dim magenta")
    table.add_column("Utterances", style="bold yellow")
    table.add_column("Avg Latency", style="magenta")

    for i, s in enumerate(sessions, 1):
        dt_str = s["start"][:19].replace("T", " ") if "T" in s["start"] else s["start"]
        table.add_row(
            str(i),
            s["dir_name"],
            dt_str,
            s["duration"],
            s["audio_size"],
            s["format"],
            s["utterances"],
            s["avg_lat"],
        )

    console.print(table)
    console.print("\n[bold]Commands to inspect:[/bold]")
    console.print("  View transcript : [bold cyan]run_asr_cpu_tui.bat --view-session <SessionID or No>[/bold cyan]")
    console.print("  Open in Explorer: [bold cyan]run_asr_cpu_tui.bat --open-logs[/bold cyan]\n")


def view_single_session_transcript(target: str, base_dir: str = SESSIONS_LOG_DIR):
    """指定されたセッションの発話ログ全文を表示"""
    console = Console()
    target_dir = None

    if os.path.exists(os.path.join(base_dir, target)):
        target_dir = os.path.join(base_dir, target)
    elif os.path.exists(os.path.join(base_dir, f"session_{target}")):
        target_dir = os.path.join(base_dir, f"session_{target}")
    else:
        try:
            idx = int(target)
            all_dirs = sorted(
                [d for d in os.listdir(base_dir) if os.path.isdir(os.path.join(base_dir, d))],
                reverse=True,
            )
            if 1 <= idx <= len(all_dirs):
                target_dir = os.path.join(base_dir, all_dirs[idx - 1])
        except (ValueError, FileNotFoundError):
            pass

    if not target_dir or not os.path.exists(target_dir):
        console.print(f"[bold red]Session not found:[/bold red] '{target}'")
        console.print("Run [bold cyan]--view-logs[/bold cyan] to see available sessions.")
        return

    log_path = os.path.join(target_dir, "transcripts.log")
    mp3_path = os.path.join(target_dir, "recording.mp3")
    wav_path = os.path.join(target_dir, "recording.wav")
    audio_path = mp3_path if os.path.exists(mp3_path) else wav_path

    if not os.path.exists(log_path):
        console.print(f"[bold red]transcripts.log not found in:[/bold red] {target_dir}")
        return

    with open(log_path, "r", encoding="utf-8") as f:
        content = f.read()

    console.print(Panel(Text(content), title=f"[bold]Session Log: {os.path.basename(target_dir)}[/bold]", box=box.ROUNDED))
    if os.path.exists(audio_path):
        size_kb = os.path.getsize(audio_path) / 1024.0
        console.print(f"🎵 Audio Recording: [bold cyan]{audio_path}[/bold cyan] ({size_kb:.1f} KB)")
    console.print(f"📂 Folder: [dim]{target_dir}[/dim]\n")


def open_logs_folder(base_dir: str = SESSIONS_LOG_DIR):
    """ログディレクトリを Windows エクスプローラーで開く"""
    os.makedirs(base_dir, exist_ok=True)
    try:
        os.startfile(base_dir)
        print(f"Opened in Explorer: {base_dir}")
    except Exception as e:
        print(f"Failed to open explorer: {e}")


def main():
    parser = argparse.ArgumentParser(description="CPU Whisper ASR Realtime Tester with MP3 Recording & Logging (TUI)")
    parser.add_argument("--model", "--model-dir", dest="model", type=str, default=DEFAULT_MODEL_PATH, help="Path to Faster-Whisper model or name (e.g. 'small', 'base')")
    parser.add_argument("--device", type=int, default=None, help="Input audio device index (default: system default)")
    parser.add_argument("--threads", type=int, default=4, help="Number of CPU threads for inference (default: 4)")
    parser.add_argument("--threshold", type=float, default=0.015, help="VAD energy threshold (default: 0.015)")
    parser.add_argument("--silence-timeout", type=float, default=0.65, help="Silence timeout in seconds for finalization (default: 0.65)")
    parser.add_argument("--test-duration", type=float, default=None, help="Automatically exit after N seconds (useful for testing)")
    parser.add_argument("--list-devices", action="store_true", help="List available audio input devices and exit")
    parser.add_argument("--view-logs", action="store_true", help="View all recorded sessions history and exit")
    parser.add_argument("--view-session", type=str, default=None, help="View transcript of a specific session and exit")
    parser.add_argument("--open-logs", action="store_true", help="Open sessions folder in Windows Explorer and exit")
    args = parser.parse_args()

    # 1. 履歴・デバイス確認モード
    if args.list_devices:
        list_audio_devices()
        return
    if args.view_logs:
        view_saved_sessions()
        return
    if args.view_session:
        view_single_session_transcript(args.view_session)
        return
    if args.open_logs:
        open_logs_folder()
        return

    console = Console()

    # モデル指定のチェック (ローカルパスまたは公式モデル名)
    is_local_path = os.path.exists(args.model)
    model_source = args.model
    if not is_local_path and args.model == DEFAULT_MODEL_PATH:
        console.print(f"[bold red]Error:[/bold red] Default model directory not found: {args.model}")
        console.print("Please verify the model path or complete first-launch setup.")
        sys.exit(1)

    # セッションID生成
    session_id = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")

    # デバイス情報取得
    try:
        dev_info = sd.query_devices(args.device, "input")
    except Exception as e:
        console.print(f"[bold red]Failed to query audio device:[/bold red] {e}")
        sys.exit(1)

    # セッションロガー初期化
    session_logger = SessionLogger(
        base_dir=SESSIONS_LOG_DIR,
        session_id=session_id,
        device_info=dev_info,
        model_name=os.path.basename(model_source),
        threads=args.threads,
    )

    # MP3 レコーダー初期化 & 録音開始
    recorder = AudioRecorder(output_path=session_logger.audio_path, sample_rate=SAMPLE_RATE)
    recorder.start()

    # オーディオキャプチャ初期化 (レコーダーと接続)
    try:
        audio_cap = AudioCapture(device_index=args.device, recorder=recorder)
    except Exception as e:
        recorder.stop()
        console.print(f"[bold red]Failed to initialize audio input device:[/bold red] {e}")
        console.print("Run with [bold cyan]--list-devices[/bold cyan] to see available devices.")
        sys.exit(1)

    # ASR コントローラー初期化
    asr = ASRController(
        model_path=model_source,
        session_logger=session_logger,
        cpu_threads=args.threads,
        vad_threshold=args.threshold,
        silence_timeout=args.silence_timeout,
        anti_hallucination=True,
    )

    # モデルロードをバックグラウンド実行
    threading.Thread(target=asr.load_model, daemon=True).start()

    # オーディオストリーム & 処理ループ開始
    audio_cap.start()
    asr.start(audio_cap)

    # TUI 描画ループ
    start_time = time.time()
    try:
        with Live(console=console, screen=True, auto_refresh=False, refresh_per_second=20) as live:
            while True:
                # テスト自動終了判定
                if args.test_duration and (time.time() - start_time) >= args.test_duration:
                    break

                # 画面サイズ取得 & レイアウト描画
                width, height = console.size
                layout = build_tui_layout(audio_cap, asr, recorder, session_logger, width)
                live.update(layout, refresh=True)

                # キー入力ハンドリング (msvcrt)
                if msvcrt.kbhit():
                    ch = msvcrt.getch()
                    if ch in (b"\x00", b"\xe0"):
                        ext = msvcrt.getch()
                        if ext == b"H":  # 上矢印
                            asr.adjust_threshold(-0.002)
                        elif ext == b"P":  # 下矢印
                            asr.adjust_threshold(+0.002)
                    else:
                        key = ch.decode("utf-8", errors="ignore").lower()
                        if key == "q":
                            break
                        elif key == "c":
                            asr.clear_history()
                        elif key == "h":
                            asr.toggle_anti_hallucination()
                        elif key == "m":
                            asr.switch_model()
                        elif key == "o":
                            # エクスプローラーでセッションフォルダを開く
                            try:
                                os.startfile(session_logger.session_dir)
                                asr.set_flash_message("📂 Opened session folder in Explorer!")
                            except Exception as err:
                                asr.set_flash_message(f"⚠️ Could not open folder: {err}")
                        elif key == "t":
                            curr_idx = (
                                THREAD_CHOICES.index(asr.cpu_threads)
                                if asr.cpu_threads in THREAD_CHOICES
                                else 2
                            )
                            next_threads = THREAD_CHOICES[(curr_idx + 1) % len(THREAD_CHOICES)]
                            asr.set_threads(next_threads)
                        elif key in ("+", "="):
                            asr.adjust_threshold(-0.002)
                        elif key in ("-", "_"):
                            asr.adjust_threshold(+0.002)

                time.sleep(0.05)
    except KeyboardInterrupt:
        pass
    finally:
        # クリーンアップと保存
        audio_cap.stop()
        asr.stop()
        recorder.stop()
        rec_stats = recorder.get_stats()
        session_logger.finalize(rec_stats)

        # 終了サマリーパネルの表示
        summary_text = Text()
        summary_text.append("✅  ASR CPU Tester Session Completed Successfully!\n\n", style="bold bright_green")
        summary_text.append("📂  Session Folder : ", style="bold white")
        summary_text.append(f"{session_logger.session_dir}\n", style="bright_cyan")
        summary_text.append("🎵  Audio Recording: ", style="bold white")
        summary_text.append(
            f"{os.path.basename(rec_stats['file_path'])} ({rec_stats['duration_sec']:.1f}s, {rec_stats['file_size_bytes']/1024:.1f} KB)\n",
            style="green",
        )
        summary_text.append("📝  Transcript Log : ", style="bold white")
        summary_text.append(
            f"transcripts.log ({len(session_logger.utterances)} utterances recorded)\n",
            style="yellow",
        )
        summary_text.append("📊  Metadata JSON  : ", style="bold white")
        summary_text.append("session_metadata.json\n\n", style="dim white")
        summary_text.append("💡  後で確認するコマンド:\n", style="bold cyan")
        summary_text.append(f"    履歴一覧: run_asr_cpu_tui.bat --view-logs\n", style="white")
        summary_text.append(f"    ログ確認: run_asr_cpu_tui.bat --view-session {session_id}\n", style="white")
        summary_text.append(f"    フォルダ: run_asr_cpu_tui.bat --open-logs\n", style="white")

        console.print(Panel(summary_text, title="[bold]Session Finished[/bold]", box=box.ROUNDED, style="green"))


if __name__ == "__main__":
    main()

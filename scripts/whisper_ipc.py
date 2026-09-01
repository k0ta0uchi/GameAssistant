# -*- coding: utf-8 -*-
"""
High-Performance CUDA INT8 Streaming Whisper IPC Worker
Communicates with Rust Tauri Core via standard I/O (JSON Lines).
"""
import sys
import json
import time
import os
import threading
import queue
import numpy as np

sys.stdout.reconfigure(line_buffering=True)
sys.stderr.reconfigure(line_buffering=True)

try:
    from faster_whisper import WhisperModel
except ImportError:
    print(json.dumps({"error": "faster-whisper not installed"}), flush=True)
    sys.exit(1)


class StreamTranscriber:
    def __init__(self, model, stream_name="mic", silence_threshold=1.1):
        self.model = model
        self.stream_name = stream_name
        self.silence_threshold = silence_threshold
        self.audio_queue = queue.Queue()
        self.audio_buffer = np.array([], dtype=np.float32)
        self.last_partial_text = ""
        self.silence_start_time = None
        self.sample_rate = 16000
        self.is_running = True

    def add_audio(self, chunk):
        self.audio_queue.put(chunk)

    def run_loop(self):
        while self.is_running:
            try:
                # データをキューから取り出してバッファに結合
                while not self.audio_queue.empty():
                    chunk = self.audio_queue.get_nowait()
                    self.audio_buffer = np.concatenate([self.audio_buffer, chunk])

                # 最低 0.5 秒分（8000サンプル）なければ待機
                if len(self.audio_buffer) < self.sample_rate * 0.5:
                    time.sleep(0.05)
                    continue

                # CTranslate2 CUDA INT8 推論 (極限爆速: 30ms)
                segments, info = self.model.transcribe(
                    self.audio_buffer,
                    language="ja",
                    beam_size=1,
                    vad_filter=True,
                    vad_parameters=dict(min_silence_duration_ms=250),
                )

                current_text = "".join([s.text for s in segments]).strip()

                if current_text:
                    if current_text != self.last_partial_text:
                        # Partial 通知
                        msg = {
                            "stream": self.stream_name,
                            "text": current_text,
                            "is_final": False,
                        }
                        print(json.dumps(msg, ensure_ascii=False), flush=True)
                        self.last_partial_text = current_text
                        self.silence_start_time = time.time()
                    else:
                        if self.silence_start_time is None:
                            self.silence_start_time = time.time()
                else:
                    if self.silence_start_time is None:
                        self.silence_start_time = time.time()

                    # 無音時のバッファトリム（3秒以上無音なら直近0.5秒のみ保持）
                    if len(self.audio_buffer) > self.sample_rate * 3:
                        self.audio_buffer = self.audio_buffer[-int(self.sample_rate * 0.5):]

                # 確定判定: テキスト変化停止からタイムアウト判定
                if self.last_partial_text and self.silence_start_time:
                    char_len = len(self.last_partial_text)
                    timeout = 0.75 if char_len <= 4 else (1.0 if char_len <= 15 else 1.3)

                    elapsed = time.time() - self.silence_start_time
                    if elapsed >= timeout:
                        msg = {
                            "stream": self.stream_name,
                            "text": self.last_partial_text,
                            "is_final": True,
                        }
                        print(json.dumps(msg, ensure_ascii=False), flush=True)

                        self.last_partial_text = ""
                        self.audio_buffer = np.array([], dtype=np.float32)
                        self.silence_start_time = None

                time.sleep(0.08)

            except Exception as e:
                err_msg = {"error": f"Inference error in {self.stream_name}: {str(e)}"}
                print(json.dumps(err_msg, ensure_ascii=False), flush=True)
                time.sleep(0.5)


def main():
    model_id = "kotoba-tech/kotoba-whisper-v2.0-faster"
    device = "cuda"
    compute_type = "int8"

    try:
        model = WhisperModel(model_id, device=device, compute_type=compute_type)
    except Exception as e:
        sys.stderr.write(f"CUDA failed: {e}, falling back to CPU\n")
        device = "cpu"
        compute_type = "int8"
        model = WhisperModel(model_id, device=device, compute_type=compute_type)

    ready_msg = {
        "status": "ready",
        "device": device,
        "compute_type": compute_type,
        "model": model_id,
    }
    print(json.dumps(ready_msg, ensure_ascii=False), flush=True)

    mic_transcriber = StreamTranscriber(model, stream_name="mic")
    discord_transcriber = StreamTranscriber(model, stream_name="discord")

    t1 = threading.Thread(target=mic_transcriber.run_loop, daemon=True)
    t2 = threading.Thread(target=discord_transcriber.run_loop, daemon=True)
    t1.start()
    t2.start()

    # stdin から JSON メッセージを受信
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            cmd = req.get("cmd")
            if cmd == "audio":
                stream = req.get("stream", "mic")
                samples = req.get("data", [])
                if samples:
                    arr = np.array(samples, dtype=np.float32)
                    if stream == "discord":
                        discord_transcriber.add_audio(arr)
                    else:
                        mic_transcriber.add_audio(arr)
            elif cmd == "ping":
                print(json.dumps({"status": "pong"}), flush=True)
            elif cmd == "exit":
                break
        except Exception as e:
            sys.stderr.write(f"IPC parse error: {e}\n")

    sys.exit(0)


if __name__ == "__main__":
    main()

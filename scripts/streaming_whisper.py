# -*- coding: utf-8 -*-
import os
import queue
import threading
import time
import numpy as np
import logging
from faster_whisper import WhisperModel

class StreamTranscriber:
    def __init__(self, model_size="kotoba-tech/kotoba-whisper-v2.0-faster", device="cuda", compute_type="int8", shared_model=None):
        """
        VRAM 1GB前後。Porcupineと併用するため高精度モデルを採用。
        shared_model を渡すことで複数ストリーム間でモデルインスタンスを共有可能。
        """
        # faster-whisperのログを抑制
        logging.getLogger("faster_whisper").setLevel(logging.WARNING)

        if shared_model is not None:
            self.model = shared_model
            logging.info("StreamTranscriber: Using shared WhisperModel instance.")
        else:
            # ローカルパスの確認
            local_path = "./models/kotoba-whisper-v2.0-faster"
            if os.path.exists(local_path) and os.listdir(local_path):
                model_size = local_path
                logging.info(f"Loading local Whisper model from: {model_size}")
            else:
                logging.info(f"Local model not found. Downloading from HF: {model_size}")

            logging.info(f"Initializing Faster-Whisper ({model_size}, {device}, {compute_type})...")
            self.model = WhisperModel(model_size, device=device, compute_type=compute_type)

        self.audio_queue = queue.Queue()
        self.is_running = False
        self.sample_rate = 16000

        # 音声バッファ
        self.audio_buffer = np.array([], dtype=np.float32)
        self.last_final_text = ""
        self.last_partial_text = ""

        # 沈黙検知用
        self.silence_start_time = None
        self.SILENCE_THRESHOLD = 1.2  # 1.2秒の沈黙で確定とみなす

        # アクティビティ＆ウォッチドッグ用
        self.last_audio_time = time.time()
        self.last_inference_time = time.time()
        self.thread = None

    def add_audio(self, audio_chunk):
        self.last_audio_time = time.time()
        self.audio_queue.put(audio_chunk)

    def start(self, callback):
        """
        callback(text, is_final) を受け取る
        """
        self.callback = callback
        self.is_running = True
        self.last_audio_time = time.time()
        self.last_inference_time = time.time()
        self.thread = threading.Thread(target=self._worker_loop, daemon=True)
        self.thread.start()

    def stop(self):
        self.is_running = False

    def is_healthy(self):
        """スレッド生存・キュー滞留・推論ハングアップのヘルスチェック"""
        if not self.is_running:
            return True
        if self.thread is None or not self.thread.is_alive():
            logging.warning("Whisper Watchdog: Worker thread is dead.")
            return False
        if self.audio_queue.qsize() > 200:
            logging.warning(f"Whisper Watchdog: Audio queue backlog too large ({self.audio_queue.qsize()}).")
            return False
        now = time.time()
        # マイク音声は届いているが 25 秒以上推論ループが周回していない場合
        if (now - self.last_audio_time) < 10.0 and (now - self.last_inference_time) > 25.0:
            logging.warning(f"Whisper Watchdog: Inference loop hung (last inference {now - self.last_inference_time:.1f}s ago).")
            return False
        return True

    def _worker_loop(self):
        while self.is_running:
            try:
                self.last_inference_time = time.time()

                # データを取得
                while not self.audio_queue.empty():
                    chunk = self.audio_queue.get_nowait()
                    self.audio_buffer = np.concatenate([self.audio_buffer, chunk])

                if len(self.audio_buffer) < self.sample_rate * 0.5:  # 最低0.5秒分
                    time.sleep(0.1)
                    continue

                # 推論
                segments, info = self.model.transcribe(
                    self.audio_buffer,
                    language="ja",
                    beam_size=1,
                    vad_filter=True,
                    vad_parameters=dict(min_silence_duration_ms=300),
                )

                current_text = "".join([s.text for s in segments]).strip()

                if current_text:
                    if current_text != self.last_partial_text:
                        # テキストが更新されたらPartial通知
                        self.callback(current_text, is_final=False)
                        self.last_partial_text = current_text
                        self.silence_start_time = time.time() # 最終更新時刻をリセット
                    else:
                        # テキストはあるが変化していない（＝話し終わりの可能性）
                        if self.silence_start_time is None:
                            self.silence_start_time = time.time()
                else:
                    # 音声はあるが認識結果がない（完全な無音）
                    if self.silence_start_time is None:
                        self.silence_start_time = time.time()

                # 確定判定: 最終更新から一定時間経過したらFinalとする
                if self.last_partial_text and self.silence_start_time:
                    if time.time() - self.silence_start_time > self.SILENCE_THRESHOLD:
                        logging.info(f"Finalize by silence: {self.last_partial_text}")
                        self.callback(self.last_partial_text, is_final=True)
                        self.last_partial_text = ""
                        self.audio_buffer = np.array([], dtype=np.float32) # バッファクリア
                        self.silence_start_time = None

                time.sleep(0.2)

            except Exception as e:
                logging.error(f"StreamTranscriber Error: {e}", exc_info=True)
                time.sleep(1)

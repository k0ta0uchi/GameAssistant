import pyaudio
import numpy as np
import wave
import os
import pvporcupine
import struct
import logging
from dotenv import load_dotenv
import threading

load_dotenv()

# --- グローバル変数と設定 ---
p = pyaudio.PyAudio()

# 録音設定
FORMAT = pyaudio.paInt16
CHANNELS = 1
CHUNK = 512 # Porcupineのフレーム長(512)に合わせるのが効率的
SAMPLE_RATE = 16000 # PorcupineとWhisperの標準レート

# --- Picovoice Porcupine 設定 ---
ACCESS_KEY = os.getenv("POR_ACCESS_KEY")
KEYWORD_FILE_PATH = 'porcupine/ねえぐり_ja_windows_v3_0_0.ppn'
MODEL_FILE_PATH = 'porcupine/porcupine_params_ja.pv'
STOP_KEYWORD_PATH = 'porcupine/ストップ_ja_windows_v3_0_0.ppn'

class AudioService:
    def __init__(self, app_logic):
        self.app = app_logic
        self.stream = None
        self.is_running = False
        
        # コールバックリスト
        self.listeners = [] # func(pcm_data: bytes)
        
        # Porcupine (ウェイクワード検知)
        self.porcupine = None
        self.stop_porcupine = None
        
        # イベント
        self.wake_word_detected_callback = None
        self.stop_word_detected_callback = None

    def add_listener(self, callback):
        """音声データを受け取るリスナーを追加"""
        self.listeners.append(callback)

    def remove_listener(self, callback):
        if callback in self.listeners:
            self.listeners.remove(callback)

    def start_stream(self, wake_word_callback=None, stop_word_callback=None):
        """マイク入力を開始し、登録されたリスナーとPorcupineにデータを流す"""
        if self.stream:
            return

        self.wake_word_detected_callback = wake_word_callback
        self.stop_word_detected_callback = stop_word_callback
        self.is_running = True

        # Porcupineの初期化
        try:
            self.porcupine = pvporcupine.create(
                access_key=ACCESS_KEY,
                keyword_paths=[KEYWORD_FILE_PATH],
                model_path=MODEL_FILE_PATH
            )
            # ストップワード用（オプション）
            if os.path.exists(STOP_KEYWORD_PATH):
                self.stop_porcupine = pvporcupine.create(
                    access_key=ACCESS_KEY,
                    keyword_paths=[STOP_KEYWORD_PATH],
                    model_path=MODEL_FILE_PATH
                )
        except Exception as e:
            logging.error(f"Porcupine Init Error: {e}")

        # PyAudioストリーム開始
        device_index = self.app.device_index
        try:
            self.stream = p.open(
                rate=SAMPLE_RATE,
                channels=CHANNELS,
                format=FORMAT,
                input=True,
                frames_per_buffer=CHUNK,
                input_device_index=device_index,
                stream_callback=self._audio_callback
            )
            self.stream.start_stream()
            logging.info("Audio stream started (Shared).")
        except Exception as e:
            logging.error(f"PyAudio Error: {e}")

    def stop_stream(self):
        self.is_running = False
        if self.stream:
            try:
                if self.stream.is_active():
                    self.stream.stop_stream()
                self.stream.close()
            except OSError as e:
                logging.warning(f"Error checking/stopping stream: {e}")
            self.stream = None
        
        if self.porcupine:
            self.porcupine.delete()
            self.porcupine = None
        if self.stop_porcupine:
            self.stop_porcupine.delete()
            self.stop_porcupine = None
            
        logging.info("Audio stream stopped.")

    def _audio_callback(self, in_data, frame_count, time_info, status):
        if not self.is_running:
            return (None, pyaudio.paComplete)

        # 1. Porcupine処理 (int16 pcm)
        if self.porcupine:
            try:
                # Porcupineは正確なフレーム長を要求する
                # CHUNK=512ならそのまま渡せる（Porcupineの標準も512）
                pcm = struct.unpack_from("h" * frame_count, in_data)
                
                # ウェイクワード
                idx = self.porcupine.process(pcm)
                if idx >= 0 and self.wake_word_detected_callback:
                    self.wake_word_detected_callback()
                
                # ストップワード
                if self.stop_porcupine:
                    idx_stop = self.stop_porcupine.process(pcm)
                    if idx_stop >= 0 and self.stop_word_detected_callback:
                        self.stop_word_detected_callback()
            except Exception:
                pass

        # 2. Whisper処理 (float32 numpy)
        # リスナーには Whisper 用の形式で渡す
        audio_float = np.frombuffer(in_data, dtype=np.int16).astype(np.float32) / 32768.0
        
        # デバッグ: リスナー数を表示（初回のみ、または一定間隔で）
        # if len(self.listeners) > 0 and frame_count % 100 == 0:
        #     print(f"[DEBUG] Sending audio to {len(self.listeners)} listeners")

        for listener in self.listeners:
            try:
                listener(audio_float)
            except Exception:
                pass
        
        # 3. レベルメーター (GUI更新)
        # メインスレッド以外からのGUI操作になるため、App側でafter等を使うか、ここで簡単な計算だけする
        # （ここではシンプルに値を渡す）
        vol = np.abs(np.frombuffer(in_data, dtype=np.int16)).mean()
        self.app.update_level_meter(vol)

        return (in_data, pyaudio.paContinue)

# ヘルパー関数
def get_audio_device_names():
    device_names = []
    for i in range(p.get_device_count()):
        device_info = p.get_device_info_by_index(i)
        if int(device_info.get('maxInputChannels', 0)) > 0:
            device_names.append(device_info.get('name'))
    return device_names

def get_device_index_from_name(device_name):
    for i in range(p.get_device_count()):
        device_info = p.get_device_info_by_index(i)
        if device_info.get('name') == device_name:
            return i
    return None
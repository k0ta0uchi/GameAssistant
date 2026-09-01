import pyaudio
import numpy as np
import wave
import os
import struct
import logging
import time
from dotenv import load_dotenv
import threading
from openwakeword.model import Model

load_dotenv()

# --- グローバル変数と設定 ---
p = pyaudio.PyAudio()

# 録音設定
FORMAT = pyaudio.paInt16
CHANNELS = 1
CHUNK = 1280  # openwakeword は 1280 (80ms@16kHz) または 512 で動作
SAMPLE_RATE = 16000  # 16kHz 標準レート

class AudioService:
    def __init__(self, app_logic):
        self.app = app_logic
        self.stream = None
        self.is_running = False
        
        # コールバックリスト
        self.listeners = []  # func(pcm_data: bytes)
        
        # ウェイクワードエンジン設定 (デフォルト: whisper_vad)
        self.engine_mode = os.getenv("WAKE_WORD_ENGINE", "whisper_vad").lower()
        self.oww_model = None
        self.last_detection_time = 0.0
        self.stream_start_time = 0.0
        self.warmup_duration = 2.0  # 起動直後の誤発火防止ウォームアップ(秒)
        self._threshold = 0.25      # デフォルト検出スコア閾値
        self.cooldown = 2.5         # 重複検知防止クールダウン(秒)

    @property
    def threshold(self):
        if hasattr(self.app, 'state') and hasattr(self.app.state, 'wake_word_threshold'):
            try:
                return float(self.app.state.wake_word_threshold.get())
            except Exception:
                pass
        return self._threshold

    @threshold.setter
    def threshold(self, val):
        self._threshold = val
        
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
        """マイク入力を開始し、登録されたリスナーと openwakeword にデータを流す"""
        if self.stream:
            return

        self.wake_word_detected_callback = wake_word_callback
        self.stop_word_detected_callback = stop_word_callback
        self.is_running = True
        self.stream_start_time = time.time()  # ウォームアップ開始時刻を記録

        # 最新のエンジン設定を反映
        if hasattr(self.app, 'state') and hasattr(self.app.state, 'wake_word_engine'):
            self.engine_mode = self.app.state.wake_word_engine.get().lower()
        else:
            self.engine_mode = os.getenv("WAKE_WORD_ENGINE", "whisper_vad").lower()
        logging.info(f"AudioService starting with Wake Word Engine: '{self.engine_mode}'")

        # openwakeword モード指定時のみモデル初期化
        if self.engine_mode == "openwakeword":
            try:
                neeguri_model_path = os.path.abspath("openwakeword/neeguri.onnx")
                if os.path.exists(neeguri_model_path):
                    logging.info(f"Loading custom openwakeword model: {neeguri_model_path}")
                    self.oww_model = Model(wakeword_models=[neeguri_model_path], inference_framework="onnx")
                else:
                    logging.warning(f"Custom model '{neeguri_model_path}' not found. Falling back to default alexa model.")
                    from openwakeword.utils import download_models
                    try:
                        self.oww_model = Model(wakeword_models=["alexa"], inference_framework="onnx")
                    except Exception:
                        logging.info("Downloading openwakeword models for first-time setup...")
                        download_models()
                        self.oww_model = Model(wakeword_models=["alexa"], inference_framework="onnx")
                logging.info(f"openwakeword engine initialized successfully (Active models: {list(self.oww_model.models.keys())}).")
            except Exception as e:
                logging.error(f"openwakeword Init Error: {e}", exc_info=True)
        else:
            self.oww_model = None
            logging.info("Using VAD + Faster-Whisper for instant zero-latency wake word detection.")

        # PyAudioストリーム開始
        device_index = self.app.state.device_index
        
        # デバイスインデックスの検証
        if device_index is not None:
            try:
                info = p.get_device_info_by_index(device_index)
                logging.info(f"Opening audio stream on device: {info.get('name')} (Index: {device_index})")
            except Exception:
                logging.warning(f"Invalid device index: {device_index}. Using default device.")
                device_index = None

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
            if device_index is not None:
                try:
                    logging.info("Retrying with default input device...")
                    self.stream = p.open(
                        rate=SAMPLE_RATE,
                        channels=CHANNELS,
                        format=FORMAT,
                        input=True,
                        frames_per_buffer=CHUNK,
                        input_device_index=None,
                        stream_callback=self._audio_callback
                    )
                    self.stream.start_stream()
                    logging.info("Audio stream started on default device (Shared).")
                except Exception as retry_e:
                    logging.error(f"PyAudio Retry Error: {retry_e}")

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
        
        self.oww_model = None
        logging.info("Audio stream stopped.")

    def _audio_callback(self, in_data, frame_count, time_info, status):
        if not self.is_running:
            return (None, pyaudio.paComplete)

        # 1. openwakeword 処理 (int16 pcm)
        if self.oww_model:
            try:
                pcm_data = np.frombuffer(in_data, dtype=np.int16)
                prediction = self.oww_model.predict(pcm_data)
                
                now = time.time()
                is_warmed_up = (now - self.stream_start_time) >= self.warmup_duration
                
                if is_warmed_up:
                    max_score = 0.0
                    top_model = ""
                    for model_name, score in prediction.items():
                        if score > max_score:
                            max_score = score
                            top_model = model_name

                        if score >= self.threshold and (now - self.last_detection_time) > self.cooldown:
                            self.last_detection_time = now
                            logging.info(f"Wake word detected by openwakeword: '{model_name}' (Score: {score:.2f})")
                            if self.wake_word_detected_callback:
                                self.wake_word_detected_callback()
                            break

                    if max_score >= 0.15:
                        logging.debug(f"[openwakeword] Peak score: {max_score:.3f} ({top_model})")
            except Exception as e:
                logging.error(f"openwakeword processing error: {e}", exc_info=True)

        # 2. Whisper処理 (float32 numpy)
        try:
            audio_float = np.frombuffer(in_data, dtype=np.int16).astype(np.float32) / 32768.0
            
            for listener in self.listeners:
                try:
                    listener(audio_float)
                except Exception as e:
                    logging.error(f"Audio listener error: {e}", exc_info=True)
        except Exception as e:
            logging.error(f"Audio conversion error: {e}", exc_info=True)
        
        # 3. レベルメーター (GUI更新)
        try:
            vol = np.abs(np.frombuffer(in_data, dtype=np.int16)).mean()
            self.app.update_level_meter(vol)
        except Exception as e:
            logging.error(f"Level meter update error: {e}", exc_info=True)

        return (in_data, pyaudio.paContinue)

# ヘルパー関数
def sanitize_device_name(raw_name: str) -> str:
    if not raw_name:
        return ""
    cleaned = raw_name.replace("\r", "").replace("\n", " ").strip()
    try:
        reencoded = cleaned.encode("latin-1").decode("cp932")
        if reencoded and len(reencoded) > 0:
            cleaned = reencoded
    except Exception:
        pass
    return cleaned.strip()

def get_audio_device_names():
    device_names = ["Default (System Default)"]
    seen = {"Default (System Default)"}
    for i in range(p.get_device_count()):
        try:
            device_info = p.get_device_info_by_index(i)
            if int(device_info.get('maxInputChannels', 0)) > 0:
                raw_name = device_info.get('name', '')
                clean_name = sanitize_device_name(raw_name)
                if clean_name and clean_name not in seen:
                    seen.add(clean_name)
                    device_names.append(clean_name)
        except Exception:
            continue
    return device_names

def get_discord_audio_device_names():
    """Discord音声キャプチャ用のデバイス選択肢（Autoモードを含む）"""
    devices = ["Auto (Discord App / System Loopback)"]
    for name in get_audio_device_names():
        if name not in devices:
            devices.append(name)
    return devices

def is_discord_running():
    """Discordプロセス (Discord.exe) が稼働中か判定"""
    try:
        import subprocess
        output = subprocess.check_output('tasklist /FI "IMAGENAME eq Discord.exe" /NH', shell=True, text=True)
        return "Discord.exe" in output
    except Exception:
        return False

def get_device_index_from_name(device_name):
    if not device_name or device_name == "Default (System Default)":
        try:
            default_info = p.get_default_input_device_info()
            return default_info.get('index', None)
        except Exception:
            return None
    for i in range(p.get_device_count()):
        try:
            device_info = p.get_device_info_by_index(i)
            raw_name = device_info.get('name', '')
            clean_name = sanitize_device_name(raw_name)
            if clean_name == device_name or raw_name == device_name:
                return i
        except Exception:
            continue
    return None

class DiscordAudioService:
    """Discord音声（アプリプロセス / ループバック / 専用デバイス）をキャプチャしてリスナーに渡すサービス"""
    def __init__(self, app_logic):
        self.app = app_logic
        self.stream = None
        self.is_running = False
        self.listeners = []

    def add_listener(self, callback):
        """音声データを受け取るリスナーを追加"""
        self.listeners.append(callback)

    def remove_listener(self, callback):
        if callback in self.listeners:
            self.listeners.remove(callback)

    def _resolve_target_device_index(self, device_name):
        """Autoモードまたは指定名から最適なデバイスインデックスを解決"""
        # 1. 明示的なデバイス名が指定されている場合
        if device_name and not device_name.startswith("Auto"):
            idx = get_device_index_from_name(device_name)
            if idx is not None:
                return idx, device_name

        # 2. Autoモード: Discordプロセスチェック
        discord_active = is_discord_running()
        if discord_active:
            logging.info("Discord process (Discord.exe) detected actively running.")
        else:
            logging.info("Discord process not detected. Continuing in standby loopback mode.")

        # 3. ループバックや仮想オーディオ候補（CABLE, Voicemeeter, ステレオミキサー等）の自動探索
        all_devices = []
        for i in range(p.get_device_count()):
            info = p.get_device_info_by_index(i)
            if int(info.get('maxInputChannels', 0)) > 0:
                all_devices.append((i, info.get('name', '')))

        # 優先キーワード (Discordでよく使われる仮想ラインやループバック)
        priority_keywords = ["CABLE Output", "Voicemeeter Out B", "Voicemeeter AUX", "Voicemeeter", "Stereo Mix", "ステレオ ミキサー", "WaveOut", "What U Hear"]
        for kw in priority_keywords:
            for idx, dname in all_devices:
                if kw.lower() in dname.lower():
                    logging.info(f"Auto-selected Discord capture candidate: {dname} (Index: {idx})")
                    return idx, dname

        # 候補がない場合は既定の入力デバイスまたは最初の利用可能デバイス
        if all_devices:
            return all_devices[0][0], all_devices[0][1]
        return None, "Default"

    def start_stream(self):
        """Discord音声キャプチャを開始"""
        if self.stream:
            return

        if not hasattr(self.app, 'state') or not hasattr(self.app.state, 'enable_discord_capture'):
            return

        if not self.app.state.enable_discord_capture.get():
            logging.info("Discord audio capture is disabled.")
            return

        device_setting = self.app.state.discord_audio_device.get()
        device_index, resolved_name = self._resolve_target_device_index(device_setting)

        self.is_running = True
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
            logging.info(f"Discord audio stream started on [{resolved_name}] (Index: {device_index})")
        except Exception as e:
            logging.error(f"Failed to start Discord audio stream: {e}", exc_info=True)
            self.is_running = False

    def stop_stream(self):
        """Discord音声キャプチャを停止"""
        self.is_running = False
        if self.stream:
            try:
                if self.stream.is_active():
                    self.stream.stop_stream()
                self.stream.close()
            except OSError as e:
                logging.warning(f"Error stopping Discord audio stream: {e}")
            self.stream = None
        logging.info("Discord audio stream stopped.")

    def _audio_callback(self, in_data, frame_count, time_info, status):
        if not self.is_running:
            return (None, pyaudio.paComplete)

        try:
            audio_float = np.frombuffer(in_data, dtype=np.int16).astype(np.float32) / 32768.0
            for listener in self.listeners:
                try:
                    listener(audio_float)
                except Exception as e:
                    logging.error(f"Discord audio listener error: {e}", exc_info=True)
        except Exception as e:
            logging.error(f"Discord audio conversion error: {e}", exc_info=True)

        return (in_data, pyaudio.paContinue)
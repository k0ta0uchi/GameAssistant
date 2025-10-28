
import pyaudio
import pvporcupine
import struct
import wave
import io
import os
import threading
from dotenv import load_dotenv

load_dotenv()

class TTSPlayer:
    """
    WAVデータを再生し、同時に「ストップ」ウェイクワードを監視して再生をキャンセルする機能を持つクラス。
    """
    def __init__(self, pyaudio_instance, device_index=None):
        """
        Args:
            pyaudio_instance: PyAudioのインスタンス。
            device_index (int, optional): 使用するオーディオデバイスのインデックス。
        """
        self.pyaudio_instance = pyaudio_instance
        self.device_index = device_index
        self.stop_event = threading.Event()
        self.porcupine = None
        self.playback_stream = None
        self.monitoring_stream = None
        self.playback_thread = None
        self.monitoring_thread = None

    def play(self, wav_data):
        """
        音声データを再生し、ウェイクワードの監視を開始する。
        再生が完了するか、キャンセルされるまでブロックする。
        """
        try:
            self._initialize_porcupine()

            self.playback_thread = threading.Thread(target=self._playback_loop, args=(wav_data,))
            self.monitoring_thread = threading.Thread(target=self._monitor_stop_word)

            self.playback_thread.start()
            self.monitoring_thread.start()

            self.playback_thread.join()
            self.monitoring_thread.join()

        finally:
            self._cleanup()

    def _initialize_porcupine(self):
        """Porcupineを初期化する。"""
        try:
            ACCESS_KEY = os.getenv("POR_ACCESS_KEY")
            STOP_KEYWORD_FILE_PATH = 'porcupine/ストップ_ja_windows_v3_0_0.ppn'
            MODEL_FILE_PATH = 'porcupine/porcupine_params_ja.pv'
            
            if not all([ACCESS_KEY, os.path.exists(STOP_KEYWORD_FILE_PATH), os.path.exists(MODEL_FILE_PATH)]):
                raise RuntimeError("Porcupineの初期化に必要なキーまたはファイルが見つかりません。")

            self.porcupine = pvporcupine.create(
                access_key=ACCESS_KEY,
                keyword_paths=[STOP_KEYWORD_FILE_PATH],
                model_path=MODEL_FILE_PATH
            )
        except Exception as e:
            print(f"Porcupineの初期化に失敗しました: {e}")
            self.porcupine = None

    def _playback_loop(self, wav_data):
        """音声データを再生するループ。"""
        if not wav_data:
            return

        try:
            wf = wave.open(io.BytesIO(wav_data), 'rb')
            self.playback_stream = self.pyaudio_instance.open(
                format=self.pyaudio_instance.get_format_from_width(wf.getsampwidth()),
                channels=wf.getnchannels(),
                rate=wf.getframerate(),
                output=True,
                output_device_index=self.device_index
            )

            chunk = 1024
            data = wf.readframes(chunk)
            while data and not self.stop_event.is_set():
                self.playback_stream.write(data)
                data = wf.readframes(chunk)
        except Exception as e:
            print(f"音声の再生中にエラーが発生しました: {e}")
        finally:
            self.stop_event.set()

    def _monitor_stop_word(self):
        """「ストップ」ウェイクワードを監視するループ。"""
        if not self.porcupine:
            return

        try:
            self.monitoring_stream = self.pyaudio_instance.open(
                rate=self.porcupine.sample_rate,
                channels=1,
                format=pyaudio.paInt16,
                input=True,
                frames_per_buffer=self.porcupine.frame_length,
                input_device_index=self.device_index
            )

            while not self.stop_event.is_set():
                try:
                    pcm = self.monitoring_stream.read(self.porcupine.frame_length, exception_on_overflow=False)
                    pcm = struct.unpack_from("h" * self.porcupine.frame_length, pcm)
                    
                    if self.porcupine.process(pcm) >= 0:
                        print("「ストップ」を検出しました。再生を停止します。")
                        self.stop_event.set()
                        
                except IOError as e:
                    if e.errno == pyaudio.paInputOverflowed:
                        pass
                    else:
                        raise
        except Exception as e:
            print(f"ウェイクワードの監視中にエラーが発生しました: {e}")
        finally:
            self.stop_event.set()

    def _cleanup(self):
        """リソースを解放する。"""
        if self.playback_stream:
            self.playback_stream.stop_stream()
            self.playback_stream.close()
        if self.monitoring_stream:
            self.monitoring_stream.stop_stream()
            self.monitoring_stream.close()
        if self.porcupine:
            self.porcupine.delete()

import pyaudio
import numpy as np
import wave
import os
import pvporcupine
import struct
from dotenv import load_dotenv
from scripts.voice import play_wav_data

load_dotenv()

# --- グローバル変数と設定 ---
TEMP_WAV_FILE = "temp_recording.wav"
p = pyaudio.PyAudio()

# 録音設定
FORMAT = pyaudio.paInt16
CHANNELS = 1
CHUNK = 1024
SILENCE_THRESHOLD = 50 
SILENCE_DURATION = 2

# --- Picovoice Porcupine 設定 ---
ACCESS_KEY = os.getenv("POR_ACCESS_KEY")
KEYWORD_FILE_PATH = 'porcupine/ねえぐり_ja_windows_v3_0_0.ppn'
# 手動で配置した日本語モデルファイルを指定
MODEL_FILE_PATH = 'porcupine/porcupine_params_ja.pv'

# --- 音声処理関数 ---
def get_audio_level(audio_data, channels):
    """ 音声データの音量レベルを取得（マルチチャンネル対応） """
    samples = np.frombuffer(audio_data, dtype=np.int16)
    if channels > 1:
        samples = samples.reshape(-1, channels)
        channel_levels = np.abs(samples).mean(axis=0)
        return np.max(channel_levels)
    else:
        return np.abs(samples).mean()

# --- 録音機能 ---
def record_audio(device_index, update_callback, audio_file_path=TEMP_WAV_FILE, stop_event=None):
    """ 通常の録音を行う """
    stream = None
    try:
        device_info = p.get_device_info_by_index(device_index)
        channels = int(device_info.get('maxInputChannels', 1))
        rate = int(device_info.get('defaultSampleRate', 44100))
        
        print("--- 録音デバイス情報 (通常録音) ---")
        print(f"マイク: {device_info['name']} (ID: {device_index}, Channels: {channels}, Rate: {rate})")
        
        stream = p.open(format=FORMAT, channels=channels, rate=rate, input=True,
                        frames_per_buffer=CHUNK, input_device_index=device_index)
        
        print("録音開始！話してください…")
    except Exception as e:
        print(f"!!! PyAudioストリーム開始エラー: {e}")
        return None

    frames = []
    silent_time = 0
    
    while not (stop_event and stop_event.is_set()):
        try:
            data = stream.read(CHUNK, exception_on_overflow=False)
            frames.append(data)
            volume = get_audio_level(data, channels)
            update_callback(volume)

            if volume < SILENCE_THRESHOLD:
                silent_time += CHUNK / rate
            else:
                silent_time = 0

            if silent_time > SILENCE_DURATION:
                print("\n無音が続いたため録音を終了します。")
                break
        except IOError:
            pass # Overflowエラーは無視
        except Exception as e:
            print(f"録音中に予期せぬエラーが発生しました: {e}")
            break

    print("録音ストリームを停止します。")
    if stream:
        stream.stop_stream()
        stream.close()

    if not frames:
        print("録音データがありません。")
        return None

    with wave.open(audio_file_path, 'wb') as wf:
        wf.setnchannels(channels)
        wf.setsampwidth(p.get_sample_size(FORMAT))
        wf.setframerate(rate)
        wf.writeframes(b"".join(frames))

    print(f"録音ファイルが {audio_file_path} に保存されました。")
    return audio_file_path

def wait_for_keyword(device_index, update_callback, audio_file_path=TEMP_WAV_FILE, stop_event=None):
    """ Porcupineを使用してキーワード待機録音を行う """
    porcupine = None
    stream = None
    try:
        porcupine = pvporcupine.create(
            access_key=ACCESS_KEY,
            keyword_paths=[KEYWORD_FILE_PATH],
            model_path=MODEL_FILE_PATH # モデルファイルを明示的に指定
        )

        device_info = p.get_device_info_by_index(device_index)
        channels = 1 # Porcupineはモノラルを要求
        rate = porcupine.sample_rate

        print("--- 録音デバイス情報 (キーワード待機) ---")
        print(f"マイク: {device_info['name']} (ID: {device_index}, Channels: {channels}, Rate: {rate})")

        stream = p.open(
            rate=rate,
            channels=channels,
            format=pyaudio.paInt16,
            input=True,
            frames_per_buffer=porcupine.frame_length,
            input_device_index=device_index
        )

        print(f"録音待機中！「ねえぐり」と発言してください...")
        
        while not (stop_event and stop_event.is_set()):
            pcm = stream.read(porcupine.frame_length, exception_on_overflow=False)
            pcm = struct.unpack_from("h" * porcupine.frame_length, pcm)

            keyword_index = porcupine.process(pcm)
            if keyword_index >= 0:
                print("\nキーワード検出！")
                # wav/nod/1.wavを再生
                try:
                    with open("wav/nod/1.wav", "rb") as f:
                        wav_data = f.read()
                    play_wav_data(wav_data)
                except Exception as e:
                    print(f"音声ファイルの再生エラー: {e}")
                
                print("録音を開始します...")
                break
        
        if stop_event and stop_event.is_set():
            print("キーワード待機がキャンセルされました。")
            return None

    except Exception as e:
        print(f"!!! Porcupine初期化または待機中にエラー: {e}")
        return None
    finally:
        if porcupine:
            porcupine.delete()

    # --- キーワード検出後の録音処理 ---
    print("話してください…")
    frames = []
    silent_time = 0
    
    # 最初のチャンクを保存
    if 'pcm' in locals():
        frames.append(struct.pack("h" * len(pcm), *pcm))

    while not (stop_event and stop_event.is_set()):
        try:
            data = stream.read(CHUNK, exception_on_overflow=False)
            frames.append(data)
            volume = get_audio_level(data, channels)
            update_callback(volume)

            if volume < SILENCE_THRESHOLD:
                silent_time += CHUNK / rate
            else:
                silent_time = 0

            if silent_time > SILENCE_DURATION:
                print("\n無音が続いたため録音を終了します。")
                break
        except IOError:
            pass # Overflowエラーは無視
        except Exception as e:
            print(f"録音中に予期せぬエラーが発生しました: {e}")
            break

    print("録音ストリームを停止します。")
    if stream:
        stream.stop_stream()
        stream.close()

    if not frames:
        print("録音データがありません。")
        return None

    with wave.open(audio_file_path, 'wb') as wf:
        wf.setnchannels(channels)
        wf.setsampwidth(p.get_sample_size(FORMAT))
        wf.setframerate(rate)
        wf.writeframes(b"".join(frames))

    print(f"録音ファイルが {audio_file_path} に保存されました。")
    return audio_file_path


# --- デバイス関連 ---
def get_audio_device_names(kind='input'):
    """ 指定された種類（input/output）のオーディオデバイス名リストを取得する """
    device_names = []
    for i in range(p.get_device_count()):
        device_info = p.get_device_info_by_index(i)
        if kind == 'input' and int(device_info.get('maxInputChannels', 0)) > 0:
            device_names.append(device_info.get('name'))
        elif kind == 'output' and int(device_info.get('maxOutputChannels', 0)) > 0:
            device_names.append(device_info.get('name'))
    return device_names

def get_device_index_from_name(device_name):
    """ デバイス名からPyAudioのデバイスインデックスを取得する """
    for i in range(p.get_device_count()):
        device_info = p.get_device_info_by_index(i)
        if device_info.get('name') == device_name:
            return i
    return None
def stream_audio(device_index, update_callback, stop_event=None):
    """ 音声ストリームをチャンクごとに生成するジェネレータ """
    stream = None
    try:
        device_info = p.get_device_info_by_index(device_index)
        channels = int(device_info.get('maxInputChannels', 1))
        rate = int(device_info.get('defaultSampleRate', 44100))
        
        print("--- 録音デバイス情報 (ストリーミング) ---")
        print(f"マイク: {device_info['name']} (ID: {device_index}, Channels: {channels}, Rate: {rate})")
        
        stream = p.open(format=FORMAT, channels=channels, rate=rate, input=True,
                        frames_per_buffer=CHUNK, input_device_index=device_index)
        
        print("ストリーミング録音開始！話してください…")
        
        silent_time = 0
        
        while not (stop_event and stop_event.is_set()):
            try:
                data = stream.read(CHUNK, exception_on_overflow=False)
                yield data
                
                volume = get_audio_level(data, channels)
                update_callback(volume)

                if volume < SILENCE_THRESHOLD:
                    silent_time += CHUNK / rate
                else:
                    silent_time = 0

                if silent_time > SILENCE_DURATION:
                    print("\n無音が続いたためストリーミングを終了します。")
                    break
            except IOError:
                pass # Overflowエラーは無視
            except Exception as e:
                print(f"ストリーミング中に予期せぬエラーが発生しました: {e}")
                break

    finally:
        if stream:
            print("録音ストリームを停止します。")
            stream.stop_stream()
            stream.close()
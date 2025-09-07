import pyaudio
import numpy as np
import wave
import speech_recognition as sr
import os

# --- グローバル変数と設定 ---
TEMP_WAV_FILE = "temp_recording.wav"
p = pyaudio.PyAudio()

# 録音設定
FORMAT = pyaudio.paInt16
CHUNK = 1024
SILENCE_THRESHOLD = 50 
SILENCE_DURATION = 2
KEYWORD_HIRAGANA = "ぐり"
KEYWORD_KATAKANA = "グリ"

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

# --- 音声認識 ---
def recognize_speech_from_file(audio_path):
    """ WAVファイルから音声を認識する """
    r = sr.Recognizer()
    with sr.AudioFile(audio_path) as source:
        audio = r.record(source)
    try:
        recognize_method = getattr(r, 'recognize_google')
        return recognize_method(audio, language='ja-JP')
    except (sr.UnknownValueError, sr.RequestError, AttributeError):
        return ""

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

def record_audio_with_keyword(device_index, update_callback, audio_file_path=TEMP_WAV_FILE, stop_event=None):
    """ キーワード待機録音を行う """
    stream = None
    try:
        device_info = p.get_device_info_by_index(device_index)
        channels = int(device_info.get('maxInputChannels', 1))
        rate = int(device_info.get('defaultSampleRate', 44100))

        print("--- 録音デバイス情報 (キーワード待機) ---")
        print(f"マイク: {device_info['name']} (ID: {device_index}, Channels: {channels}, Rate: {rate})")

        stream = p.open(format=FORMAT, channels=channels, rate=rate, input=True,
                        frames_per_buffer=CHUNK, input_device_index=device_index)
        
        print(f"録音待機中！'{KEYWORD_HIRAGANA}' または '{KEYWORD_KATAKANA}' と発言してください...")
    except Exception as e:
        print(f"!!! PyAudioストリーム開始エラー: {e}")
        return None

    frames = []
    silent_time = 0
    keyword_detected = False

    while not (stop_event and stop_event.is_set()):
        try:
            data = stream.read(CHUNK, exception_on_overflow=False)
            
            if not keyword_detected:
                temp_keyword_file = "temp_keyword.wav"
                with wave.open(temp_keyword_file, 'wb') as wf:
                    wf.setnchannels(channels)
                    wf.setsampwidth(p.get_sample_size(FORMAT))
                    wf.setframerate(rate)
                    wf.writeframes(data)
                
                text = recognize_speech_from_file(temp_keyword_file)
                if text:
                    print(f"認識されたテキスト: {text}", end='\r')
                if KEYWORD_HIRAGANA in text or KEYWORD_KATAKANA in text:
                    print(f"\nキーワード検出！録音を続行します...")
                    keyword_detected = True
                    frames.append(data)
            else:
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
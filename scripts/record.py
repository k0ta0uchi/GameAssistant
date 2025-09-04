import pyaudio
import numpy as np
import wave
import speech_recognition as sr
import os
import sounddevice as sd

# --- グローバル変数と設定 ---
TEMP_WAV_FILE = "temp_recording.wav"
p = pyaudio.PyAudio()

# 録音設定
FORMAT = pyaudio.paInt16
CHANNELS = 1
RATE = 44100
CHUNK = 1024
SILENCE_THRESHOLD = 1500
SILENCE_DURATION = 2
KEYWORD_HIRAGANA = "ぐり"
KEYWORD_KATAKANA = "グリ"

# --- 音声処理関数 ---

def get_audio_level(audio_data):
    """ 音声データの音量レベルを取得 """
    samples = np.frombuffer(audio_data, dtype=np.int16)
    return np.abs(samples).mean()

def perform_echo_cancellation(mic_data, loopback_data, attenuation=1.2):
    """
    スペクトル減算を用いてエコーキャンセリングを行う。
    :param mic_data: マイクからの音声データ (bytes)
    :param loopback_data: ループバック音声データ (bytes)
    :param attenuation: ノイズ除去の強度を調整する係数
    :return: エコー除去後の音声データ (bytes)
    """
    # bytesからnumpy arrayに変換
    mic_samples = np.frombuffer(mic_data, dtype=np.int16).astype(np.float32)
    loopback_samples = np.frombuffer(loopback_data, dtype=np.int16).astype(np.float32)

    # FFT (高速フーリエ変換) を実行
    mic_fft = np.fft.rfft(mic_samples)
    loopback_fft = np.fft.rfft(loopback_samples)

    # パワースペクトルを計算
    mic_power = np.abs(mic_fft)**2
    loopback_power = np.abs(loopback_fft)**2

    # スペクトル減算
    # マイクのパワースペクトルからループバックのパワースペクトルを引く
    subtracted_power = mic_power - loopback_power * attenuation
    # 結果が負にならないようにクリッピング
    subtracted_power = np.maximum(subtracted_power, 0)

    # 位相情報を保持
    mic_phase = np.angle(mic_fft)

    # 新しいスペクトルを計算
    new_fft = np.sqrt(subtracted_power) * np.exp(1j * mic_phase)

    # IFFT (逆高速フーリエ変換) で音声波形に戻す
    cleaned_samples = np.fft.irfft(new_fft)

    # int16に戻してbytes形式で返す
    return cleaned_samples.astype(np.int16).tobytes()


# --- 音声認識 ---

def recognize_speech_from_file(audio_path):
    """ WAVファイルから音声を認識する """
    r = sr.Recognizer()
    with sr.AudioFile(audio_path) as source:
        audio = r.record(source)
    try:
        # getattrを使用して動的にメソッドを取得し、Pylanceのエラーを回避
        recognize_method = getattr(r, 'recognize_google')
        return recognize_method(audio, language='ja-JP')
    except (sr.UnknownValueError, sr.RequestError, AttributeError):
        return ""

# --- 録音機能 ---

def record_audio_with_echo_cancellation(mic_device_index, loopback_device_index, update_callback, audio_file_path=TEMP_WAV_FILE, stop_event=None):
    """
    マイクとループバックデバイスから同時に録音し、エコーキャンセリングを行う。
    キーワード検出機能も統合。
    loopback_device_indexがNoneの場合は、通常のマイク録音を行う。
    """
    mic_stream = None
    loopback_stream = None
    
    try:
        mic_info = p.get_device_info_by_index(mic_device_index)
        rate = int(mic_info['defaultSampleRate'])
        mic_channels = int(mic_info['maxInputChannels'])
        
        print("--- 録音デバイス情報 ---")
        print(f"マイク: {mic_info['name']} (ID: {mic_device_index}, Channels: {mic_channels}, Rate: {rate})")

        mic_stream = p.open(format=FORMAT,
                            channels=mic_channels,
                            rate=rate,
                            input=True,
                            frames_per_buffer=CHUNK,
                            input_device_index=mic_device_index)

        if loopback_device_index is not None:
            loopback_info = p.get_device_info_by_index(loopback_device_index)
            loopback_channels = int(loopback_info['maxInputChannels'])
            print(f"ループバック: {loopback_info['name']} (ID: {loopback_device_index}, Channels: {loopback_channels}, Rate: {rate})")
            loopback_stream = p.open(format=FORMAT,
                                     channels=loopback_channels,
                                     rate=rate,
                                     input=True,
                                     frames_per_buffer=CHUNK,
                                     input_device_index=loopback_device_index)
        else:
            print("ループバックデバイスが指定されていないため、エコーキャンセリングは無効です。")
        
        print("--------------------------")
    except Exception as e:
        print(f"!!! PyAudioストリーム開始エラー: {e}")
        if mic_stream: mic_stream.close()
        if loopback_stream: loopback_stream.close()
        return None

    print(f"録音待機中！'{KEYWORD_HIRAGANA}' または '{KEYWORD_KATAKANA}' と発言してください...")
    
    frames = []
    silent_time = 0
    keyword_detected = False

    while not (stop_event and stop_event.is_set()):
        try:
            mic_data = mic_stream.read(CHUNK, exception_on_overflow=False)
            
            if loopback_stream:
                loopback_data = loopback_stream.read(CHUNK, exception_on_overflow=False)
                # チャンネル数が異なる場合はモノラルに変換
                if mic_channels != CHANNELS:
                    mic_data = convert_to_mono(mic_data, mic_channels)
                if loopback_channels != CHANNELS:
                    loopback_data = convert_to_mono(loopback_data, loopback_channels)
                # エコーキャンセリング実行
                cleaned_data = perform_echo_cancellation(mic_data, loopback_data)
            else:
                # ループバックがない場合はマイクのデータをそのまま使用
                cleaned_data = mic_data

            if not keyword_detected:
                # キーワード検出のために一時ファイルに書き込み
                temp_keyword_file = "temp_keyword.wav"
                with wave.open(temp_keyword_file, 'wb') as wf:
                    wf.setnchannels(CHANNELS)
                    wf.setsampwidth(p.get_sample_size(FORMAT))
                    wf.setframerate(rate)
                    wf.writeframes(cleaned_data)
                
                text = recognize_speech_from_file(temp_keyword_file)
                if text:
                    print(f"認識されたテキスト: {text}", end='\r')
                if KEYWORD_HIRAGANA in text or KEYWORD_KATAKANA in text:
                    print(f"\nキーワード検出！録音を続行します...")
                    keyword_detected = True
                    frames.append(cleaned_data)
            else:
                frames.append(cleaned_data)
                volume = get_audio_level(cleaned_data)
                update_callback(volume)

                if volume < SILENCE_THRESHOLD:
                    silent_time += CHUNK / rate
                else:
                    silent_time = 0

                if silent_time > SILENCE_DURATION:
                    print("\n無音が続いたため録音を終了します。")
                    break
        except IOError as e:
            # Input overflowedなどのエラーを無視
            # print(f"ストリーム読み込みエラー: {e}")
            pass
        except Exception as e:
            print(f"録音中に予期せぬエラーが発生しました: {e}")
            break

    print("録音ストリームを停止します。")
    if mic_stream:
        mic_stream.stop_stream()
        mic_stream.close()
    if loopback_stream:
        loopback_stream.stop_stream()
        loopback_stream.close()

    if not frames:
        print("録音データがありません。")
        return None

    # 録音データをWAVファイルに保存
    with wave.open(audio_file_path, 'wb') as wf:
        wf.setnchannels(CHANNELS)
        wf.setsampwidth(p.get_sample_size(FORMAT))
        wf.setframerate(rate)
        wf.writeframes(b"".join(frames))

    print(f"録音ファイルが {audio_file_path} に保存されました。")
    return audio_file_path

def convert_to_mono(audio_data, channels):
    """ マルチチャンネル音声をモノラルに変換 """
    samples = np.frombuffer(audio_data, dtype=np.int16)
    # 全チャンネルを平均化してモノラルにする
    mono_samples = samples.reshape(-1, channels).mean(axis=1)
    return mono_samples.astype(np.int16).tobytes()

# --- デバイス関連 ---

def get_audio_device_info():
    """ sounddeviceを使ってオーディオデバイスの情報を取得・表示 """
    print("利用可能なオーディオデバイス:")
    print(sd.query_devices())
    return sd.query_devices()

def get_audio_device_names(kind='input'):
    """
    指定された種類（input/output）のオーディオデバイス名リストを取得する。
    PyAudioを使用。
    """
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

# --- 以前の関数（互換性のため、あるいは削除予定） ---
# record_audio と record_audio_with_keyword は record_audio_with_echo_cancellation に統合されたため、
# 必要に応じて呼び出し側を修正するか、ここでラップ関数を定義します。
# 今回は、呼び出し側（main.pyなど）で新しい関数を直接使うことを想定し、古い関数はコメントアウトまたは削除します。

# def record_audio(...):
#     # ... (古い実装)

# def record_audio_with_keyword(...):
#     # ... (古い実装)

# def list_audio_devices(...):
#     # sounddevice版に置き換え
import pyaudio
import numpy as np
import wave
import speech_recognition as sr  # 音声認識ライブラリ
import os

# ファイル名定義
TEMP_WAV_FILE = "temp_recording.wav"

# 録音設定
FORMAT = pyaudio.paInt16
CHANNELS = 1
RATE = 44100
CHUNK = 1024
SILENCE_THRESHOLD = 50  # 無音と判断する音量の閾値
SILENCE_DURATION = 2  # 何秒間静かなら終了するか（秒）
KEYWORD_HIRAGANA = "ぐり"  # 録音開始のトリガーとなるキーワード（ひらがな）
KEYWORD_KATAKANA = "グリ"  # 録音開始のトリガーとなるキーワード（カタカナ）

def get_audio_level(audio_data):
    """ 音声データの音量レベルを取得 """
    samples = np.frombuffer(audio_data, dtype=np.int16)
    return np.abs(samples).mean()

def recognize_speech(device_index=0):
    """ 音声データをテキストに変換する """
    r = sr.Recognizer()

    with sr.Microphone(device_index=device_index) as source:
        try:
            audio = r.listen(source, timeout=1, phrase_time_limit=3)
        except sr.WaitTimeoutError:
            return ""

    try:
        # getattrを使用して動的にメソッドを取得し、Pylanceのエラーを回避
        recognize_method = getattr(r, 'recognize_google')
        return recognize_method(audio, language='ja-JP')  # 日本語で認識
    except (sr.UnknownValueError, AttributeError):
        # print(f"不明のエラーまたはメソッドが存在しない")
        return ""
    except sr.RequestError as e:
        print(f"音声認識APIへのリクエストエラー: {e}")
        return ""

def record_audio(device_index, update_callback, audio_file_path=TEMP_WAV_FILE):
    """ 指定したデバイスから音声を録音し、無音が続いたら終了する（通常の録音）"""
    p = pyaudio.PyAudio()
    stream = p.open(format=FORMAT, channels=CHANNELS, rate=RATE, input=True, frames_per_buffer=CHUNK, input_device_index=device_index)
    print(f"選択されたデバイス: ID {device_index}")
    
    print("録音開始！話してください…")
    frames = []
    silent_time = 0

    while True:
        data = stream.read(CHUNK, exception_on_overflow=False)
        frames.append(data)
        volume = get_audio_level(data)

        # コールバック関数を呼び出してGUIのレベルメーターを更新
        update_callback(volume)

        if volume < SILENCE_THRESHOLD:
            silent_time += CHUNK / RATE
        else:
            silent_time = 0

        if silent_time > SILENCE_DURATION:
            print("\n静かになったので録音終了")
            break

    stream.stop_stream()
    stream.close()
    p.terminate()

    # 録音データをWAVファイルに保存
    with wave.open(audio_file_path, 'wb') as wf:
        wf.setnchannels(CHANNELS)
        wf.setsampwidth(p.get_sample_size(FORMAT))
        wf.setframerate(RATE)
        wf.writeframes(b"".join(frames))

    print(f"録音ファイルが{audio_file_path}に保存されました")

    return TEMP_WAV_FILE

def record_audio_with_keyword(device_index, update_callback, audio_file_path=TEMP_WAV_FILE, stop_event=None):
    """ 指定したデバイスから音声を録音し、キーワード検出後に無音が続いたら終了する """
    p = pyaudio.PyAudio()
    stream = p.open(format=FORMAT, channels=CHANNELS, rate=RATE, input=True, frames_per_buffer=CHUNK, input_device_index=device_index)
    print(f"選択されたデバイス: ID {device_index}")
    
    print(f"録音待機中！'{KEYWORD_HIRAGANA}' または '{KEYWORD_KATAKANA}' と発言してください...")
    frames = []
    silent_time = 0
    keyword_detected = False

    while not (stop_event and stop_event.is_set()):  # stop_eventがセットされるまでループ
        data = stream.read(CHUNK, exception_on_overflow=False)
        # キーワード検出前は音声認識を行う
        if not keyword_detected:
            text = recognize_speech(device_index)
            if text != "":
                print(f"認識されたテキスト: {text}", end='\r')
            if KEYWORD_HIRAGANA in text or KEYWORD_KATAKANA in text:
                print(f"\nキーワード '{KEYWORD_HIRAGANA}' または '{KEYWORD_KATAKANA}' を検出！録音を続行します...")
                keyword_detected = True
                # キーワード検出後の最初のCHUNKもframesに追加
                frames.append(data)  
        else:
            # キーワード検出後は無音状態を監視
            frames.append(data)
            volume = get_audio_level(data)
            # コールバック関数を呼び出してGUIのレベルメーターを更新
            update_callback(volume)

            if volume < SILENCE_THRESHOLD:
                silent_time += CHUNK / RATE
            else:
                silent_time = 0

            if silent_time > SILENCE_DURATION:
                print("\n静かになったので録音終了")
                break

    stream.stop_stream()
    stream.close()
    p.terminate()

    # 録音データをWAVファイルに保存
    with wave.open(audio_file_path, 'wb') as wf:
        wf.setnchannels(CHANNELS)
        wf.setsampwidth(p.get_sample_size(FORMAT))
        wf.setframerate(RATE)
        wf.writeframes(b"".join(frames))

    print(f"録音ファイルが{audio_file_path}に保存されました")

    return TEMP_WAV_FILE

def list_audio_devices():
    """ マイクの一覧を取得して表示し、デバイスの詳細をファイルに保存 """
    DEVICE_DETAILS_FILE = "device_details.txt"
    p = pyaudio.PyAudio()
    info = p.get_host_api_info_by_index(0)
    num_devices = info.get('deviceCount')

    return p

def get_audio_device_names():
    """オーディオデバイスの名前のリストを取得する"""
    p = list_audio_devices()  # record.pyの関数を使用
    info = p.get_host_api_info_by_index(0)
    num_devices = info.get('deviceCount')
    device_names = []
    if isinstance(num_devices, int):
        for i in range(0, num_devices):
            device_info = p.get_device_info_by_host_api_device_index(0, i)
            try:
                if int(device_info.get('maxInputChannels', 0)) > 0:
                    device_names.append(device_info.get('name'))
            except (TypeError, AttributeError, ValueError):
                continue
    return device_names

def get_device_index_from_name(device_name):
    """デバイス名からデバイスインデックスを取得する"""
    p = list_audio_devices()
    info = p.get_host_api_info_by_index(0)
    num_devices = info.get('deviceCount')
    if isinstance(num_devices, int):
        for i in range(0, num_devices):
            device_info = p.get_device_info_by_host_api_device_index(0, i)
            try:
                if int(device_info.get('maxInputChannels', 0)) > 0 and device_info.get('name') == device_name:
                    return i
            except (TypeError, AttributeError, ValueError):
                continue
    return None  # デバイスが見つからなかった場合
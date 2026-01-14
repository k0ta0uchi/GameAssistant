import requests
import json
import wave
import threading

stop_playback_event = threading.Event()

def request_stop_playback():
    """音声再生の停止をリクエストする。"""
    stop_playback_event.set()
import io
import pyaudio
import urllib.parse
import random
from kokoro import KPipeline
import soundfile as sf
import torch
from scripts.gemini import GeminiSession

RANDOM_NOD = [
    "0.wav",
    "1.wav",
    "2.wav",
]

_gemini_session_for_tts = None

def generate_speech_data(text, speaker_id=46, core_version=None):
    """
    与えられたテキストを音声データに変換する。
    設定に応じてVOICEVOXまたはGemini TTSを使用する。
    """
    global _gemini_session_for_tts
    try:
        with open('settings.json', 'r', encoding="utf-8") as f:
            settings = json.load(f)
        tts_engine = settings.get("tts_engine", "voicevox")
    except (FileNotFoundError, json.JSONDecodeError):
        tts_engine = "voicevox"

    if tts_engine == "gemini":
        if _gemini_session_for_tts is None:
            _gemini_session_for_tts = GeminiSession()
        pcm_data = _gemini_session_for_tts.generate_speech(text)
        if pcm_data:
            # PCMデータをWAV形式に変換
            wav_data = io.BytesIO()
            with wave.open(wav_data, 'wb') as wf:
                wf.setnchannels(1)
                wf.setsampwidth(2)
                wf.setframerate(24000)
                wf.writeframes(pcm_data)
            wav_data.seek(0)
            return wav_data.read()
    else: # voicevox
        base_url = "http://localhost:50021"

        # 1. クエリ作成APIを呼び出す
        encoded_text = urllib.parse.quote(text)
        query_url = f"{base_url}/audio_query?text={encoded_text}&speaker={speaker_id}"
        if core_version:
            query_url += f"&core_version={core_version}"

        try:
            response = requests.post(query_url, timeout=3) # タイムアウトを設定
            response.raise_for_status()
            query_data = response.json()
            
            # 2. 音声合成APIを呼び出す
            synthesis_url = f"{base_url}/synthesis?speaker={speaker_id}"
            if core_version:
                synthesis_url += f"&core_version={core_version}"

            response = requests.post(synthesis_url, headers={"Content-Type": "application/json"}, data=json.dumps(query_data), timeout=10)
            response.raise_for_status()
            return response.content

        except (requests.exceptions.RequestException, json.JSONDecodeError) as e:
            print(f"VOICEVOX接続エラーのため、Gemini TTSにフォールバックします: {e}")
            # Gemini TTS で再試行
            if _gemini_session_for_tts is None:
                _gemini_session_for_tts = GeminiSession()
            pcm_data = _gemini_session_for_tts.generate_speech(text)
            if pcm_data:
                wav_data = io.BytesIO()
                with wave.open(wav_data, 'wb') as wf:
                    wf.setnchannels(1)
                    wf.setsampwidth(2)
                    wf.setframerate(24000)
                    wf.writeframes(pcm_data)
                wav_data.seek(0)
                return wav_data.read()

    return None

def text_to_speech_kokoro(text):
    """
    Kokoro TTSを用いてテキストから音声を生成し、再生する。
    """
    pipeline = KPipeline(lang_code='j')
    
    generator = pipeline(text, voice='jf_alpha')
    
    # 音声データを一時ファイルに保存し、それを読み込んでplay_wav_dataに渡す
    all_audio = []
    for i, (gs, ps, audio) in enumerate(generator):
        print(i, gs, ps)
        if audio is not None:
            all_audio.extend(audio)

    sf.write('temp_recording.wav', all_audio, 24000)

    # WAVファイルを読み込み、バイナリデータとしてplay_wav_dataに渡す
    with open('temp_recording.wav', 'rb') as f:
        wav_data = f.read()
    
    play_wav_data(wav_data)


def play_wav_data(wav_data):
    """
    WAVデータを再生する。
    再生中に stop_playback_event がセットされたら停止する。

    Args:
        wav_data (bytes): WAVデータ。
    """
    stop_playback_event.clear()  # 再生開始時に停止フラグをリセット
    try:
        wf = wave.open(io.BytesIO(wav_data), 'rb')
        p = pyaudio.PyAudio()

        stream = p.open(format=p.get_format_from_width(wf.getsampwidth()),
                        channels=wf.getnchannels(),
                        rate=wf.getframerate(),
                        output=True)

        data = wf.readframes(1024)

        while data:
            if stop_playback_event.is_set():
                print("音声再生を中断しました。")
                break
            stream.write(data)
            data = wf.readframes(1024)

        stream.stop_stream()
        stream.close()
        p.terminate()

    except wave.Error as e:
        print(f"WAVデータエラー: {e}")
    except Exception as e:
        print(f"PyAudioエラー: {e}")

def play_random_nod():
    """
    RANDOM_NODからランダムにWAVファイルを選び、再生する。
    """
    try:
        filename = random.choice(RANDOM_NOD)
        filepath = f"wav/nod/{filename}"  # RANDOM_NODディレクトリにあると仮定

        with open(filepath, 'rb') as f:
            wav_data = f.read()
            play_wav_data(wav_data)

    except FileNotFoundError:
        print(f"ファイルが見つかりません: {filepath}")
    except Exception as e:
        print(f"エラーが発生しました: {e}")

def play_wav_file(filepath):
    """
    指定されたWAVファイルを再生する。
    """
    try:
        with open(filepath, 'rb') as f:
            wav_data = f.read()
            play_wav_data(wav_data)

    except FileNotFoundError:
        print(f"ファイルが見つかりません: {filepath}")
    except Exception as e:
        print(f"エラーが発生しました: {e}")

if __name__ == '__main__':
    text = '''
        Kokoro（/kˈOkəɹO/）は、8200万のパラメータを持つオープンウェイトTTSモデルです。軽量なアーキテクチャにもかかわらず、大幅に高速かつコスト効率が良い一方で、より大規模なモデルに匹敵する品質を提供します。Apacheライセンスの重みを持つKokoro（/kˈOkəɹO/）は、本番環境から個人プロジェクトまで、あらゆる場所にデプロイできます。
        '''
    text_to_speech_kokoro(text) # デフォルト設定で実行
    #text_to_speech(text, speaker_id=2) # speaker_id を指定して実行
    #text_to_speech(text, speaker_id=1, core_version="0.14.5") # core_versionを指定して実行
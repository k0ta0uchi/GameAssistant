import requests
import json
import wave
import threading
import os
import io
import pyaudio
import urllib.parse
import random
from kokoro import KPipeline
import soundfile as sf
import torch
from scripts.gemini import GeminiSession

stop_playback_event = threading.Event()

def request_stop_playback():
    """音声再生の停止をリクエストする。"""
    stop_playback_event.set()

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
    elif tts_engine == "style_bert_vits2":
        # Style-Bert-VITS2 ブリッジサーバーを使用
        base_url = "http://localhost:50021"
        
        # 選択されたモデル(speaker)を取得（デフォルトは0）
        vits2_speaker_id = settings.get("vits2_speaker_id", 0)
        
        # 1. クエリ作成
        encoded_text = urllib.parse.quote(text)
        query_url = f"{base_url}/audio_query?text={encoded_text}&speaker={vits2_speaker_id}"
        
        try:
            # タイムアウトを少し長めに設定
            response = requests.post(query_url, timeout=10)
            response.raise_for_status()
            query_data = response.json()
            
            # 2. 音声合成
            synthesis_url = f"{base_url}/synthesis?speaker={vits2_speaker_id}"
            # 大型モデル向けにさらに長いタイムアウトを設定
            response = requests.post(synthesis_url, json=query_data, timeout=60)
            response.raise_for_status()
            return response.content
        except Exception as e:
            print(f"Style-Bert-VITS2接続エラー: {e}")
            return None
    else: # voicevox
        base_url = "http://localhost:50021"

        # 1. クエリ作成APIを呼び出す
        encoded_text = urllib.parse.quote(text)
        query_url = f"{base_url}/audio_query?text={encoded_text}&speaker={speaker_id}"
        if core_version:
            query_url += f"&core_version={core_version}"

        try:
            response = requests.post(query_url, timeout=3)
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
    all_audio = []
    for i, (gs, ps, audio) in enumerate(generator):
        if audio is not None:
            all_audio.extend(audio)

    sf.write('temp_recording.wav', all_audio, 24000)
    with open('temp_recording.wav', 'rb') as f:
        wav_data = f.read()
    play_wav_data(wav_data)


def play_wav_data(wav_data):
    """
    WAVデータを再生する。
    """
    # 再生開始時に停止フラグを強制リセット
    stop_playback_event.clear()
    try:
        wf = wave.open(io.BytesIO(wav_data), 'rb')
        p_audio = pyaudio.PyAudio()

        stream = p_audio.open(format=p_audio.get_format_from_width(wf.getsampwidth()),
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
        p_audio.terminate()

    except wave.Error as e:
        print(f"WAVデータエラー: {e}")
    except Exception as e:
        print(f"PyAudioエラー: {e}")

def play_random_nod():
    """
    wav/nod ディレクトリからランダムにWAVファイルを選び、再生する。
    """
    try:
        # パスを確実に取得
        base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        nod_dir = os.path.join(base_dir, "wav", "nod")
        
        if not os.path.exists(nod_dir):
            # fallback to current working directory
            nod_dir = os.path.join(os.getcwd(), "wav", "nod")

        if not os.path.exists(nod_dir):
            print(f"nod directory not found: {nod_dir}")
            return

        files = [f for f in os.listdir(nod_dir) if f.endswith(".wav")]
        if not files:
            print("No wav files in nod directory.")
            return

        filename = random.choice(files)
        filepath = os.path.join(nod_dir, filename)

        with open(filepath, 'rb') as f:
            wav_data = f.read()
            play_wav_data(wav_data)

    except Exception as e:
        print(f"Error in play_random_nod: {e}")

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

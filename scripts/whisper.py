import torch
from transformers import pipeline
from huggingface_hub import login
import os

# --- グローバル設定 ---
# Hugging Face Hubへのログイン（初回実行時やトークンが必要な場合に備える）
# 環境変数 HUGGING_FACE_HUB_TOKEN の設定を推奨
try:
    if os.getenv("HUGGING_FACE_HUB_TOKEN"):
        login(token=os.getenv("HUGGING_FACE_HUB_TOKEN"))
        print("Hugging Face Hubにログインしました。")
except Exception as e:
    print(f"Hugging Face Hubへのログインに失敗しました: {e}。処理を続行します。")

# モデルとパイプラインの初期化
MODEL_ID = "kotoba-tech/kotoba-whisper-v2.2"
TORCH_DTYPE = torch.float16 if torch.cuda.is_available() else torch.float32
DEVICE = "cuda:0" if torch.cuda.is_available() else "cpu"

try:
    print(f"モデル {MODEL_ID} をロードしています... ({DEVICE}, {TORCH_DTYPE})")
    PIPE = pipeline(
        "automatic-speech-recognition",
        model=MODEL_ID,
        torch_dtype=TORCH_DTYPE,
        device=DEVICE,
        trust_remote_code=True,  # kotoba-whisperではTrueが必要
    )
    print("モデルのロードが完了しました。")
except Exception as e:
    print(f"パイプラインの初期化中にエラーが発生しました: {e}")
    PIPE = None

def recognize_speech(audio_file_path: str) -> str:
    print(f"[DEBUG] whisper.py: Starting recognition for {audio_file_path}")
    """ 録音した音声をテキストに変換する """
    if PIPE is None:
        error_message = "音声認識パイプラインが初期化されていません。"
        print(error_message)
        return error_message

    import time
    import wave
    import numpy as np
    
    max_retries = 5
    for attempt in range(max_retries):
        try:
            if attempt == 0:
                time.sleep(0.1)

            print(f"音声ファイル '{audio_file_path}' の読み込みを開始します... (試行 {attempt + 1})")
            
            if not os.path.exists(audio_file_path):
                raise FileNotFoundError(f"Audio file missing: {audio_file_path}")
            
            # --- waveライブラリを使用して直接読み込む ---
            with wave.open(audio_file_path, 'rb') as wf:
                num_channels = wf.getnchannels()
                sample_width = wf.getsampwidth()
                framerate = wf.getframerate()
                num_frames = wf.getnframes()
                
                if num_frames == 0:
                    raise ValueError("Audio file contains no frames.")
                
                raw_data = wf.readframes(num_frames)
                
                # 16bit PCM を float32 (-1.0 to 1.0) に変換
                audio_np = np.frombuffer(raw_data, dtype=np.int16).astype(np.float32) / 32768.0
                
                # ステレオの場合はモノラルに変換
                if num_channels > 1:
                    audio_np = audio_np.reshape(-1, num_channels).mean(axis=1)

            print(f"音声データの読み込み成功: {len(audio_np)} samples, {framerate}Hz")

            # パイプラインに直接データを渡す
            # transformersのASRパイプラインは {"raw": array, "sampling_rate": sr} 形式を受け付ける
            result = PIPE(
                {"raw": audio_np, "sampling_rate": framerate},
                generate_kwargs={"task": "transcribe"},
                return_timestamps=True
            )
            
            if isinstance(result, dict) and "text" in result:
                text = result["text"]  # type: ignore
                print("\n認識結果:", text)
                return str(text)
            
            break

        except Exception as e:
            if attempt < max_retries - 1:
                print(f"読み込みエラー: {e}。再試行します...")
                time.sleep(0.3)
            else:
                print(f"音声認識中に最終的なエラーが発生しました: {e}")
                return "音声認識エラーが発生しました。"
    
    return "認識結果の形式が不正です。"

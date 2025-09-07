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
    """ 録音した音声をテキストに変換する """
    if PIPE is None:
        error_message = "音声認識パイプラインが初期化されていません。"
        print(error_message)
        return error_message

    try:
        print(f"音声ファイル '{audio_file_path}' の認識を開始します...")
        # 話者分離を有効にする場合は generate_kwargs を使用
        result = PIPE(audio_file_path, generate_kwargs={"task": "transcribe"})
        
        # Pylanceの型推論エラーを回避するため、isinstanceで型をチェック
        if isinstance(result, dict) and "text" in result:
            text = result["text"]  # type: ignore
            print("\n認識結果:", text)
            return str(text) # 明示的にstrに変換
        
        print(f"予期しない形式の結果が返されました: {result}")
        return "認識結果の形式が不正です。"

    except Exception as e:
        print(f"音声認識中にエラーが発生しました: {e}")
        return "音声認識エラーが発生しました。"

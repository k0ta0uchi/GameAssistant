import os
from dotenv import load_dotenv
from google import genai

# .envファイルを読み込む
load_dotenv()

# グローバル変数としてクライアントインスタンスを保持
_gemini_client = None

def get_gemini_client():
    """
    Gemini APIクライアントのシングルトンインスタンスを返す。
    """
    global _gemini_client
    if _gemini_client is None:
        google_api_key = os.environ.get("GOOGLE_API_KEY")
        if not google_api_key:
            raise ValueError("GOOGLE_API_KEY is not set in the environment.")
        _gemini_client = genai.Client(api_key=google_api_key)
    return _gemini_client
import chromadb

_chroma_client = None

def get_chroma_client(path="chromadb"):
    """
    ChromaDBクライアントのシングルトンインスタンスを返す。
    """
    global _chroma_client
    if _chroma_client is None:
        _chroma_client = chromadb.PersistentClient(path=path)
    return _chroma_client
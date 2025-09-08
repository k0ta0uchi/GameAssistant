# scripts/clients.py
import os
from dotenv import load_dotenv
from typing import Optional, Any, List, Tuple
import logging

import chromadb
from chromadb.config import Settings

from google import genai

# cryptography optional
try:
    from cryptography.fernet import Fernet, InvalidToken  # type: ignore
    _HAS_CRYPTO = True
except Exception:
    _HAS_CRYPTO = False

load_dotenv()
logger = logging.getLogger(__name__)

# chroma client は型が厄介なので Any を使って静的解析を落ち着かせる
_chroma_client: Optional[Any] = None

# グローバル変数としてクライアントインスタンスを保持
_gemini_client = None

def _get_chroma_settings() -> Settings:
    persist_dir = os.environ.get("CHROMA_PERSIST_DIR", "chromadb")
    return Settings(chroma_db_impl="duckdb+parquet", persist_directory=persist_dir)

def get_chroma_client() -> Any:
    """
    新しい Chroma の永続クライアントを使う（PersistentClient）。
    - 既存の古いデータがある場合は chroma-migrate が必要（下記参照）。
    - PersistentClient のシグネチャが環境により変わるため type ignore を付ける。
    """
    global _chroma_client
    if _chroma_client is None:
        persist_dir = os.environ.get("CHROMA_PERSIST_DIR", "chromadb")

        # 1) まず PersistentClient を使ってローカル永続化を試す（一般的で簡単）
        try:
            # ここは chromadb.PersistentClient(path=...) の形式が安定してる例が多い
            _chroma_client = chromadb.PersistentClient(path=persist_dir)  # type: ignore[call-arg]
            logger.info("Using chromadb.PersistentClient (path=%s)", persist_dir)
            return _chroma_client
        except Exception as e:
            logger.warning("PersistentClient construction failed: %s", e)

        # 2) fallback: 簡易 client（in-memory / ephemeral）
        try:
            _chroma_client = chromadb.Client()  # type: ignore[call-arg]
            logger.info("Using ephemeral chromadb.Client() as fallback")
            return _chroma_client
        except Exception as e:
            logger.exception("Failed to construct chromadb client fallback: %s", e)
            raise
    return _chroma_client

# optional encryption helpers using Fernet
def _get_fernet() -> Optional['Fernet']:
    key = os.environ.get("CHROMA_ENCRYPT_KEY")
    if not key:
        return None
    if not _HAS_CRYPTO:
        raise RuntimeError("cryptography がインストールされていません。pip install cryptography を実行してください。")
    return Fernet(key.encode() if isinstance(key, str) else key)

def _encrypt(s: str) -> str:
    f = _get_fernet()
    if not f:
        return s
    return f.encrypt(s.encode()).decode()

def _decrypt(s: str) -> str:
    f = _get_fernet()
    if not f:
        return s
    try:
        return f.decrypt(s.encode()).decode()
    except InvalidToken:
        logger.exception("Invalid encryption token while decrypting")
        raise

class TwitchTokenManager:
    """
    ChromaDB を使ったトークン管理ラッパー。
    collection の id = user_id、metadatas に access_token / refresh_token を入れる。
    """

    def __init__(self, collection_name: str = "twitch_tokens"):
        client = get_chroma_client()
        # 型は Any なので get_or_create_collection も Any として扱う
        self.collection: Any = client.get_or_create_collection(name=collection_name)

    def get_all_tokens(self) -> List[Tuple[str, str, str]]:
        """
        全トークンを取得: [(user_id, access_token, refresh_token), ...]
        """
        try:
            results = self.collection.get()
            ids = results.get("ids", []) or []
            metadatas = results.get("metadatas", []) or []
            out: List[Tuple[str, str, str]] = []
            for uid, meta in zip(ids, metadatas):
                if not meta:
                    continue
                at = meta.get("access_token")
                rt = meta.get("refresh_token")
                if at is None or rt is None:
                    continue
                out.append((uid, _decrypt(at), _decrypt(rt)))
            return out
        except Exception:
            logger.exception("Failed to get_all_tokens")
            return []

    def get_token(self, user_id: str) -> Optional[Tuple[str, str]]:
        """
        user_id のトークンを返す。見つからなければ None。
        """
        try:
            res = self.collection.get(ids=[user_id])
            metas = res.get("metadatas", [])
            if not metas:
                return None
            meta = metas[0] or {}
            at = meta.get("access_token")
            rt = meta.get("refresh_token")
            if at is None or rt is None:
                return None
            return (_decrypt(at), _decrypt(rt))
        except Exception:
            logger.exception("Failed to get_token for %s", user_id)
            return None

    def upsert_token(self, user_id: str, access_token: str, refresh_token: str) -> None:
        try:
            at = _encrypt(access_token)
            rt = _encrypt(refresh_token)
            # documents は空でも良いが識別用に軽く文字列を入れておく
            self.collection.upsert(
                ids=[user_id],
                metadatas=[{"access_token": at, "refresh_token": rt}],
                documents=[f"twitch-token:{user_id}"],
            )
        except Exception:
            logger.exception("Failed to upsert_token for %s", user_id)
            raise

    def delete_token(self, user_id: str) -> bool:
        try:
            self.collection.delete(ids=[user_id])
            return True
        except Exception:
            logger.exception("Failed to delete_token for %s", user_id)
            return False

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
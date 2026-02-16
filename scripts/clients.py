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
_api_keys = []
_current_key_index = 0

def _load_api_keys():
    global _api_keys
    if not _api_keys:
        raw_keys = os.environ.get("GOOGLE_API_KEY", "")
        # カンマ区切りで分割し、空白や不自然なクォーテーションを除去
        _api_keys = [k.strip().strip("'").strip('"') for k in raw_keys.split(",") if k.strip()]
    return _api_keys

def get_gemini_client(force_refresh=False):
    """
    Gemini APIクライアントのシングルトンインスタンスを返す。
    force_refresh=True の場合、現在のインデックスのキーでクライアントを再生成する。
    """
    global _gemini_client, _current_key_index
    keys = _load_api_keys()
    
    if not keys:
        raise ValueError("GOOGLE_API_KEY is not set in the environment.")

    if _gemini_client is None or force_refresh:
        current_key = keys[_current_key_index]
        # セキュリティのため最初と最後の4文字だけ表示
        masked_key = f"{current_key[:4]}...{current_key[-4:]}" if len(current_key) > 8 else "****"
        logger.info(f"Initializing Gemini client with key index {_current_key_index} (Key: {masked_key})")
        _gemini_client = genai.Client(api_key=current_key)
    
    return _gemini_client

def switch_to_next_api_key():
    """
    次のAPIキーに切り替える。
    すべてのキーを使い切った（一周した）場合は False を返す。
    """
    global _current_key_index, _gemini_client
    keys = _load_api_keys()
    
    if _current_key_index + 1 < len(keys):
        _current_key_index += 1
        get_gemini_client(force_refresh=True)
        return True
    
    return False

def get_chroma_client() -> Any:
    """
    新しい Chroma の永続クライアントを使う（PersistentClient）。
    テレメトリを完全に無効化してエラーを防止します。
    """
    os.environ['CHROMA_TELEMETRY_DISABLED'] = 'true'
    global _chroma_client
    if _chroma_client is None:
        persist_dir = os.environ.get("CHROMA_PERSIST_DIR", "chromadb")

        # テレメトリを無効化した設定
        settings = Settings(
            anonymized_telemetry=False,
            telemetry_enabled=False,
            is_persistent=True
        )

        try:
            # PersistentClient に settings を明示的に渡す
            _chroma_client = chromadb.PersistentClient(path=persist_dir, settings=settings)
            logger.info("Using chromadb.PersistentClient (path=%s) with telemetry disabled", persist_dir)
            return _chroma_client
        except Exception as e:
            logger.warning("PersistentClient construction failed: %s", e)

        # 2) fallback: 簡易 client（in-memory / ephemeral）
        try:
            settings = Settings(anonymized_telemetry=False, telemetry_enabled=False, is_persistent=False)
            _chroma_client = chromadb.Client(settings=settings)
            logger.info("Using ephemeral chromadb.Client() as fallback with telemetry disabled")
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
        self.collection: Any = client.get_or_create_collection(
            name=collection_name,
            metadata={
                "hnsw:space": "l2",
                "hnsw:M": 16,
                "hnsw:construction_ef": 256,
                "hnsw:ef": 256
            }
        )

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

def get_gemini_client(force_refresh=False):
    """
    Gemini APIクライアントのシングルトンインスタンスを返す。
    force_refresh=True の場合、現在のインデックスのキーでクライアントを再生成する。
    """
    global _gemini_client, _current_key_index
    keys = _load_api_keys()
    
    if not keys:
        raise ValueError("GOOGLE_API_KEY is not set in the environment.")

    if _gemini_client is None or force_refresh:
        current_key = keys[_current_key_index]
        # セキュリティのため最初と最後の4文字だけ表示
        masked_key = f"{current_key[:4]}...{current_key[-4:]}" if len(current_key) > 8 else "****"
        logger.info(f"Initializing Gemini client with key index {_current_key_index} (Key: {masked_key})")
        _gemini_client = genai.Client(api_key=current_key)
    
    return _gemini_client
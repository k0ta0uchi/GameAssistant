import asyncio
import time
from typing import Optional, Dict, Any

import aiohttp
import chromadb

# --- 定数 ---
# 重要: このリダイレクトURIは、Twitch開発者コンソールに登録されているものと
#       完全に一致している必要があります。
#       auth.htmlをホスティングするURL（例: GitHub PagesのURL）に変更してください。
REDIRECT_URI = "https://k0ta0uchi.github.io/GameAssistant/auth.html"
SCOPES = "chat:read chat:edit moderator:read:followers user:read:chat user:write:chat user:bot channel:bot"

class DummyEmbeddingFunction:
    def __call__(self, input):
        return [[0.0] * 384 for _ in input]

# --- ChromaDBクライアントの初期化 ---
chroma_client = chromadb.PersistentClient(path="./chroma_tokens_data")
token_collection = chroma_client.get_or_create_collection(
    name="user_tokens",
    embedding_function=DummyEmbeddingFunction(),
    metadata={
        "hnsw:space": "l2",
        "hnsw:M": 16,
        "hnsw:construction_ef": 256,
        "hnsw:ef": 256
    }
)

# --- トークン管理 (ChromaDB) ---

async def save_token_to_db(user_id: str, token_data: Dict[str, Any]):
    """トークン情報をChromaDBに保存/更新する"""
    expires_at = time.time() + token_data.get("expires_in", 3600) - 60
    refresh_token = token_data.get("refresh_token") or ""
    
    metadata = {
        "token": token_data["access_token"],
        "refresh": refresh_token,
        "expires_at": expires_at
    }

    token_collection.upsert(
        ids=[user_id],
        metadatas=[metadata],
        documents=[f"auth_token_for_{user_id}"]
    )
    print(f"[info] ユーザーID {user_id} のトークンをDBに保存しました。")

async def get_token_from_db(user_id: str) -> Optional[Dict[str, Any]]:
    """ユーザーIDを使ってChromaDBからトークン情報を取得する"""
    result = token_collection.get(ids=[user_id])
    metadatas = result.get('metadatas')
    if metadatas and len(metadatas) > 0:
        metadata = metadatas[0]
        if metadata:
            return dict(metadata)
    return None


async def fetch_user_id_from_token(client_id: str, access_token: str) -> Optional[str]:
    """アクセストークンを使ってユーザーIDを検証・取得する"""
    url = "https://id.twitch.tv/oauth2/validate"
    headers = {"Authorization": f"OAuth {access_token}"}
    try:
        async with aiohttp.ClientSession() as session:
            async with session.get(url, headers=headers) as resp:
                if resp.status == 200:
                    data = await resp.json()
                    if data.get("client_id") == client_id:
                        return data.get("user_id")
    except aiohttp.ClientError as e:
        print(f"[error] トークン検証中にエラー: {e}")
    return None

async def refresh_token_for_user(client_id: str, client_secret: str, user_id: str) -> Optional[str]:
    """指定されたユーザーのトークンをリフレッシュする"""
    token_info = await get_token_from_db(user_id)
    if not token_info or 'refresh' not in token_info or not token_info['refresh']:
        print(f"[warning] ユーザーID {user_id} のリフレッシュトークンが見つかりません。")
        return None

    url = "https://id.twitch.tv/oauth2/token"
    params = {
        "client_id": client_id,
        "client_secret": client_secret,
        "grant_type": "refresh_token",
        "refresh_token": token_info['refresh'],
    }
    try:
        async with aiohttp.ClientSession() as session:
            async with session.post(url, data=params) as resp:
                if resp.status == 200:
                    new_token_data = await resp.json()
                    await save_token_to_db(user_id, new_token_data)
                    print(f"[info] ユーザーID {user_id} のトークンを正常にリフレッシュしました。")
                    return new_token_data['access_token']
                else:
                    print(f"[error] トークンのリフレッシュに失敗しました: {await resp.text()}")
                    return None
    except aiohttp.ClientError as e:
        print(f"[error] トークンリフレッシュ中にネットワークエラー: {e}")
        return None

# --- 認証関連ヘルパー ---

def generate_auth_url(client_id: str) -> str:
    """Twitch認証用のURLを生成する"""
    auth_url = (
        f"https://id.twitch.tv/oauth2/authorize"
        f"?client_id={client_id}"
        f"&redirect_uri={REDIRECT_URI}"
        f"&response_type=code"
        f"&scope={SCOPES}"
    )
    return auth_url

async def exchange_code_for_token(client_id: str, client_secret: str, code: str) -> Optional[Dict[str, Any]]:
    """認証コードをアクセストークンに交換し、DBに保存する"""
    token_url = "https://id.twitch.tv/oauth2/token"
    params = {
        "client_id": client_id,
        "client_secret": client_secret,
        "code": code,
        "grant_type": "authorization_code",
        "redirect_uri": REDIRECT_URI,
    }
    try:
        async with aiohttp.ClientSession() as session:
            async with session.post(token_url, data=params) as resp:
                if resp.status == 200:
                    token_data = await resp.json()
                    user_id = await fetch_user_id_from_token(client_id, token_data['access_token'])
                    if user_id:
                        await save_token_to_db(user_id, token_data)
                        return {"user_id": user_id, "token_data": token_data}
                    else:
                        print("[error] トークンからユーザーIDの取得に失敗しました。")
                        return None
                else:
                    print(f"[error] トークン交換に失敗しました: {await resp.text()}")
                    return None
    except aiohttp.ClientError as e:
        print(f"[error] トークン交換中にネットワークエラー: {e}")
        return None

async def ensure_bot_token_valid(client_id: str, client_secret: str, bot_id: str) -> bool:
    """
    指定されたBot IDのトークンが有効か確認し、無効ならリフレッシュを試みる。
    """
    if not bot_id:
        print("[warning] Bot IDが未設定のため、トークン検証をスキップします。")
        return False

    token_info = await get_token_from_db(bot_id)

    if token_info and time.time() < token_info.get("expires_at", 0):
        print("[info] ボットの既存トークンは有効です。")
        return True
    
    if token_info:
        print("[info] ボットのトークンが期限切れです。リフレッシュを試みます...")
        refreshed_token = await refresh_token_for_user(client_id, client_secret, bot_id)
        if refreshed_token:
            return True

    print("[warning] ボットの有効なトークンが見つかりません。ボットオーナーの認証コードを登録してください。")
    return False

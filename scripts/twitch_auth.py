import os
import json
import time
import webbrowser
import asyncio
from pathlib import Path
from typing import Optional, Dict, Any

from aiohttp import web
import aiohttp

# --- 定数 ---
TOKEN_FILE = Path(__file__).parent.parent / ".twitch_tokens.json"
REDIRECT_URI = "http://localhost:8081/callback"
SCOPES = "chat:read chat:edit moderator:read:followers"  # AutoBotがConduitを管理するためにスコープを追加 # Botに必要なスコープを定義

# --- トークン管理 ---

class TokenManager:
    """TwitchのOAuthトークンを管理するクラス"""

    def __init__(self, client_id: str, client_secret: str):
        self.client_id = client_id
        self.client_secret = client_secret
        self.tokens: Dict[str, Any] = self._load_tokens()

    def _load_tokens(self) -> Dict[str, Any]:
        """トークンをファイルから読み込む"""
        if not TOKEN_FILE.exists():
            return {}
        try:
            with open(TOKEN_FILE, "r") as f:
                data = json.load(f)
                # ファイルが空、または不正な形式の場合
                if not isinstance(data, dict):
                    return {}
                return data
        except (json.JSONDecodeError, IOError):
            return {}

    def _save_tokens(self) -> None:
        """現在のトークンをファイルに保存する"""
        try:
            with open(TOKEN_FILE, "w") as f:
                json.dump(self.tokens, f, indent=4)
        except IOError as e:
            print(f"[error] Failed to save tokens: {e}")

    def get_access_token(self) -> Optional[str]:
        """有効なアクセストークンを取得する"""
        return self.tokens.get("access_token")

    async def fetch_user_id(self, username: str) -> Optional[str]:
        """ユーザー名からユーザーIDを取得する"""
        access_token = self.get_access_token()
        if not access_token:
            return None
        
        url = f"https://api.twitch.tv/helix/users?login={username}"
        headers = {
            "Client-ID": self.client_id,
            "Authorization": f"Bearer {access_token}"
        }
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get(url, headers=headers) as resp:
                    if resp.status == 200:
                        data = await resp.json()
                        if data.get("data"):
                            return data["data"][0]["id"]
                    return None
        except aiohttp.ClientError:
            return None


    async def get_or_create_conduit(self) -> Optional[str]:
        """Conduitを取得、なければ作成する"""
        app_access_token = await self._get_app_access_token()
        if not app_access_token:
            return None

        headers = {
            "Client-ID": self.client_id,
            "Authorization": f"Bearer {app_access_token}" # アプリケーションアクセストークンを使用
        }

        # 1. Conduit のリストを取得
        try:
            async with aiohttp.ClientSession() as session:
                get_url = "https://api.twitch.tv/helix/eventsub/conduits"
                async with session.get(get_url, headers=headers) as resp:
                    if resp.status == 200:
                        data = await resp.json()
                        if data.get("data"):
                            return data["data"][0]["id"] # 既存のものを利用

                # 2. 既存がなければ作成
                create_url = "https://api.twitch.tv/helix/eventsub/conduits"
                body = {"shard_count": 1}
                async with session.post(create_url, headers=headers, json=body) as resp:
                    if resp.status == 200:
                        data = await resp.json()
                        if data.get("data"):
                            print(f"[info] Created new conduit with id: {data['data'][0]['id']}")
                            return data["data"][0]["id"]
                    else:
                        print(f"[error] Failed to create conduit: {await resp.text()}")
                        return None
        except aiohttp.ClientError as e:
            print(f"[error] Conduit operation failed: {e}")
            return None
        return None

    async def _get_app_access_token(self) -> Optional[str]:
        """アプリケーションアクセストークンを取得する"""
        url = f"https://id.twitch.tv/oauth2/token?client_id={self.client_id}&client_secret={self.client_secret}&grant_type=client_credentials"
        try:
            async with aiohttp.ClientSession() as session:
                async with session.post(url) as resp:
                    if resp.status == 200:
                        data = await resp.json()
                        return data.get("access_token")
                    else:
                        print(f"[error] Failed to get app access token: {await resp.text()}")
                        return None
        except aiohttp.ClientError as e:
            print(f"[error] App access token request failed: {e}")
            return None

    def is_token_valid(self) -> bool:
        """トークンが有効期限内かチェックする"""
        if not self.tokens:
            return False
        # 有効期限 (expires_at) が現在時刻より未来か
        expires_at = self.tokens.get("expires_at", 0)
        return time.time() < expires_at

    async def refresh_tokens(self) -> bool:
        """リフレッシュトークンを使ってトークンを更新する"""
        refresh_token = self.tokens.get("refresh_token")
        if not refresh_token:
            return False

        url = "https://id.twitch.tv/oauth2/token"
        params = {
            "client_id": self.client_id,
            "client_secret": self.client_secret,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }
        try:
            async with aiohttp.ClientSession() as session:
                async with session.post(url, data=params) as resp:
                    if resp.status == 200:
                        new_tokens = await resp.json()
                        self.update_tokens(new_tokens)
                        print("[info] Tokens refreshed successfully.")
                        return True
                    else:
                        print(f"[error] Failed to refresh tokens. Status: {resp.status}, Body: {await resp.text()}")
                        return False
        except aiohttp.ClientError as e:
            print(f"[error] Network error during token refresh: {e}")
            return False

    def update_tokens(self, new_token_data: Dict[str, Any]) -> None:
        """新しいトークンデータでインスタンスを更新し、保存する"""
        self.tokens["access_token"] = new_token_data["access_token"]
        self.tokens["refresh_token"] = new_token_data.get("refresh_token", self.tokens.get("refresh_token"))
        # expires_in (秒) をもとに有効期限のタイムスタンプを計算
        expires_in = new_token_data.get("expires_in", 3600)
        self.tokens["expires_at"] = time.time() + expires_in - 60  # 60秒のマージン
        self._save_tokens()


# --- 認証Webサーバー ---

async def get_new_tokens_via_server(client_id: str, client_secret: str) -> Optional[Dict[str, Any]]:
    """
    認証用のWebサーバーを起動し、ユーザー認証を経て新しいトークンを取得する
    """
    app_key = "shutdown_event"

    async def handle_login(request: web.Request) -> web.Response:
        """ユーザーをTwitchの認証ページにリダイレクトする"""
        auth_url = (
            f"https://id.twitch.tv/oauth2/authorize"
            f"?client_id={client_id}"
            f"&redirect_uri={REDIRECT_URI}"
            f"&response_type=code"
            f"&scope={SCOPES}"
        )
        raise web.HTTPFound(auth_url)

    async def handle_callback(request: web.Request) -> web.Response:
        """Twitchからのコールバックを処理し、認証コードをトークンに交換する"""
        code = request.query.get("code")
        if not code:
            return web.Response(text="Authentication failed: No code provided.", status=400)

        # 認証コードをトークンに交換
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
                        # 取得したトークンをアプリに保存
                        token_manager = TokenManager(client_id, client_secret)
                        token_manager.update_tokens(token_data)
                        
                        # サーバーシャットダウンをトリガー
                        request.app[app_key].set()
                        
                        return web.Response(text="Authentication successful! You can close this window now.", content_type="text/html")
                    else:
                        error_text = await resp.text()
                        return web.Response(text=f"Failed to get token: {error_text}", status=resp.status)
        except aiohttp.ClientError as e:
            return web.Response(text=f"Network error: {e}", status=500)

    app = web.Application()
    app.add_routes([
        web.get('/login', handle_login),
        web.get('/callback', handle_callback),
    ])
    app[app_key] = asyncio.Event()

    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, 'localhost', 8081)
    
    try:
        await site.start()
        print("\n--- Twitch Authentication Required ---")
        login_url = "http://localhost:8081/login"
        print(f"Please open this URL in your browser to log in: {login_url}")
        try:
            webbrowser.open(login_url)
        except Exception:
            print("Could not automatically open browser. Please copy the URL manually.")
        
        print("Waiting for authentication to complete...")
        await app[app_key].wait() # 認証完了まで待機
        
        # 認証後のトークンを返す
        return TokenManager(client_id, client_secret)._load_tokens()

    finally:
        await runner.cleanup()
        print("Authentication server has been shut down.")


async def ensure_valid_token(client_id: str, client_secret: str, bot_username: str) -> Optional[Dict[str, str]]:
    """
    有効なトークンとIDを保証する。
    戻り値: {"access_token": ..., "bot_id": ...} or None
    """
    if not all([client_id, client_secret, bot_username]):
        raise ValueError("client_id, client_secret, and bot_username must be provided")

    token_manager = TokenManager(client_id, client_secret)

    async def _get_ids_and_tokens() -> Optional[Dict[str, str]]:
        access_token = token_manager.get_access_token()
        user_id = await token_manager.fetch_user_id(bot_username)
        if access_token and user_id:
            return {"access_token": access_token, "bot_id": user_id}
        return None

    if token_manager.is_token_valid():
        print("[info] Existing token is valid.")
        return await _get_ids_and_tokens()
    
    if token_manager.tokens:
        print("[info] Token has expired. Attempting to refresh...")
        if await token_manager.refresh_tokens():
            return await _get_ids_and_tokens()

    print("[info] No valid token found. Starting authentication process...")
    new_tokens = await get_new_tokens_via_server(client_id, client_secret)
    if new_tokens:
        return await _get_ids_and_tokens()
    
    return None


if __name__ == '__main__':
    # テスト用: このファイル単体で実行すると認証フローが開始される
    from dotenv import load_dotenv
    load_dotenv()
    
    cid = os.getenv("TWITCH_CLIENT_ID")
    csecret = os.getenv("TWITCH_CLIENT_SECRET")
    buser = os.getenv("TWITCH_BOT_USERNAME")

    async def main():
        if not all([cid, csecret, buser]):
            print("Please set TWITCH_CLIENT_ID, TWITCH_CLIENT_SECRET, and TWITCH_BOT_USERNAME in your .env file")
            return
        
        # 型チェックをパスするために、Noneでないことを確認
        assert cid is not None
        assert csecret is not None
        assert buser is not None

        token_info = await ensure_valid_token(cid, csecret, buser)
        if token_info:
            print(f"\nSuccessfully obtained token for user {token_info['bot_id']}: {token_info['access_token'][:10]}...")
        else:
            print("\nFailed to obtain token or user ID.")

    asyncio.run(main())

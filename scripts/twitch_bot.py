import logging
import re
from typing import Optional, List, Callable, Awaitable, Any, cast
import inspect

import twitchio
from twitchio.ext import commands
from twitchio import eventsub
from twitchio.authentication import UserTokenPayload, ValidateTokenPayload
from scripts.twitch_auth import SCOPES


# ロガーの設定
LOGGER = logging.getLogger(__name__)

# mention_callbackの型定義
MentionCallback = Optional[Callable[[str, str, twitchio.PartialUser], Awaitable[Optional[str]]]]

import asyncio
import threading
import time
import chromadb
from . import twitch_auth

class DummyEmbeddingFunction:
    def __call__(self, input):
        return [[0.0] * 384 for _ in input]

async def setup_database(chroma_client: Any, bot_id: str) -> tuple[list[tuple[str, str]], Any]:
    """
    ChromaDBをセットアップします。
    """
    tokens = []
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
    results = token_collection.get()

    if results and results['ids']:
        for i, user_id_str in enumerate(results['ids']):
            # "bot" というIDはボット自身のトークンなので、ここではスキップ
            
            metadata = results['metadatas'][i]
            token = metadata.get('token')
            refresh = metadata.get('refresh')
            if token and refresh:
                tokens.append((token, refresh))
    return tokens, token_collection

class TwitchBot(commands.Bot):
    """
    TwitchIO v3 Bot (chromadb対応)
    """
    def __init__(self, *, token: str, client_id: str, client_secret: str, bot_id: str, owner_id: str, nick: str, token_collection: Any, mention_callback: MentionCallback = None, message_callback: Optional[Callable[[twitchio.ChatMessage], Awaitable[None]]] = None) -> None:
        self.token_collection = token_collection
        self.mention_callback = mention_callback
        self.message_callback = message_callback
        self.bot_id_str = bot_id
        self.nick = nick
        # self._bot_token = token # この行は不要
        super().__init__(
            client_id=client_id,
            client_secret=client_secret,
            bot_id=bot_id,
            prefix="!",
            owner_id=owner_id,
            scopes=cast(Any, SCOPES.split()),
        )

    async def setup_hook(self) -> None:
        """
        クライアントがログインし、すべてのイベント処理を開始する前に呼び出される非同期フック。
        """
        LOGGER.info("Bot is setting up...")
        # DBからトークンを読み込み、EventSubを購読
        results = self.token_collection.get()

        if results and results['ids']:
            for i, user_id_str in enumerate(results['ids']):
                # "bot" というIDはボット自身のトークンなので、ここではスキップ
                
                metadata = results['metadatas'][i]
                token = metadata.get('token')
                refresh = metadata.get('refresh')

                if token and refresh:
                    try:
                        # 1. トークンをクライアントに追加 (これにより self._tokens に保存される)
                        await self.add_token(token, refresh)

                        # 2. サブスクリプションオブジェクトを作成
                        sub = eventsub.ChatMessageSubscription(
                            broadcaster_user_id=user_id_str,
                            user_id=self.bot_id_str
                        )

                        # 3. token_for にはボットのIDを指定して購読
                        await self.subscribe_websocket(sub, token_for=self.bot_id_str)

                        LOGGER.info(f"Subscribed to channel.chat.message for broadcaster {user_id_str}")

                    except Exception as e:
                        LOGGER.warning(f"Failed to add token or create subscription for {user_id_str}: {e}")

        LOGGER.info("Setup complete!")


    async def event_ready(self) -> None:
        """ボットが正常にログインしたときに呼び出されます。"""
        LOGGER.info(f"Twitchに正常にログインしました: {self.nick} (ID: {self.bot_id})")

    async def event_oauth_authorized(self, payload: UserTokenPayload) -> None:
        """OAuth認証が成功したときに呼び出されます。"""
        resp: ValidateTokenPayload = await self.add_token(payload.access_token, payload.refresh_token) # type: ignore
        user_id = resp.user_id

        doc_id = user_id
        metadata = {
            'token': payload.access_token,
            'refresh': payload.refresh_token,
        }
        
        self.token_collection.upsert(
            ids=[user_id], # type: ignore
            metadatas=[metadata],
            documents=[f"auth_token_for_{user_id}"] # ドキュメントの例
        )
        LOGGER.info(f"データベース(ChromaDB)にID '{user_id}' のトークンを追加/更新しました。")

        if user_id != self.bot_id_str:
            try:
                # 新しいユーザーに対してチャットメッセージのサブスクリプションを登録
                new_subscription_payload = eventsub.ChatMessageSubscription(
                    broadcaster_user_id=user_id,
                    user_id=self.bot_id_str
                )
                # token_for にはボットのIDを指定して購読
                await self.subscribe_websocket(new_subscription_payload, token_for=self.bot_id_str)
                LOGGER.info(f"Subscribed to channel.chat.message for new user {user_id}")
            except Exception as e:
                LOGGER.warning(f"ユーザー {user_id} のサブスクリプションに失敗しました: {e}")

    async def event_message(self, message: twitchio.ChatMessage) -> None:
        logging.debug(f"Twitchメッセージ受信: {message.chatter.name if message.chatter else 'Unknown'}: {message.text}")
        if self.message_callback:
            result = self.message_callback(message)
            if inspect.isawaitable(result):
                await result
        await super().event_message(message) # type: ignore
        if self.mention_callback and self.nick and (f"@{self.nick.lower()}" in message.text.lower() or "ねえぐり" in message.text):
            prompt = message.text
            if f"@{self.nick.lower()}" in prompt.lower():
                prompt = re.sub(rf"@{self.nick.lower()}\b", "", prompt, flags=re.I).strip()
            if "ねえぐり" in prompt:
                prompt = prompt.replace("ねえぐり", "").strip()
            
            author_name = message.chatter.name if message.chatter.name else ""
            channel = message.broadcaster
            try:
                result = self.mention_callback(author_name, prompt, channel)
                if inspect.isawaitable(result):
                    await result
            except Exception as exc:
                LOGGER.error(f"メンションコールバックの実行に失敗しました: {exc}", exc_info=True)

    @commands.command()
    async def hello(self, ctx: commands.Context) -> None:
        if ctx.author:
            await ctx.reply(f"Hello {ctx.author.name}!")
            
    async def send_chat_message(self, channel: twitchio.PartialUser, message: str) -> None:
        """指定されたチャンネルにチャットメッセージを送信します。"""# type: ignore
        logging.debug(f"Twitchへチャット送信: channel={getattr(channel, 'name', 'N/A')}, message={message}")
        if channel:
            try:
                await channel.send_message(message, self.bot_id)
                LOGGER.info(f"メッセージを送信しました -> {channel.name}: {message}")
            except Exception as e:
                LOGGER.error(f"メッセージの送信に失敗しました -> {channel.name}: {e}", exc_info=True)
        else:
            LOGGER.warning("メッセージの送信先チャンネルが見つかりませんでした。")


class TwitchService:
    def __init__(self, app_logic, message_callback=None, mention_callback=None):
        self.app = app_logic
        self.twitch_bot = None
        self.twitch_thread = None
        self.twitch_bot_loop = None
        self.message_callback = message_callback
        self.mention_callback = mention_callback

    def copy_auth_url(self):
        client_id = self.app.state.twitch_client_id.get()
        if not client_id:
            logging.error("Twitch Client IDが設定されていません。")
            return
        
        auth_url = twitch_auth.generate_auth_url(client_id)
        
        try:
            import pyperclip
            pyperclip.copy(auth_url)
            logging.info("Twitch認証URLをクリップボードにコピーしました。")
        except ImportError:
            logging.warning("pyperclipモジュールが見つかりません。`pip install pyperclip`でインストールしてください。")
            logging.info(f"認証URL: {auth_url}")
        except Exception as e:
            logging.error(f"クリップボードへのコピーに失敗しました: {e}", exc_info=True)
            logging.info(f"認証URL: {auth_url}")

    def register_auth_code(self):
        threading.Thread(target=self.run_register_auth_code, daemon=True).start()

    def run_register_auth_code(self):
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(self.async_register_auth_code())

    async def async_register_auth_code(self):
        code = self.app.state.twitch_auth_code.get()
        if not code:
            logging.error("Twitch認証コードが入力されていません。")
            return

        client_id = self.app.state.twitch_client_id.get()
        client_secret = self.app.state.twitch_client_secret.get()

        if not all([client_id, client_secret]):
            logging.error("Twitch Client IDまたはClient Secretが設定されていません。")
            return
        
        logging.info(f"認証コード '{code[:10]}...' を使ってトークンを交換しています...")
        try:
            result = await twitch_auth.exchange_code_for_token(client_id, client_secret, code)
            if result and result.get("user_id"):
                user_id = result["user_id"]
                logging.info(f"成功: ユーザーID {user_id} のトークンを登録しました。")
                
                # 登録されたIDをBot IDとして設定・保存する
                self.app.state.twitch_bot_id.set(user_id)
                self.app.state.save('bot_id', user_id)
                logging.info(f"Bot IDを {user_id} に設定し、保存しました。")

                self.app.state.twitch_auth_code.set("")
            else:
                logging.error("トークンの登録に失敗しました。")
        except Exception as e:
            logging.error(f"トークン登録中にエラーが発生しました: {e}", exc_info=True)

    def toggle_twitch_connection(self):
        if self.twitch_bot and self.twitch_thread and self.twitch_thread.is_alive():
            self.disconnect_twitch_bot()
        else:
            self.connect_twitch_bot()

    def connect_twitch_bot(self):
        threading.Thread(target=self.run_connect_twitch_bot, daemon=True).start()

    def run_connect_twitch_bot(self):
        logging.info("Twitchボット接続スレッドを開始します (run_connect_twitch_bot)")
        try:
            self.twitch_bot_loop = asyncio.new_event_loop()
            asyncio.set_event_loop(self.twitch_bot_loop)
            self.twitch_bot_loop.run_until_complete(self.async_connect_twitch_bot())

            if self.twitch_bot:
                logging.info("Twitchボットインスタンスが作成されました。実行スレッドを開始します。")
                self.twitch_thread = threading.Thread(target=self.run_bot_in_thread, args=(self.twitch_bot_loop,), daemon=True)
                self.twitch_thread.start()
                
                # UIボタンの更新（SettingsWindowが開いている場合のみ）
                if self.app.twitch_connect_button and self.app.twitch_connect_button.winfo_exists():
                    self.app.root.after(0, self.app.twitch_connect_button.config, {"text": "切断", "style": "danger.TButton"})
                else:
                    logging.info("Twitch bot connected. (Settings UI is closed)")
            else:
                logging.warning("Twitchボットインスタンスの作成に失敗したため、実行スレッドは開始されません。")
        except Exception as e:
            logging.error(f"Twitchボット接続スレッドで致命的なエラーが発生しました: {e}", exc_info=True)

    async def async_connect_twitch_bot(self):
        logging.info("Twitchボットへの非同期接続処理を開始します (async_connect_twitch_bot)")
        client_id = self.app.state.twitch_client_id.get()
        client_secret = self.app.state.twitch_client_secret.get()
        
        bot_id = self.app.state.twitch_bot_id.get()
        if not bot_id:
            logging.error("ボットのIDが設定ファイルに見つかりません。認証コードでボットのトークンを登録してください。")
            return

        logging.debug(f"ボットトークンの有効性を確認しています... Bot ID: {bot_id}")
        if not await twitch_auth.ensure_bot_token_valid(client_id, client_secret, bot_id):
            logging.error("ボットトークンの有効性確認に失敗しました。")
            return

        logging.debug("DBからボットトークンを取得しています...")
        bot_token_info = await twitch_auth.get_token_from_db(bot_id)
        if not bot_token_info or 'token' not in bot_token_info:
            logging.error("DBからボットのトークンを取得できませんでした。")
            return
        bot_token = bot_token_info['token']

        logging.info("Twitchボットインスタンスを初期化しています...")
        try:
            logging.debug("ChromaDBクライアントを初期化しています (path='./chroma_tokens_data')...")
            # DBパスの絶対パスを表示して確認
            import os
            abs_path = os.path.abspath("./chroma_tokens_data")
            logging.debug(f"ChromaDB Path: {abs_path}")
            
            chroma_client = chromadb.PersistentClient(path="./chroma_tokens_data")
            logging.debug("ChromaDBクライアント初期化完了。コレクションを取得/作成します...")
            
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
            logging.debug("ChromaDBコレクション取得完了。TwitchBotクラスをインスタンス化します...")
            
            self.twitch_bot = TwitchBot(
                token=bot_token,
                client_id=client_id,
                client_secret=client_secret,
                bot_id=bot_id,
                owner_id=bot_id,
                nick=self.app.state.twitch_bot_username.get(),
                token_collection=token_collection,
                message_callback=self.message_callback,
                mention_callback=self.mention_callback,
            )
            logging.info("TwitchBotインスタンス化に成功しました。")

        except Exception as e:
            logging.error(f"Twitchへの接続初期化中にエラーが発生しました: {e}", exc_info=True)
            self.twitch_bot = None

    def disconnect_twitch_bot(self):
        logging.info("Twitchボットの切断を試みます...")
        if self.twitch_bot and self.twitch_bot_loop:
            asyncio.run_coroutine_threadsafe(self.twitch_bot.close(), self.twitch_bot_loop)
        if self.twitch_thread and self.twitch_thread.is_alive():
            self.twitch_thread.join(timeout=5)
        self.twitch_thread = None
        self.twitch_bot = None
        
        # UIボタンの更新（SettingsWindowが開いている場合のみ）
        if self.app.twitch_connect_button and self.app.twitch_connect_button.winfo_exists():
            self.app.root.after(0, self.app.twitch_connect_button.config, {"text": "接続", "style": "primary.TButton"})
        
        logging.info("Twitchボットを切断しました。")

    def run_bot_in_thread(self, loop):
        asyncio.set_event_loop(loop)
        if self.twitch_bot:
            loop.run_until_complete(self.twitch_bot.start())

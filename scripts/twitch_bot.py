import logging
import re
from typing import Optional, List, Callable, Awaitable, Any

from twitchio.ext import commands
from twitchio import eventsub
from twitchio.authentication import UserTokenPayload, ValidateTokenPayload

# ロガーの設定
LOGGER = logging.getLogger(__name__)

# mention_callbackの型定義
MentionCallback = Optional[Callable[[str, str], Awaitable[Optional[str]]]]

async def setup_database(chroma_client: Any, bot_id: str) -> tuple[list[tuple[str, str]], Any]:
    """
    ChromaDBをセットアップします。
    """
    tokens = []
    token_collection = chroma_client.get_or_create_collection(name="user_tokens")
    results = token_collection.get()

    if results and results['ids']:
        for i, user_id_str in enumerate(results['ids']):
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
    def __init__(self, *, client_id: str, client_secret: str, bot_id: str, owner_id: str, nick: str, token_collection: Any, mention_callback: MentionCallback = None) -> None:
        self.token_collection = token_collection
        self.mention_callback = mention_callback
        self.bot_id_str = bot_id
        self.nick = nick
        super().__init__(
            client_id=client_id,
            client_secret=client_secret,
            prefix="!",
            owner_id=owner_id,
            bot_id=bot_id,
        )

    async def setup_hook(self) -> None:
        """
        クライアントがログインし、すべてのイベント処理を開始する前に呼び出される非同期フック。
        """
        LOGGER.info("Bot is setting up...")
        # DBからトークンを読み込み、EventSubを購読
        results = self.token_collection.get()
        initial_subs = []

        if results and results['ids']:
            for i, user_id_str in enumerate(results['ids']):
                metadata = results['metadatas'][i]
                token = metadata.get('token')
                refresh = metadata.get('refresh')

                if token and refresh:
                    try:
                        # トークンを追加
                        await self.add_user_token(token, refresh) # type: ignore
                        # ボット自身のチャンネルを除き、チャットメッセージイベントを購読
                        if user_id_str != self.bot_id_str:
                            initial_subs.append(eventsub.ChatMessageSubscription(broadcaster_user_id=user_id_str, user_id=self.bot_id_str))
                    except Exception as e:
                        LOGGER.warning(f"Failed to add token or create subscription for {user_id_str}: {e}")
        
        if initial_subs:
            try:
                await self.multi_subscribe(initial_subs) # type: ignore
            except Exception as e:
                LOGGER.error(f"Failed to subscribe to initial events: {e}")

        LOGGER.info("Setup complete!")


    async def event_ready(self) -> None:
        """ボットが正常にログインしたときに呼び出されます。"""
        LOGGER.info(f"Successfully logged in as: {self.bot_id}")
        print(f"** {self.nick} (id: {self.bot_id}) として正常にログインしました！ **")

    async def event_oauth_authorized(self, payload: UserTokenPayload) -> None:
        """OAuth認証が成功したときに呼び出されます。"""
        resp: ValidateTokenPayload = await self.add_user_token(payload.access_token, payload.refresh_token) # type: ignore
        user_id = resp.user_id

        self.token_collection.upsert(
            ids=[user_id], # type: ignore
            metadatas=[{'token': payload.access_token, 'refresh': payload.refresh_token}],
            documents=["auth_token"]
        )
        LOGGER.info(f"データベース(ChromaDB)にユーザーID {user_id} のトークンを追加/更新しました")

        if user_id != self.bot_id_str:
            new_subs = [eventsub.ChatMessageSubscription(broadcaster_user_id=user_id, user_id=self.bot_id_str)]
            try:
                await self.multi_subscribe(new_subs) # type: ignore
            except Exception as e:
                LOGGER.warning(f"ユーザー {user_id} のサブスクリプションに失敗しました: {e}")

    async def event_message(self, message: Any) -> None:
        if message.echo:
            return
        await self.handle_commands(message) # type: ignore
        if self.mention_callback and self.nick and f"@{self.nick.lower()}" in message.content.lower(): # type: ignore
            prompt = re.sub(rf"@{self.nick.lower()}\b", "", message.content, flags=re.I).strip() # type: ignore
            author_name = message.author.name if message.author else ""
            try:
                reply = await self.mention_callback(author_name, prompt)
                if reply and message.channel:
                    await message.channel.send(reply)
            except Exception as exc:
                LOGGER.error(f"mention_callback failed: {exc}")

    @commands.command()
    async def hello(self, ctx: commands.Context) -> None:
        if ctx.author:
            await ctx.reply(f"Hello {ctx.author.name}!")
            
    async def send_chat_message(self, channel_name: str, message: str) -> None:
        """指定されたチャンネルにチャットメッセージを送信します。"""
        channel = self.get_channel(channel_name) # type: ignore
        if channel:
            await channel.send(message)
            LOGGER.info(f"Sent message to {channel_name}: {message}")
        else:
            LOGGER.warning(f"Could not find channel {channel_name} to send message.")


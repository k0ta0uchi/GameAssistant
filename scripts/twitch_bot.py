# scripts/twitch_bot.py
import asyncio
import re
import collections
from typing import Optional, List, Callable, Awaitable, Any, Protocol, runtime_checkable, Coroutine

from twitchio.ext import commands

# mention_callback: (author_name, prompt) -> Optional[str] (非同期)
MentionCallback = Optional[Callable[[str, str], Awaitable[Optional[str]]]]

# Protocol を使って必要な属性だけ定義する（Pylance に content 等の存在を教える）
@runtime_checkable
class ChatMessageLike(Protocol):
    content: str
    echo: Optional[bool]
    author: Any
    id: Optional[str]
    tags: Optional[dict]
    channel: Any

class TwitchBot(commands.Bot):
    # Pylance に「これらの属性がある」と明示
    nick: str
    user_id: str
    initial_channels: List[str]

    # base にあるメソッドへアクセスする箇所が静的解析で見つからない場合の保険
    get_channel: Any
    handle_commands: Any
    join_channels: Callable[..., Awaitable[Any]]
    run: Callable[..., None]
    close: Callable[..., Coroutine[Any, Any, None]]

    def __init__(
        self,
        *,
        token: str,
        client_id: str,
        client_secret: str,
        bot_id: str,
        prefix: str = "!",
        mention_callback: MentionCallback = None,
        initial_channels: Optional[List[str]] = None,
    ) -> None:
        self.mention_callback = mention_callback
        self._recent_message_ids = collections.deque(maxlen=200)
        self.initial_channels = initial_channels or []

        super().__init__(token=token, client_id=client_id, client_secret=client_secret, bot_id=bot_id, prefix=prefix)

    async def event_ready(self) -> None:
        print(f"[ready] logged in as: (id: {self.bot_id})")
        if not self.initial_channels:
            print("[info] no initial_channels configured")
            return

    async def event_message(self, message: ChatMessageLike) -> None:
        # 無限ループ防止（echo / author / message-id）
        if getattr(message, "echo", False):
            return

        author = getattr(message, "author", None)
        try:
            author_name = (author.name or "").lower() if author else ""
        except Exception:
            author_name = ""

        if author_name and author_name == (self.nick or "").lower():
            return

        msg_id = getattr(message, "id", None) or (getattr(message, "tags", {}) or {}).get("id")
        if msg_id:
            if msg_id in self._recent_message_ids:
                return
            self._recent_message_ids.append(msg_id)

        content = (message.content or "")
        if f"@{(self.nick or '').lower()}" in content.lower():
            prompt = re.sub(rf"@{re.escape(self.nick)}\b", "", content, flags=re.I).strip()
            if self.mention_callback:
                try:
                    reply = await self.mention_callback(author.name if author else "", prompt)
                    if reply:
                        chan = getattr(message, "channel", None)
                        if chan:
                            await chan.send(reply)
                        else:
                            if self.initial_channels:
                                ch = self.get_channel(self.initial_channels[0])
                                if ch:
                                    await ch.send(reply)
                except Exception as exc:
                    print(f"[error] mention_callback failed: {exc}")

        try:
            await self.handle_commands(message)
        except Exception as e:
            print(f"[error] handle_commands raised: {e}")

    async def send_chat_message(self, channel_name: str, message: str) -> None:
        try:
            # チャンネルオブジェクトを取得
            channel = self.connected_channels[0]

            if channel:
                await channel.send(message)
                print(f"[info] sent message to {channel_name}")
            else:
                print(f"[warn] could not resolve channel {channel_name}")

        except Exception as e:
            print(f"[error] failed to send message to {channel_name}: {e}")

    @commands.command()
    async def hello(self, ctx: commands.Context) -> None:
        await ctx.send(f"Hello {ctx.author.name}!")

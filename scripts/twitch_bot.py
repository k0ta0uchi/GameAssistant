from twitchio.ext import commands
import re
import collections
from typing import Optional, List, Callable, Awaitable, Any, Protocol, runtime_checkable

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

class TwitchBot(commands.AutoBot):
    # Pylance に「これらの属性がある」と明示
    nick: str
    user_id: str
    
    # base にあるメソッドへアクセスする箇所が静的解析で見つからない場合の保険
    get_channel: Any
    handle_commands: Any
    join_channels: Callable[..., Awaitable[Any]]

    def __init__(
        self,
        *,
        client_id: str,
        client_secret: str,
        bot_id: str,
        owner_id: Optional[str] = None,
        prefix: str = "!",
        mention_callback: MentionCallback = None,
        initial_channels: Optional[List[str]] = None,
    ) -> None:
        self.mention_callback = mention_callback
        self._recent_message_ids = collections.deque(maxlen=200)
        self.initial_channels = initial_channels or []

        super().__init__(
            client_id=client_id,
            client_secret=client_secret,
            bot_id=bot_id,
            owner_id=owner_id,
            prefix=prefix,
        )

        # 実行時にも属性を確実にセットしておく（静的解析との齟齬防止）
        self.nick = getattr(self, "nick", "") or ""
        self.user_id = bot_id

    async def setup_hook(self) -> None:
        # ここでコンポーネント登録するなど
        return

    async def event_ready(self) -> None:
        print(f"[ready] logged in as: {self.nick} (id: {self.user_id})")
        # AutoBot は initial_channels に基づいて自動 join するので明示的に join_channels は不要
        # ここでは通知だけにしておく
        # conduit を取得して join_channels する
        if self.conduits:
            conduit = self.conduits[0]
            await conduit.join_channels(self.initial_channels)
            print(f"[ready] joined channels: {self.initial_channels}")
        else:
            print("[warn] no conduit available, cannot join channels")

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
        name = channel_name if channel_name.startswith("#") else f"#{channel_name}"
        ch = self.get_channel(name)
        if ch:
            await ch.send(message)
        else:
            print(f"[warn] channel not found: {name}")

    @commands.command()
    async def hello(self, ctx: commands.Context) -> None:
        await ctx.send(f"Hello {ctx.author.name}!")

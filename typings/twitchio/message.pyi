from typing import Optional, Any, Dict, Coroutine, Awaitable

class User:
    id: int
    name: str

    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...

class Channel:
    name: str

    async def send(self, message: str, *args: Any, **kwargs: Any) -> None: ...
    async def reply(self, message: str, *args: Any, **kwargs: Any) -> None: ...

class ChatMessage:
    """
    必要な属性のみ定義（実行時の twitchio.ChatMessage と合わせるための部分型）
    """
    content: str
    echo: Optional[bool]
    author: Optional[User]
    id: Optional[str]
    tags: Optional[Dict[str, str]]
    channel: Optional[Channel]

    async def reply(self, msg: str) -> None: ...

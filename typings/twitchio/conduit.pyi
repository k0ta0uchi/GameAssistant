from typing import List, Optional, Any, Coroutine, Awaitable
from twitchio.message import ChatMessage, Channel

class Conduit:
    id: str
    # channels は "#channel" 形式、もしくは plain "channel"
    async def join_channels(self, channels: List[str] | str, *, conduit_id: Optional[str] = None) -> None: ...
    async def leave_channels(self, channels: List[str] | str, *, conduit_id: Optional[str] = None) -> None: ...

    # 便利に使われることがあるメソッド（存在することが多い）
    async def close(self) -> None: ...

"""Subscription topics. A topic factory turns ``$/subscribe`` params into a live subscription."""

from __future__ import annotations

import secrets
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, cast

from pydantic import ValidationError

from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EVENT_LOG
from spaceknife_protocol.streams import TOPIC_LOG, LogLevel, LogRecordEvent, LogTopicParams

from ..logbus import LEVEL_NO, LogEntry, LogHub
from .rpc import RpcError

if TYPE_CHECKING:
    from .session import Session


@dataclass(slots=True)
class Subscription:
    topic: str
    id: str = field(default_factory=lambda: "s" + secrets.token_hex(6))
    on_close: Callable[[], None] | None = None
    closed: bool = False

    def close(self) -> None:
        if not self.closed:
            self.closed = True
            if self.on_close is not None:
                self.on_close()


TopicFactory = Callable[["Session", dict[str, Any] | None, float | None], Awaitable[Subscription]]


class TopicRegistry:
    def __init__(self) -> None:
        self._factories: dict[str, TopicFactory] = {}

    def register(self, name: str, factory: TopicFactory) -> None:
        self._factories[name] = factory

    async def open(
        self, session: Session, name: str, params: dict[str, Any] | None, rate_hz: float | None
    ) -> Subscription:
        factory = self._factories.get(name)
        if factory is None:
            raise RpcError(
                ErrorCode.UNKNOWN_TOPIC, f"unknown topic: {name}", {"topics": sorted(self._factories)}
            )
        return await factory(session, params, rate_hz)

    def names(self) -> list[str]:
        return sorted(self._factories)


def log_topic(hub: LogHub) -> TopicFactory:
    """``log`` topic: replays a backlog, then streams new records at or above ``min_level``."""

    async def open_log(
        session: Session, params: dict[str, Any] | None, rate_hz: float | None
    ) -> Subscription:
        try:
            opts = LogTopicParams.model_validate(params or {})
        except ValidationError as exc:
            raise RpcError(ErrorCode.INVALID_PARAMS, "invalid log topic params") from exc
        floor = LEVEL_NO[opts.min_level]
        sub = Subscription(topic=TOPIC_LOG)

        def forward(entry: LogEntry) -> None:
            if sub.closed or entry.level_no < floor:
                return
            session.emit(
                EVENT_LOG,
                LogRecordEvent(
                    sub=sub.id,
                    seq=entry.seq,
                    time=entry.time,
                    level=cast(LogLevel, entry.level),
                    source=entry.source,
                    message=entry.message,
                    extra=entry.extra,
                ),
            )

        for entry in hub.recent(opts.backlog, opts.min_level):
            forward(entry)
        token = hub.subscribe(forward)
        sub.on_close = lambda: hub.unsubscribe(token)
        return sub

    return open_log

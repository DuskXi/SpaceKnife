"""In-process log bus feeding the ``log`` topic (W8 log panel).

loguru may call :meth:`LogHub.sink` from any thread; subscribers live on the asyncio loop,
so delivery always goes through ``loop.call_soon_threadsafe``.
"""

from __future__ import annotations

import asyncio
import threading
from collections import deque
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import datetime
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from loguru import Message

LEVEL_NO = {"TRACE": 5, "DEBUG": 10, "INFO": 20, "SUCCESS": 25, "WARNING": 30, "ERROR": 40, "CRITICAL": 50}


@dataclass(frozen=True, slots=True)
class LogEntry:
    seq: int
    time: datetime
    level: str
    source: str
    message: str
    extra: dict[str, str] = field(default_factory=dict)

    @property
    def level_no(self) -> int:
        return LEVEL_NO.get(self.level, 0)


Listener = Callable[[LogEntry], None]


class LogHub:
    def __init__(self, capacity: int = 5000) -> None:
        self._lock = threading.Lock()
        self._buffer: deque[LogEntry] = deque(maxlen=capacity)
        self._seq = 0
        self._listeners: dict[int, tuple[asyncio.AbstractEventLoop, Listener]] = {}
        self._next_token = 0

    def sink(self, message: Message) -> None:
        record = message.record
        extra = {str(k): str(v) for k, v in record["extra"].items() if not str(k).startswith("_")}
        source = str(extra.pop("source", "") or record["name"] or "")
        with self._lock:
            self._seq += 1
            entry = LogEntry(
                seq=self._seq,
                time=record["time"],
                level=record["level"].name,
                source=source,
                message=record["message"],
                extra=extra,
            )
            self._buffer.append(entry)
            listeners = list(self._listeners.values())
        for loop, listener in listeners:
            if not loop.is_closed():
                loop.call_soon_threadsafe(listener, entry)

    def recent(self, limit: int, min_level: str = "TRACE") -> list[LogEntry]:
        floor = LEVEL_NO.get(min_level, 0)
        with self._lock:
            items = [e for e in self._buffer if e.level_no >= floor]
        return items[-limit:] if limit else []

    def subscribe(self, listener: Listener, loop: asyncio.AbstractEventLoop | None = None) -> int:
        with self._lock:
            self._next_token += 1
            token = self._next_token
            self._listeners[token] = (loop or asyncio.get_running_loop(), listener)
        return token

    def unsubscribe(self, token: int) -> None:
        with self._lock:
            self._listeners.pop(token, None)

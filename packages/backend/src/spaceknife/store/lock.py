"""The lock between threads that read a scanned tree and the changes that rewrite parts of it.

A scan only appends (readers are safe without a lock, see :meth:`NodeStore.child_range`), but a
file change (M5) moves a folder's children to a new id range and marks the old rows ``GONE``: a
render or a summary walking that folder at the same moment could pair the old start with the new
count. So every call on the compute pool reads under :data:`STRUCTURE`, and the event loop takes
it for writing around such a change. The writer waits *asynchronously* for running readers and
then works synchronously on the event loop thread — so the loop's own (quick) reads cannot
interleave with it either. Writers have priority: no new reader starts while one waits.

One lock for all stores: changes are short (milliseconds) and rare (coalesced).
"""

from __future__ import annotations

import asyncio
import contextlib
import threading
from collections.abc import AsyncIterator, Iterator


class StructureLock:
    def __init__(self) -> None:
        self._cond = threading.Condition()
        self._readers = 0
        self._writing = False
        self._waiting = 0  # writers waiting to start: new readers hold back
        self._wakers: list[tuple[asyncio.AbstractEventLoop, asyncio.Event]] = []

    @contextlib.contextmanager
    def read(self) -> Iterator[None]:
        """Shared access from a worker thread (blocks while a change is pending or running)."""
        with self._cond:
            while self._writing or self._waiting:
                self._cond.wait()
            self._readers += 1
        try:
            yield
        finally:
            with self._cond:
                self._readers -= 1
                if self._readers == 0:
                    self._wake()

    @contextlib.asynccontextmanager
    async def write(self) -> AsyncIterator[None]:
        """Exclusive access on the event loop thread; the body must not await (it would let other
        coroutines read a half-made change)."""
        loop = asyncio.get_running_loop()
        with self._cond:
            self._waiting += 1
        try:
            while True:
                with self._cond:
                    if self._readers == 0 and not self._writing:
                        self._writing = True
                        break
                    event = asyncio.Event()
                    self._wakers.append((loop, event))
                await event.wait()
        finally:
            with self._cond:
                self._waiting -= 1
        try:
            yield
        finally:
            with self._cond:
                self._writing = False
                self._cond.notify_all()
                self._wake()  # another writer may be waiting

    def _wake(self) -> None:
        """Let waiting writers look again (called with ``_cond`` held)."""
        for loop, event in self._wakers:
            loop.call_soon_threadsafe(event.set)
        self._wakers.clear()

    @property
    def readers(self) -> int:
        return self._readers


STRUCTURE = StructureLock()

__all__ = ["STRUCTURE", "StructureLock"]

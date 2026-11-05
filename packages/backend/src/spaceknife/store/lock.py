"""The lock between threads that read a scanned tree and the changes that rewrite parts of it.

A scan only appends (readers are safe without a lock, see :meth:`NodeStore.child_range`), but a
file change (M5) moves a folder's children to a new id range and marks the old rows ``GONE``: a
render or a summary walking that folder at the same moment could pair the old start with the new
count. So every call on the compute pool reads under :data:`STRUCTURE`, and the event loop takes
it for writing around such a change. The writer waits *asynchronously* for running readers and
then works synchronously on the event loop thread — so the loop's own (quick) reads cannot
interleave with it either.

Who waits for whom: a writer waits for the readers already running when it asked. While it does,
new readers still start — the writer could not start before those older readers finish anyway —
so one long reader (a cleanup plan built over a large scan, say) holds up the changes, never every
other reader with them: a waiting writer used to stop all new readers, and with them the pictures,
the summaries and the views of every other scan, for as long as that one reader ran. Once the
older readers are done, new readers wait for the writer; it waits at most for the readers that
started until then, so a steady stream of short readers cannot starve it.

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
        self._waiting = 0  # writers waiting to start
        self._next = 0  # ticket of the next reader
        self._running: set[int] = set()  # tickets of the readers running now
        self._cut = 0  # readers with a smaller ticket were running when the waiting writers asked
        self._wakers: list[tuple[asyncio.AbstractEventLoop, asyncio.Event]] = []

    def _held_back(self) -> bool:
        """Whether a new reader waits (``_cond`` held): while a change runs, and while a writer
        waits that no longer waits for any reader older than itself."""
        if self._writing:
            return True
        if not self._waiting:
            return False
        return not any(t < self._cut for t in self._running)

    @contextlib.contextmanager
    def read(self) -> Iterator[None]:
        """Shared access from a worker thread (blocks while a change runs or is about to)."""
        with self._cond:
            while self._held_back():
                self._cond.wait()
            ticket = self._next
            self._next += 1
            self._running.add(ticket)
            self._readers += 1
        try:
            yield
        finally:
            with self._cond:
                self._running.discard(ticket)
                self._readers -= 1
                if self._readers == 0:
                    self._wake()

    @contextlib.asynccontextmanager
    async def write(self) -> AsyncIterator[None]:
        """Exclusive access on the event loop thread; the body must not await (it would let other
        coroutines read a half-made change)."""
        loop = asyncio.get_running_loop()
        with self._cond:
            if not self._waiting:
                self._cut = self._next  # the readers running now are the ones it waits for
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
                if not self._writing:
                    self._cond.notify_all()  # given up (cancelled): readers held back go on
        try:
            yield
        finally:
            with self._cond:
                self._writing = False
                if self._waiting:
                    self._cut = self._next  # no reader runs now: the next writer waits for none
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

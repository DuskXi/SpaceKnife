"""Bounded worker threads for work that walks a whole scan: renders, filters, summaries, searches.

Each such call allocates temporaries proportional to the scan — a few hundred MB for ten million
nodes. On asyncio's default executor (up to 32 threads here) a burst of requests ran them all at
once, and once the machine started paging every call got slower, so even more of them overlapped.
A small pool bounds the temporaries to ``WORKERS`` calls; numpy releases the GIL in the heavy
parts, so a few threads keep the cores busy. Blocking I/O (volume probes, snapshot files) stays on
the default executor.

:class:`Latest` drops queued calls that a newer request made pointless (the side panel asking for
the summary of every frame while a scan runs, a search for each typed character).
"""

from __future__ import annotations

import asyncio
import functools
import os
import threading
from collections.abc import Callable, Hashable
from concurrent.futures import ThreadPoolExecutor

from spaceknife_protocol.errors import ErrorCode

from ..store.lock import STRUCTURE
from .rpc import RpcError

WORKERS = max(2, min(4, (os.cpu_count() or 4) // 2))

_pool: ThreadPoolExecutor | None = None
_pool_lock = threading.Lock()


def pool() -> ThreadPoolExecutor:
    global _pool
    with _pool_lock:
        if _pool is None:
            _pool = ThreadPoolExecutor(max_workers=WORKERS, thread_name_prefix="spaceknife-compute")
        return _pool


async def compute[T](fn: Callable[..., T], /, *args: object, **kwargs: object) -> T:
    """Run ``fn`` on the compute pool, reading scanned trees under :data:`STRUCTURE` (a file change
    never rewrites a folder under it). Cancelling the awaiting task drops the call if it has not
    started yet (a running call finishes; its result is discarded)."""
    loop = asyncio.get_running_loop()
    return await loop.run_in_executor(pool(), _reading, functools.partial(fn, *args, **kwargs))


def _reading[T](fn: Callable[[], T]) -> T:
    with STRUCTURE.read():
        return fn()


class Superseded(RpcError):
    def __init__(self) -> None:
        super().__init__(ErrorCode.REQUEST_CANCELLED, "superseded by a newer request")


class Latest:
    """Latest-wins slots: a call still queued when a newer one with the same key arrives is dropped
    (raises :class:`Superseded`, which clients treat like a cancellation)."""

    def __init__(self) -> None:
        self._gen: dict[Hashable, int] = {}
        self._lock = threading.Lock()

    async def run[T](self, key: Hashable, fn: Callable[..., T], /, *args: object) -> T:
        with self._lock:
            gen = self._gen[key] = self._gen.get(key, 0) + 1

        def job() -> T:
            with self._lock:
                stale = self._gen.get(key) != gen
            if stale:
                raise Superseded
            return fn(*args)

        try:
            return await compute(job)
        finally:
            with self._lock:
                if self._gen.get(key) == gen:
                    del self._gen[key]

    def __len__(self) -> int:
        return len(self._gen)


def stats() -> dict[str, int]:
    """Queued and running calls of the pool (diagnostics)."""
    p = _pool
    if p is None:
        return {"workers": WORKERS, "queued": 0}
    return {"workers": WORKERS, "queued": p._work_queue.qsize()}


__all__ = ["WORKERS", "Latest", "Superseded", "compute", "pool", "stats"]

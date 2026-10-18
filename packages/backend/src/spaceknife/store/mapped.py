"""Memory-mapped mode of the node store: columns and names in temporary files.

Pages of a shared file mapping are backed by their file, not by the page file: they are not
private commit (Windows) or anonymous memory (Linux, macOS). The OS keeps the pages in use in RAM —
reading them then costs the same as plain memory — and drops cold ones without writing them to
swap. A store switches to it when its columns would push the open scans past the memory budget
(``advanced.memory_budget``: a share of physical RAM); smaller scans stay in plain memory.

The files clean up after themselves: opened delete-on-close on Windows (the mapping holds the last
handle) and unlinked right after creation on POSIX, so nothing is left behind, even after a crash.
"""

from __future__ import annotations

import itertools
import mmap
import os
import secrets
import sys
import threading
import weakref
from pathlib import Path
from typing import TYPE_CHECKING, Any

import numpy as np

from .. import paths
from ..memory import physical_memory

if TYPE_CHECKING:
    from .nodes import NodeStore

_ids = itertools.count()


def columns_dir() -> Path:
    d = paths.cache_dir() / "columns"
    d.mkdir(parents=True, exist_ok=True)
    return d


def mapped_zeros(count: int, dtype: Any = np.uint8) -> np.ndarray:
    """A zero-filled, writable array of ``count`` items backed by a self-deleting temporary file."""
    dt = np.dtype(dtype)
    size = max(mmap.ALLOCATIONGRANULARITY, count * dt.itemsize)
    path = columns_dir() / f"sk-{os.getpid()}-{next(_ids)}-{secrets.token_hex(4)}.bin"
    flags = os.O_RDWR | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOINHERIT", 0)
    if sys.platform == "win32":
        # delete on close; "short lived": the cache manager writes pages out only under pressure
        flags |= os.O_TEMPORARY | os.O_SHORT_LIVED
    fd = os.open(path, flags, 0o600)
    try:
        if sys.platform != "win32":
            os.unlink(path)  # the mapping keeps the data; the name goes now
        os.ftruncate(fd, size)  # a fresh extent reads as zeros
        mm = mmap.mmap(fd, size, access=mmap.ACCESS_WRITE)
    finally:
        os.close(fd)  # Windows: the mapping holds a duplicate handle, the file goes when it closes
    return np.frombuffer(mm, dtype=dt, count=count)


def is_mapped(arr: np.ndarray) -> bool:
    base: Any = arr
    while isinstance(base, np.ndarray) and base.base is not None:
        base = base.base
    if isinstance(base, memoryview):  # np.frombuffer keeps the buffer behind a memoryview
        base = base.obj
    return isinstance(base, mmap.mmap)


def cleanup_stale() -> int:
    """Remove column files left by processes that are gone (files in use cannot be deleted)."""
    removed = 0
    try:
        entries = list(columns_dir().iterdir())
    except OSError:
        return 0
    for p in entries:
        try:
            p.unlink()
            removed += 1
        except OSError:
            pass
    return removed


class Policy:
    """When a growing or loading store switches to file-backed columns.

    ``fraction`` of physical memory is the budget for the columns and names of all open stores in
    plain memory. ``SPACEKNIFE_MAP_ABOVE`` (bytes) overrides it, for tests and experiments.
    """

    def __init__(self) -> None:
        self.fraction = 0.25
        env = os.environ.get("SPACEKNIFE_MAP_ABOVE")
        self.force_bytes: int | None = int(env) if env else None
        self._stores: weakref.WeakSet[NodeStore] = weakref.WeakSet()
        self._lock = threading.Lock()

    def register(self, store: NodeStore) -> None:
        with self._lock:
            self._stores.add(store)

    def budget(self) -> int | None:
        if self.force_bytes is not None:
            return self.force_bytes
        phys = physical_memory()
        return int(phys * self.fraction) if phys else None

    def should_map(self, store: NodeStore | None, new_bytes: int) -> bool:
        budget = self.budget()
        if budget is None:
            return False
        with self._lock:
            others = [s for s in self._stores if s is not store]
        return sum(s.ram_bytes() for s in others) + new_bytes > budget


POLICY = Policy()

__all__ = ["POLICY", "Policy", "cleanup_stale", "columns_dir", "is_mapped", "mapped_zeros"]

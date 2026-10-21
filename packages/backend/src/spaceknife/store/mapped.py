"""Memory-mapped mode of the node store: columns and names in temporary files.

Pages of a shared file mapping are backed by their file, not by the page file: they are not
private commit (Windows) or anonymous memory (Linux, macOS). The OS keeps the pages in use in RAM —
reading them then costs the same as plain memory — and drops cold ones without writing them to
swap. A store switches to it when it grows past :data:`LARGE_BYTES` (about two million nodes;
only with ``advanced.map_large_scans``, off by default: see the writes below), or when its
columns would push the open scans past the memory budget (``advanced.memory_budget``: a share
of physical RAM); other scans stay in plain memory.

The files clean up after themselves: opened delete-on-close on Windows (the mapping holds the last
handle) and unlinked right after creation on POSIX, so nothing is left behind, even after a crash.

The system writes the pages a store fills to its files (that is what lets it drop them later), so
every page should be written once: a growing column's file is extended in place (:func:`grow`; the
old mapping and the new one share their pages) instead of copied to a new file, and a finished
scan keeps its mapped columns as they are (``NodeStore.compact``): the unused tail was never
touched, so it takes neither memory nor writes.
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


class _Backing:
    """The open file behind a growable mapping; closed (on Windows: deleted) with its last mapping."""

    __slots__ = ("__weakref__", "fd")

    def __init__(self, fd: int) -> None:
        self.fd = fd
        weakref.finalize(self, os.close, fd)


_backings: weakref.WeakKeyDictionary[mmap.mmap, _Backing] = weakref.WeakKeyDictionary()
_backings_lock = threading.Lock()

# a store whose columns and names reach this goes to mapped files by itself (about 2 M nodes)
LARGE_BYTES = 256 << 20


def columns_dir() -> Path:
    d = paths.cache_dir() / "columns"
    d.mkdir(parents=True, exist_ok=True)
    return d


def mapped_zeros(count: int, dtype: Any = np.uint8, *, growable: bool = False) -> np.ndarray:
    """A zero-filled, writable array of ``count`` items backed by a self-deleting temporary file.

    ``growable``: the file stays open, so :func:`grow` can extend it in place."""
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
        _extend(fd, size)
        mm = mmap.mmap(fd, size, access=mmap.ACCESS_WRITE)
    except BaseException:
        os.close(fd)
        raise
    if growable:
        with _backings_lock:
            _backings[mm] = _Backing(fd)
    else:
        os.close(fd)  # Windows: the mapping holds a duplicate handle, the file goes when it closes
    return np.frombuffer(mm, dtype=dt, count=count)


def _extend(fd: int, size: int) -> None:
    """Make the file ``size`` bytes long without writing anything: the new extent reads as zeros.

    Windows: not ``os.ftruncate``, which writes every new byte as a zero (the C runtime's
    ``_chsize``: a whole volume's columns written to disk once more for nothing); mapping the file
    with the larger size extends it instead (``mmap.mmap``, right after)."""
    if sys.platform != "win32":
        os.ftruncate(fd, size)


def _mapping(arr: np.ndarray) -> mmap.mmap | None:
    base: Any = arr
    while isinstance(base, np.ndarray) and base.base is not None:
        base = base.base
    if isinstance(base, memoryview):
        base = base.obj
    return base if isinstance(base, mmap.mmap) else None


def grow(arr: np.ndarray, count: int) -> np.ndarray | None:
    """``arr`` (a whole growable mapped array) as ``count`` items, its file extended in place: the
    items it holds stay where they are, without a copy, and ``arr`` stays valid (it shares the
    pages). None when ``arr`` is not such an array or the file cannot grow (no room on the disk)."""
    mm = _mapping(arr)
    with _backings_lock:
        backing = _backings.get(mm) if mm is not None else None
    if mm is None or backing is None or arr.nbytes > len(mm) or count < len(arr):
        return None
    if np.frombuffer(mm, dtype=np.uint8, count=1).ctypes.data != arr.ctypes.data:
        return None  # a view that starts inside the mapping
    size = max(mmap.ALLOCATIONGRANULARITY, count * arr.dtype.itemsize)
    try:
        if size > os.fstat(backing.fd).st_size:
            _extend(backing.fd, size)
        new = mmap.mmap(backing.fd, size, access=mmap.ACCESS_WRITE)
    except OSError:
        return None
    with _backings_lock:
        _backings[new] = backing
    return np.frombuffer(new, dtype=arr.dtype, count=count)


def is_mapped(arr: np.ndarray) -> bool:
    return _mapping(arr) is not None  # np.frombuffer keeps the buffer behind a memoryview


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

    A store of ``large`` bytes or more (columns with their headroom, and names) maps by itself;
    ``None`` (the default, ``advanced.map_large_scans`` off) turns that off. ``fraction`` of
    physical memory is the budget for the columns and names of all open stores in plain memory.
    ``SPACEKNIFE_MAP_ABOVE`` (bytes) replaces both, for tests and experiments.
    """

    def __init__(self) -> None:
        self.fraction = 0.25
        self.large: int | None = None  # LARGE_BYTES with advanced.map_large_scans
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
        if self.force_bytes is None and self.large is not None and new_bytes >= self.large:
            return True
        budget = self.budget()
        if budget is None:
            return False
        with self._lock:
            others = [s for s in self._stores if s is not store]
        return sum(s.ram_bytes() for s in others) + new_bytes > budget


POLICY = Policy()

__all__ = [
    "LARGE_BYTES",
    "POLICY",
    "Policy",
    "cleanup_stale",
    "columns_dir",
    "grow",
    "is_mapped",
    "mapped_zeros",
]

"""Tree images: whole columns of a scan, copied once into read-only shared memory for plugin processes.

The copy is made on the compute pool under the tree's read lock (held only while copying); the
plugin maps the segment read-only (``spaceknife_sdk.image``). An image is cached per (scan, store
version, columns) and counted per plugin; at most two versions of a scan are kept, and while a scan
runs it is exported at most every two seconds (ids only grow then, so a slightly older image draws
the same nodes). The host unlinks every segment it made.
"""

from __future__ import annotations

import contextlib
import mmap
import os
import secrets
import sys
import threading
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

import numpy as np
from loguru import logger

from ..store.nodes import NodeStore

ALIGN = 8
HEADER = 16  # MAGIC and the segment's own name: the plugin checks it maps this segment
MAGIC = b"SKIMG1\0\0"
RUNNING_EVERY_S = 2.0
KEEP_VERSIONS = 2


def _pad(n: int) -> int:
    return (-n) % ALIGN


class Segment:
    """A named shared memory segment the host writes and plugin processes map read-only.

    Made directly (not with ``multiprocessing.shared_memory``, whose resource tracker would count the
    host's own unlinking as a leak): a POSIX ``shm_open`` object, or a named Windows section.
    """

    def __init__(self, name: str, size: int) -> None:
        self.name = name
        self.size = size
        if sys.platform == "win32":
            self.mm = mmap.mmap(-1, size, tagname=name)  # a new section backed by the paging file
        else:
            import _posixshmem  # type: ignore[import-not-found]  # CPython's, behind shared_memory

            fd = _posixshmem.shm_open("/" + name, os.O_CREAT | os.O_EXCL | os.O_RDWR, mode=0o600)
            try:
                os.ftruncate(fd, size)
                self.mm = mmap.mmap(fd, size)
            finally:
                os.close(fd)

    def close(self) -> None:
        with contextlib.suppress(BufferError, ValueError):
            self.mm.close()
        if sys.platform != "win32":
            import _posixshmem

            with contextlib.suppress(FileNotFoundError, OSError):
                _posixshmem.shm_unlink("/" + self.name)


@dataclass(slots=True)
class Exported:
    key: str
    scan_id: str
    version: int
    epoch: int
    n: int
    columns: frozenset[str]
    shm: Segment
    desc: dict[str, Any]
    users: set[str] = field(default_factory=set)
    made: float = field(default_factory=time.monotonic)

    @property
    def size(self) -> int:
        return int(self.desc["size"])

    def release(self) -> None:
        self.shm.close()


def export(
    scan_id: str,
    store: NodeStore,
    columns: Sequence[str],
    cls_of: Callable[[], np.ndarray] | None = None,
) -> Exported:
    """Copy ``columns`` of ``store`` into a new segment (call under the read lock)."""
    n = len(store)
    arrays: dict[str, np.ndarray] = {}
    for name in sorted(set(columns)):
        if name == "cls":
            if cls_of is None:
                continue
            arrays[name] = np.ascontiguousarray(cls_of()[:n], dtype=np.uint8)
        else:
            arrays[name] = store.cols[name][:n]
    layout: list[dict[str, Any]] = []
    offset = HEADER
    for name, arr in arrays.items():
        dtype = arr.dtype.newbyteorder("<") if arr.dtype.byteorder == ">" else arr.dtype
        layout.append({"name": name, "dtype": np.dtype(dtype).str, "offset": offset, "length": n})
        offset += arr.nbytes + _pad(arr.nbytes)
    size = max(offset, HEADER)
    tag = secrets.token_bytes(4)
    name = "sk" + tag.hex()
    shm = Segment(name, size)
    head = MAGIC + tag + bytes(HEADER - len(MAGIC) - len(tag))
    shm.mm[:HEADER] = head
    for spec, arr in zip(layout, arrays.values(), strict=True):
        dst = np.ndarray((n,), dtype=np.dtype(spec["dtype"]), buffer=shm.mm, offset=spec["offset"])
        dst[:] = arr
        del dst  # no export of the mapping left behind: it can close
    version = int(store.version)
    epoch = int(getattr(store, "epoch", 0))
    key = f"{scan_id}:{version}:{name}"
    desc = {
        "key": key,
        "scan_id": scan_id,
        "version": version,
        "epoch": epoch,
        "n": n,
        "shm": shm.name,
        "size": size,
        "columns": layout,
        "head": head.hex(),
    }
    return Exported(key, scan_id, version, epoch, n, frozenset(arrays), shm, desc)


class Images:
    """The images of one host, shared by its plugins."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._items: dict[str, Exported] = {}

    def find(self, scan_id: str, version: int, columns: set[str], running: bool) -> Exported | None:
        with self._lock:
            best: Exported | None = None
            for e in self._items.values():
                if e.scan_id != scan_id or not columns <= e.columns:
                    continue
                fresh = e.version == version or (running and time.monotonic() - e.made < RUNNING_EVERY_S)
                if fresh and (best is None or e.version > best.version):
                    best = e
            return best

    def add(self, e: Exported, user: str) -> None:
        """Keep ``e`` and, of the other images of its scan, the newest ``KEEP_VERSIONS - 1`` (newest
        version first, then the latest made: a plugin that asks for one more column of the same
        version gets a new image, which replaces its older one). Never the one just added: the
        plugin maps it next, and a segment gone before that maps as nothing (or, on Windows, as a
        fresh zero-filled one)."""
        with self._lock:
            e.users.add(user)
            self._items[e.key] = e
            others = sorted(
                (x for x in self._items.values() if x.scan_id == e.scan_id and x is not e),
                key=lambda x: (-x.version, -x.made),
            )
            for old in others[KEEP_VERSIONS - 1 :]:
                if not old.users - {user}:
                    self._items.pop(old.key, None)
                    old.release()

    def use(self, e: Exported, user: str) -> None:
        with self._lock:
            e.users.add(user)

    def drop(self, key: str, user: str) -> None:
        with self._lock:
            e = self._items.get(key)
            if e is None:
                return
            e.users.discard(user)
            newest = max(
                (x.version for x in self._items.values() if x.scan_id == e.scan_id), default=e.version
            )
            if not e.users and e.version < newest:
                self._items.pop(key, None)
                e.release()

    def release_user(self, user: str) -> None:
        with self._lock:
            for key, e in list(self._items.items()):
                e.users.discard(user)
                if not e.users:
                    self._items.pop(key, None)
                    e.release()

    def drop_scan(self, scan_id: str) -> None:
        with self._lock:
            for key, e in list(self._items.items()):
                if e.scan_id == scan_id:
                    self._items.pop(key, None)
                    e.release()

    def bytes(self) -> int:
        with self._lock:
            return sum(e.size for e in self._items.values())

    def close(self) -> None:
        with self._lock:
            items = list(self._items.values())
            self._items.clear()
        for e in items:
            e.release()
        if items:
            logger.debug("released {} plugin tree images", len(items))


__all__ = ["Exported", "Images", "export"]

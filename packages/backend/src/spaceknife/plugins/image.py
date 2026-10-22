"""Tree images: whole columns of a scan, copied once into read-only shared memory for plugin processes.

The copy is made on the compute pool under the tree's read lock (held only while copying); the
plugin maps the segment read-only (``spaceknife_sdk.image``). An image is cached per (scan, store
version, columns) and counted per plugin; at most two versions of a scan are kept, and while a scan
runs it is exported at most every two seconds (ids only grow then, so a slightly older image draws
the same nodes). The host unlinks every segment it made.
"""

from __future__ import annotations

import contextlib
import secrets
import sys
import threading
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from multiprocessing import shared_memory
from typing import Any

import numpy as np
from loguru import logger

from ..store.nodes import NodeStore

ALIGN = 8
RUNNING_EVERY_S = 2.0
KEEP_VERSIONS = 2


def _pad(n: int) -> int:
    return (-n) % ALIGN


@dataclass(slots=True)
class Exported:
    key: str
    scan_id: str
    version: int
    epoch: int
    n: int
    columns: frozenset[str]
    shm: shared_memory.SharedMemory
    desc: dict[str, Any]
    users: set[str] = field(default_factory=set)
    made: float = field(default_factory=time.monotonic)

    @property
    def size(self) -> int:
        return int(self.desc["size"])

    def release(self) -> None:
        with contextlib.suppress(Exception):
            self.shm.close()
        with contextlib.suppress(FileNotFoundError, OSError):
            self.shm.unlink()


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
    offset = 0
    for name, arr in arrays.items():
        dtype = arr.dtype.newbyteorder("<") if arr.dtype.byteorder == ">" else arr.dtype
        layout.append({"name": name, "dtype": np.dtype(dtype).str, "offset": offset, "length": n})
        offset += arr.nbytes + _pad(arr.nbytes)
    size = max(offset, ALIGN)
    name = "sk" + secrets.token_hex(4)
    shm = shared_memory.SharedMemory(name=name, create=True, size=size)
    if sys.platform != "win32":  # the host unlinks it; the tracker would warn about a "leak"
        with contextlib.suppress(Exception):
            from multiprocessing import resource_tracker

            resource_tracker.unregister(shm._name, "shared_memory")  # type: ignore[attr-defined]
    buf = shm.buf
    assert buf is not None
    for spec, arr in zip(layout, arrays.values(), strict=True):
        dst = np.ndarray((n,), dtype=np.dtype(spec["dtype"]), buffer=buf, offset=spec["offset"])
        dst[:] = arr
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
        with self._lock:
            e.users.add(user)
            self._items[e.key] = e
            same = sorted(
                (x for x in self._items.values() if x.scan_id == e.scan_id), key=lambda x: -x.version
            )
            for old in same[KEEP_VERSIONS:]:
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

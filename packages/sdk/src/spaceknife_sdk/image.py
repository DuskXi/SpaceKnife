"""Mapping a tree image the host exported (read-only shared memory, or a read-only file)."""

from __future__ import annotations

import mmap
import os
import sys
from collections.abc import Mapping
from typing import Any

import numpy as np

from ._link import Image

if sys.platform == "win32":

    def _map_shm(name: str, size: int) -> mmap.mmap:
        # opens the host's named mapping; ACCESS_READ maps it read-only (FILE_MAP_READ)
        return mmap.mmap(-1, size, tagname=name, access=mmap.ACCESS_READ)

    def _map_file(path: str, size: int) -> mmap.mmap:
        fd = os.open(path, os.O_RDONLY | os.O_BINARY)
        try:
            return mmap.mmap(fd, size, access=mmap.ACCESS_READ)
        finally:
            os.close(fd)

else:

    def _map_shm(name: str, size: int) -> mmap.mmap:
        import _posixshmem  # type: ignore[import-not-found]  # CPython's, behind shared_memory

        fd = _posixshmem.shm_open("/" + name.lstrip("/"), os.O_RDONLY, mode=0o600)
        try:
            return mmap.mmap(fd, size, prot=mmap.PROT_READ)
        finally:
            os.close(fd)

    def _map_file(path: str, size: int) -> mmap.mmap:
        fd = os.open(path, os.O_RDONLY)
        try:
            return mmap.mmap(fd, size, prot=mmap.PROT_READ)
        finally:
            os.close(fd)


def map_image(desc: Mapping[str, Any]) -> Image:
    """The columns an ``image`` descriptor names, as read-only arrays over the mapping."""
    size = int(desc["size"])
    mm = _map_file(str(desc["file"]), size) if desc.get("file") else _map_shm(str(desc["shm"]), size)
    columns: dict[str, np.ndarray] = {}
    for c in desc["columns"]:
        dtype = np.dtype(str(c["dtype"]))
        arr = np.frombuffer(mm, dtype=dtype, count=int(c["length"]), offset=int(c["offset"]))
        arr.flags.writeable = False
        columns[str(c["name"])] = arr
    return Image(
        scan_id=str(desc["scan_id"]),
        version=int(desc["version"]),
        epoch=int(desc.get("epoch", 0)),
        n=int(desc["n"]),
        columns=columns,
        key=str(desc.get("key", "")),
        keep=[mm],
    )


__all__ = ["map_image"]

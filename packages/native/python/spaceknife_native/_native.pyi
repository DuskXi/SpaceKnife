# Contract: docs/native.md. Keep in sync with src/mft and src/hardscan.
import os
from collections.abc import Callable, Sequence
from typing import Any, TypedDict

import numpy as np

__version__: str

_Buffer = bytes | bytearray | memoryview | np.ndarray[Any, np.dtype[np.uint8]]

# --------------------------------------------------------------------------- MFT (src/mft)

def mft_volume_info(volume: str) -> dict[str, int]:
    """Boot-sector geometry and $MFT layout of ``X:`` (administrator rights; Windows only)."""

def read_mft(volume: str) -> bytes:
    """The whole $MFT ``$DATA`` (administrator rights; Windows only). Holds it all in memory."""

def parse_mft(data: _Buffer, record_size: int, cluster_size: int) -> dict[str, Any]:
    """Columns of docs/native.md 2.1 (numpy arrays, ``names`` bytes, ``bad_records`` int)."""

def scan_mft(
    volume: str,
    duty: float = 1.0,
    progress: Callable[[int, int], object] | None = None,
    stats: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, int]]:
    """Read and parse the $MFT window by window: ``(parse_mft columns, mft_volume_info keys)``.

    Several windows are read at once (overlapped, unbuffered reads; how many is found while it
    reads) and parsed in parallel; rows are in record order except files with extension records,
    which come last. ``duty`` in (0, 1]: reads are in flight that share of the time.
    ``progress(done, total)`` (bytes of $MFT) follows every parsed window, on the reading thread;
    its errors are ignored. ``stats``: filled with the scan's timings and counts (benchmarks).
    """

def mft_build(
    table: dict[str, Any],
    root_name: bytes,
    meta_name: bytes,
    orphan_name: bytes,
    ext_ids: Callable[[list[bytes]], list[int]],
    include_ads: bool = False,
    count_links_each: bool = False,
    consume: bool = False,
) -> dict[str, Any]:
    """The node store's columns of a ``parse_mft`` table (``scan.mft.table_to_store``):
    ``{"columns": {name: array}, "names": uint8 \\0name image, "count", "cycles", "orphans"}``.
    ``ext_ids`` numbers the distinct raw extensions (``scan.mft.ext_numbers``); ``consume``
    deletes each table column once the build no longer needs it."""

def _mft_image_info(path: str | os.PathLike[str]) -> dict[str, int]: ...
def _read_mft_image(path: str | os.PathLike[str]) -> bytes: ...
def _scan_mft_image(
    path: str | os.PathLike[str],
    duty: float = 1.0,
    window: int = 0,
    progress: Callable[[int, int], object] | None = None,
    stats: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, int]]: ...
def _parse_mft_stream(
    data: _Buffer,
    record_size: int,
    cluster_size: int,
    window_records: int,
    concurrent: bool = False,
    seed: int = 0,
) -> dict[str, Any]: ...

# --------------------------------------------------------------------------- hard scan (src/hardscan)

class RootOpts(TypedDict, total=False):
    dev: int
    cluster_size: int
    resident_max: int
    cross_mounts: bool
    skip: Sequence[str]
    protect: Sequence[str]
    path: str  # the scan root as given: tasks are checked by their names below it
    verify_names: bool  # Windows: check every task by its stored names (Btrfs roots always are)

ROOT_KEYS: tuple[str, ...]  # the RootOpts keys this build accepts

class SliceOpts(TypedDict, total=False):
    roots: Sequence[RootOpts]
    budget_s: float
    max_entries: int
    threads: int  # 0: min(logical CPUs, 16)
    keep_access_time: bool  # Windows and Linux, default True: listing a folder keeps its last access time
    relative_open: bool  # Windows, default True: found subdirectories are opened from their parent's handle
    # test seams (docs/native.md 2.2), not part of the interface
    _fold_strict: bool
    _extd_self_ids: bool
    _pinned_cap: int | None

class LinkSet:
    """The (dev, ino) of hard-linked files already counted during a scan (Scanner._fold)."""

    def __init__(self) -> None: ...
    def mark(
        self,
        dev: np.ndarray[Any, np.dtype[np.uint64]],
        ino: np.ndarray[Any, np.dtype[np.uint64]],
        nlink: np.ndarray[Any, np.dtype[np.uint16]],
        kind: np.ndarray[Any, np.dtype[np.uint8]],
        flags: np.ndarray[Any, np.dtype[np.uint32]],
    ) -> int:
        """In order: a file with nlink > 1 whose (dev, ino) was counted before gets HARDLINK_DUP in
        ``flags`` (in place); the first one is remembered. Returns how many were marked."""
    def __len__(self) -> int: ...
    def __contains__(self, key: tuple[int, int]) -> bool: ...

def _open_stats(reset: bool = False) -> dict[str, int]:
    """How directories were opened (Windows; empty on POSIX), process-wide since the last reset."""

def scan_slice(tasks: list[tuple[tuple[int, int, int], str, int]], opts: SliceOpts | Any) -> dict[str, Any]:
    """Same fields as spaceknife.scan.worker.SliceResult (numpy arrays, bytes, lists).

    ``opts`` may also be a worker.SliceOptions. Listing runs on a thread pool with the GIL released.
    """

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
OPT_KEYS: tuple[str, ...]  # the option keys of scan_slice this build accepts

class ListingGate:
    """A limit on listings in flight shared by every ``scan_slice`` call that names it, and its
    pause (the backend's disk-load limit, one per disk; docs/native.md 2.2)."""

    def __init__(self, limit: int = 1) -> None: ...
    def set(self, limit: int | None = None, paused: bool | None = None) -> None:
        """Change the limit (at least 1) and/or the pause; calls waiting for a slot look again."""
    @property
    def limit(self) -> int: ...
    @property
    def paused(self) -> bool: ...
    @property
    def inflight(self) -> int:
        """Listings in flight now."""
    def window(self) -> tuple[float, int, bool]:
        """(seconds spent listing, most listings in flight at once, whether a folder waited for the
        gate) since the last call."""

class SliceOpts(TypedDict, total=False):
    roots: Sequence[RootOpts]
    budget_s: float
    max_entries: int
    threads: int  # 0: the shared carriers, within the walker's width; N: a pool of the call's own
    gate: ListingGate  # the disk's limit on listings in flight
    keep_access_time: bool  # Windows and Linux, default True: listing a folder keeps its last access time
    relative_open: bool  # Windows, default True: found subdirectories are opened from their parent's handle
    skip_final: bool  # Windows, default False: no last query if the buffer had room (local NTFS, ReFS, Btrfs)
    dot_check: bool  # Windows, default False: relative opens checked by their listing's "." entry
    keep_where_updated: bool  # Windows, default True: keep_access_time only where access times are updated
    # test seams (docs/native.md 2.2), not part of the interface
    _verify_final: bool
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

class ListingThreads(TypedDict):
    cpus: int  # the CPUs this process may use (cpu_count)
    start: int  # the start width: the scanner's calling threads
    least: int  # the narrowest a warm probe tries
    cold: int  # the width while the disk is read
    max: int  # the widest
    width: int  # now
    threads: int  # the carriers' threads now
    mode: str  # "adaptive", "fixed" (SPACEKNIFE_SCAN_THREADS), "start" (SPACEKNIFE_WALK_ADAPT=0)
    low: int  # the narrowest width since the last reset
    high: int  # the widest

def listing_threads(reset: bool = False) -> ListingThreads:
    """How many folders calls on the shared carriers list at once (docs/native.md 2.2);
    ``reset``: ``low`` / ``high`` start over from the width now, after this reading."""

def cpu_count() -> int:
    """The CPUs this process may use: Windows affinity across processor groups, Linux affinity and
    cgroup v2 quota, else the logical CPUs; at least 1."""

def _open_stats(reset: bool = False) -> dict[str, int]:
    """How directories were opened (Windows; empty on POSIX), process-wide since the last reset."""

class SliceFold:
    """NodeStore.ingest_slices in three steps (docs/native.md 2.3): checks, rows without the GIL,
    then what readers see."""

    def __init__(
        self,
        parts: list[
            tuple[
                np.ndarray[Any, np.dtype[np.int64]],  # refs, flattened
                np.ndarray[Any, np.dtype[np.int64]],  # bounds
                np.ndarray[Any, np.dtype[np.uint8]],  # name bytes
                np.ndarray[Any, np.dtype[np.uint16]],  # name lengths
                dict[str, np.ndarray],  # ENTRY_COLUMNS in the store's types
                np.ndarray[Any, np.dtype[np.uint16]] | None,  # ext ids
            ]
        ],
        n: int,
        kind: np.ndarray,
        flags: np.ndarray,
        child_count: np.ndarray,
        depth: np.ndarray,
    ) -> None:
        """Raises ValueError as NodeStore.ingest_slice does (nothing written then)."""
    @property
    def rows(self) -> int: ...
    @property
    def name_bytes(self) -> int: ...
    @property
    def listings(self) -> int: ...
    def write(self, cols: dict[str, np.ndarray], chunk: np.ndarray, pos: int, base: int) -> None:
        """The new rows and their names (``chunk[pos:pos + name_bytes]`` at offset ``base``), GIL released."""
    def publish(self, store: Any) -> list[np.ndarray]:
        """PENDING, version, row count, child ranges, totals; each part's listing starts (-1: skipped)."""

def scan_slice(tasks: list[tuple[tuple[int, int, int], str, int]], opts: SliceOpts | Any) -> dict[str, Any]:
    """Same fields as spaceknife.scan.worker.SliceResult (numpy arrays, bytes, lists).

    ``opts`` may also be a worker.SliceOptions. Listing runs on a thread pool with the GIL released.
    """

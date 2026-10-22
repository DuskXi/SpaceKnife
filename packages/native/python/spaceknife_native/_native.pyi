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
    volume: str, duty: float = 1.0, progress: Callable[[int, int], object] | None = None
) -> tuple[dict[str, Any], dict[str, int]]:
    """Read and parse the $MFT window by window: ``(parse_mft columns, mft_volume_info keys)``.

    ``duty`` in (0, 1]: the reader pauses so that it is busy that share of the time. Rows are in
    record order except files with extension records, which come last. ``progress(done, total)``
    (bytes of $MFT) follows every parsed window, on the parsing thread; its errors are ignored.
    """

def _mft_image_info(path: str | os.PathLike[str]) -> dict[str, int]: ...
def _read_mft_image(path: str | os.PathLike[str]) -> bytes: ...
def _scan_mft_image(
    path: str | os.PathLike[str],
    duty: float = 1.0,
    window: int = 0,
    progress: Callable[[int, int], object] | None = None,
) -> tuple[dict[str, Any], dict[str, int]]: ...

# --------------------------------------------------------------------------- hard scan (src/hardscan)

class RootOpts(TypedDict, total=False):
    dev: int
    cluster_size: int
    resident_max: int
    cross_mounts: bool

class SliceOpts(TypedDict, total=False):
    roots: Sequence[RootOpts]
    budget_s: float
    max_entries: int
    threads: int  # 0: min(logical CPUs, 16)
    open_by_id: bool  # Windows, default True: open subdirectories found during the call by file id
    keep_access_time: bool  # Windows, default True: listing a folder keeps its last access time

def scan_slice(tasks: list[tuple[tuple[int, int, int], str, int]], opts: SliceOpts | Any) -> dict[str, Any]:
    """Same fields as spaceknife.scan.worker.SliceResult (numpy arrays, bytes, lists).

    ``opts`` may also be a worker.SliceOptions. Listing runs on a thread pool with the GIL released.
    """

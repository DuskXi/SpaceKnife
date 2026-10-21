"""Snapshot files (``.sks``): a complete scan, compressed and columnar.

Layout::

    b"SKSNAP\\x00\\x01"                     8-byte magic + format version
    u32 little-endian header length H
    H bytes: SnapshotHeader (pydantic, JSON)
    data: zstd-compressed blocks (one per node column, then the names)

The names block is one ``\\0name`` run per node in id order (format 2, the store's own layout,
loaded without copying); format 1 stored the names back to back and is still read.

Files are written to a temporary name and atomically renamed, so a crash (or Electron killing
the backend's process tree on Windows) never leaves a half-written snapshot behind.

Every finished scan is saved automatically under ``snapshots_dir()/<root key>/``; retention keeps
the newest :data:`KEEP_RECENT` plus the newest one of each older ISO week.
"""

from __future__ import annotations

import hashlib
import json
import mmap
import os
import struct
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import numpy as np
import zstandard
from pydantic import BaseModel, ConfigDict, Field

from .. import __version__
from ..paths import snapshots_dir
from .nodes import COLUMNS, NodeStore

MAGIC = b"SKSNAP\x00\x01"
FORMAT_VERSION = 2  # 2: NUL-separated names in id order
SUFFIX = ".sks"
KEEP_RECENT = 10
_LEN = struct.Struct("<I")


class BlockInfo(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)

    offset: int = Field(ge=0)
    length: int = Field(ge=0)
    raw_length: int = Field(ge=0)
    dtype: str = ""


class SnapshotHeader(BaseModel):
    model_config = ConfigDict(extra="ignore", frozen=True)

    format: int = FORMAT_VERSION
    app_version: str = __version__
    created_at: datetime
    roots: list[str]
    node_count: int
    volumes: list[dict[str, Any]] = Field(default_factory=list)
    stats: dict[str, Any] = Field(default_factory=dict)
    ext_table: list[str]
    columns: dict[str, BlockInfo]
    names: BlockInfo
    checkpoints: dict[str, Any] = Field(default_factory=dict, description="USN / FSEvents resume points")


class SnapshotError(Exception):
    pass


def root_key(roots: list[str]) -> str:
    """Stable folder name for a set of scan roots."""
    norm = "\n".join(sorted(os.path.normcase(os.path.normpath(r)) for r in roots))
    digest = hashlib.sha1(norm.encode("utf-8", "surrogatepass")).hexdigest()[:12]
    label = "".join(
        ch if ch.isalnum() else "_" for ch in os.path.basename(roots[0].rstrip("\\/")) or roots[0]
    )
    return f"{label.strip('_')[:24] or 'root'}-{digest}"


def save(
    store: NodeStore,
    path: Path,
    *,
    roots: list[str],
    volumes: list[dict[str, Any]] | None = None,
    stats: dict[str, Any] | None = None,
    checkpoints: dict[str, Any] | None = None,
    level: int = 3,
) -> SnapshotHeader:
    compressor = zstandard.ZstdCompressor(level=level, threads=-1)
    blocks: list[bytes] = []
    offset = 0
    col_info: dict[str, BlockInfo] = {}
    for key, arr in store.export_columns().items():
        raw = np.ascontiguousarray(arr)  # compressed straight from the column, no copy
        packed = compressor.compress(memoryview(raw).cast("B"))
        col_info[key] = BlockInfo(
            offset=offset, length=len(packed), raw_length=raw.nbytes, dtype=arr.dtype.str
        )
        blocks.append(packed)
        offset += len(packed)
    # compressed piece by piece: no joined copy of all names
    stream = compressor.compressobj(size=store.names_size())
    pieces = [stream.compress(memoryview(part).cast("B")) for part in store.names_parts()]
    pieces.append(stream.flush())
    names_packed = b"".join(pieces)
    names_info = BlockInfo(offset=offset, length=len(names_packed), raw_length=store.names_size())
    blocks.append(names_packed)
    header = SnapshotHeader(
        created_at=datetime.now(UTC),
        roots=roots,
        node_count=len(store),
        volumes=volumes or [],
        stats=stats or {},
        ext_table=store.ext_table,
        columns=col_info,
        names=names_info,
        checkpoints=checkpoints or {},
    )
    head = _dump_header(header)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "wb") as fh:
        fh.write(MAGIC)
        fh.write(_LEN.pack(len(head)))
        fh.write(head)
        for block in blocks:
            fh.write(block)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
    return header


def _dump_header(header: SnapshotHeader) -> bytes:
    """ASCII JSON, so that text from file names keeps its lone surrogates (as JSON escapes) and reads
    back exactly; strict UTF-8 cannot carry them (an extension cut from a Windows name with an
    unpaired surrogate, or from a POSIX name that is not UTF-8, is such text)."""
    doc = header.model_dump(mode="json")
    return json.dumps(doc, ensure_ascii=True, separators=(",", ":")).encode("ascii")


def read_header(path: Path) -> SnapshotHeader:
    with open(path, "rb") as fh:
        return _read_header(fh.read(len(MAGIC) + _LEN.size), fh)


def _read_header(prefix: bytes, fh: Any) -> SnapshotHeader:
    if len(prefix) < len(MAGIC) + _LEN.size or prefix[: len(MAGIC)] != MAGIC:
        raise SnapshotError("not a SpaceKnife snapshot (bad magic)")
    (hlen,) = _LEN.unpack_from(prefix, len(MAGIC))
    if hlen > 64 << 20:
        raise SnapshotError("snapshot header too large")
    try:
        header = SnapshotHeader.model_validate(json.loads(fh.read(hlen)))
    except ValueError as exc:
        raise SnapshotError(f"invalid snapshot header: {exc}") from exc
    if header.format > FORMAT_VERSION:
        raise SnapshotError(f"snapshot format {header.format} is newer than this build supports")
    return header


def load(path: Path) -> tuple[NodeStore, SnapshotHeader]:
    """The store of a snapshot file. Each block is decompressed straight into the array the store
    then owns (no bytes object and no second copy per column), from a read-only mapping of the
    file: opening a snapshot needs about the store itself, not twice that (docs/memory.md §8)."""
    with open(path, "rb") as fh:
        header = _read_header(fh.read(len(MAGIC) + _LEN.size), fh)
        base = fh.tell()
        size = os.fstat(fh.fileno()).st_size
        mm = mmap.mmap(fh.fileno(), 0, access=mmap.ACCESS_READ) if size > base else None
    try:
        return _load_blocks(memoryview(mm)[base:] if mm is not None else memoryview(b""), header)
    finally:
        if mm is not None:
            mm.close()


def _load_blocks(data: memoryview, header: SnapshotHeader) -> tuple[NodeStore, SnapshotHeader]:
    decompressor = zstandard.ZstdDecompressor()

    def block_into(info: BlockInfo, out: np.ndarray) -> None:
        end = info.offset + info.length
        if end > len(data):
            raise SnapshotError("snapshot is truncated")
        view = memoryview(out).cast("B")
        if len(view) != info.raw_length:
            raise SnapshotError("snapshot block has the wrong length")
        with decompressor.stream_reader(data[info.offset : end]) as reader:
            filled = 0
            while filled < len(view):
                got = reader.readinto(view[filled:])
                if not got:
                    break
                filled += got
            if filled != info.raw_length or reader.read(1):
                raise SnapshotError("snapshot block has the wrong length")
        view.release()

    try:
        columns: dict[str, np.ndarray] = {}
        for key, info in header.columns.items():
            if key not in COLUMNS:
                continue
            dtype = np.dtype(info.dtype)
            if info.raw_length % dtype.itemsize or info.raw_length // dtype.itemsize != header.node_count:
                raise SnapshotError(
                    f"column {key} has {info.raw_length // dtype.itemsize} rows, expected {header.node_count}"
                )
            arr = np.empty(header.node_count, dtype=dtype)
            block_into(info, arr)
            columns[key] = arr
        missing = [k for k in ("parent", "kind", "name_off", "name_len") if k not in columns]
        if missing:
            raise SnapshotError(f"snapshot lacks required columns: {missing}")
        names = np.empty(header.names.raw_length, dtype=np.uint8)
        block_into(header.names, names)
        store = NodeStore.from_columns(columns, names, header.ext_table, separated=header.format >= 2)
    except zstandard.ZstdError as exc:
        raise SnapshotError(f"snapshot block is damaged: {exc}") from exc
    finally:
        data.release()
    return store, header


def auto_path(roots: list[str], when: datetime | None = None, base: Path | None = None) -> Path:
    when = when or datetime.now(UTC)
    folder = (base or snapshots_dir()) / root_key(roots)
    return folder / f"{when.strftime('%Y%m%d-%H%M%S')}{SUFFIX}"


def prune(folder: Path, keep_recent: int = KEEP_RECENT) -> list[Path]:
    """Apply retention to one root's snapshot folder; returns the deleted files."""
    files = sorted(folder.glob(f"*{SUFFIX}"), key=lambda p: p.name, reverse=True)
    keep: set[Path] = set(files[:keep_recent])
    weeks: set[tuple[int, int]] = set()
    for f in files[keep_recent:]:
        try:
            stamp = datetime.strptime(f.stem, "%Y%m%d-%H%M%S")
        except ValueError:
            keep.add(f)
            continue
        week = stamp.isocalendar()[:2]
        if week not in weeks:
            weeks.add(week)
            keep.add(f)
    deleted = []
    for f in files:
        if f not in keep:
            f.unlink(missing_ok=True)
            deleted.append(f)
    return deleted


def list_snapshots(base: Path | None = None) -> list[tuple[Path, SnapshotHeader]]:
    """All snapshots under ``base`` (default: the auto-save folder), newest first."""
    base = base or snapshots_dir()
    out = []
    if base.exists():
        for f in base.glob(f"*/*{SUFFIX}"):
            try:
                out.append((f, read_header(f)))
            except (OSError, SnapshotError):
                continue
    out.sort(key=lambda item: item[1].created_at, reverse=True)
    return out

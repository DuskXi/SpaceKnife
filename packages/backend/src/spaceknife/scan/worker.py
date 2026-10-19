"""Directory-listing worker (runs in spawned processes, or inline for small scans and tests).

One call (:func:`scan_slice`) lists directories depth-first for a small time budget and returns
everything it saw as compact columns, plus the directories it did not get to (the *frontier*)
so the coordinator can hand them to other workers. Nothing here touches the node store.

References tie listings to tree nodes without the worker knowing node ids:
``(0, node_id, 0)`` is a directory the coordinator already knows; ``(1, L, j)`` is entry ``j``
of listing ``L`` from this same slice (always an earlier listing, because the walk is DFS).

Keep this module import-light: every pool process imports it at start-up.
"""

from __future__ import annotations

import os
import posixpath
import stat
import sys
import time
from dataclasses import dataclass, field
from typing import Any

import numpy as np

KIND_FILE, KIND_DIR, KIND_LINK, KIND_OTHER = 0, 1, 2, 3

# Mirrors spaceknife.store.nodes.Flag (kept literal so workers don't import the store).
F_MOUNT = 1 << 1
F_COMPRESSED = 1 << 3
F_SPARSE = 1 << 4
F_HIDDEN = 1 << 5
F_SYSTEM = 1 << 6
F_READONLY = 1 << 7
F_REPARSE = 1 << 8
F_CLOUD = 1 << 9
F_ERROR = 1 << 10
F_ARCHIVE = 1 << 12
F_ENCRYPTED = 1 << 13
F_TEMPORARY = 1 << 14
F_NOT_INDEXED = 1 << 15
F_OFFLINE = 1 << 16

IS_WINDOWS = sys.platform == "win32"
NAME_ERRORS = "surrogatepass" if IS_WINDOWS else "surrogateescape"
U32_MAX = 2**32 - 1

# Windows file attributes
_FA_READONLY = 0x1
_FA_HIDDEN = 0x2
_FA_SYSTEM = 0x4
_FA_DIRECTORY = 0x10
_FA_ARCHIVE = 0x20
_FA_TEMPORARY = 0x100
_FA_SPARSE = 0x200
_FA_REPARSE = 0x400
_FA_COMPRESSED = 0x800
_FA_OFFLINE = 0x1000
_FA_NOT_INDEXED = 0x2000
_FA_ENCRYPTED = 0x4000
_FA_RECALL_ON_OPEN = 0x40000
_FA_RECALL_ON_DATA_ACCESS = 0x400000
_REPARSE_NAME_SURROGATE = 0x20000000  # symlinks, junctions, mount points: never descend

_ATTR_FLAGS = (
    (_FA_READONLY, F_READONLY),
    (_FA_HIDDEN, F_HIDDEN),
    (_FA_SYSTEM, F_SYSTEM),
    (_FA_ARCHIVE, F_ARCHIVE),
    (_FA_TEMPORARY, F_TEMPORARY),
    (_FA_SPARSE, F_SPARSE),
    (_FA_REPARSE, F_REPARSE),
    (_FA_COMPRESSED, F_COMPRESSED),
    (_FA_OFFLINE, F_OFFLINE),
    (_FA_NOT_INDEXED, F_NOT_INDEXED),
    (_FA_ENCRYPTED, F_ENCRYPTED),
)

Ref = tuple[int, int, int]
Task = tuple[Ref, str, int]  # (ref, absolute directory path, root index)


@dataclass(frozen=True, slots=True)
class RootOptions:
    dev: int = 0  # st_dev of the root (POSIX): directories on another device are not crossed
    cluster_size: int = 4096  # Windows hard scan: allocation = size rounded up to clusters
    resident_max: int = 0  # NTFS: files this small live inside the MFT record (0 clusters)
    cross_mounts: bool = False
    # folders not to enter, as mount points (macOS: the other half of the root's APFS volume
    # group, already in the tree through the firmlinks; scan.volume.volume_group_twins)
    skip: tuple[str, ...] = ()


@dataclass(frozen=True, slots=True)
class SliceOptions:
    roots: tuple[RootOptions, ...]
    budget_s: float = 0.03
    max_entries: int = 100_000


@dataclass(slots=True)
class SliceResult:
    refs: np.ndarray  # (L, 3) int64
    paths: list[str]  # directory path of each listing
    bounds: np.ndarray  # (L + 1,) int64 offsets into the entry columns
    name_blob: bytes
    name_lens: np.ndarray  # uint16 per entry
    cols: dict[str, np.ndarray]  # spaceknife.store.nodes.ENTRY_COLUMNS + "dev"
    ext_table: list[str]
    ext_ids: np.ndarray  # uint16 per entry, ids into ext_table
    frontier: list[Task]
    errors: list[tuple[Ref, str, str]]
    elapsed: float = 0.0


@dataclass(slots=True)
class _Acc:
    refs: list[Ref] = field(default_factory=list)
    paths: list[str] = field(default_factory=list)
    bounds: list[int] = field(default_factory=lambda: [0])
    names: list[bytes] = field(default_factory=list)
    kind: list[int] = field(default_factory=list)
    flags: list[int] = field(default_factory=list)
    size: list[int] = field(default_factory=list)
    alloc: list[int] = field(default_factory=list)
    mtime: list[int] = field(default_factory=list)
    ctime: list[int] = field(default_factory=list)
    atime: list[int] = field(default_factory=list)
    ino: list[int] = field(default_factory=list)
    nlink: list[int] = field(default_factory=list)
    dev: list[int] = field(default_factory=list)
    ext: list[int] = field(default_factory=list)
    ext_table: list[str] = field(default_factory=lambda: [""])
    ext_index: dict[str, int] = field(default_factory=lambda: {"": 0})

    def ext_id(self, name: str) -> int:
        dot = name.rfind(".")
        if dot <= 0 or len(name) - dot > 17:
            return 0
        ext = name[dot + 1 :].lower()
        idx = self.ext_index.get(ext)
        if idx is None:
            idx = len(self.ext_table)
            self.ext_table.append(ext)
            self.ext_index[ext] = idx
        return idx


def _secs(t: float) -> int:
    if t <= 0:
        return 0
    return U32_MAX if t >= U32_MAX else int(t)


def _long_path(path: str) -> str:
    if not IS_WINDOWS or len(path) < 240 or path.startswith("\\\\?\\"):
        return path
    if path.startswith("\\\\"):
        return "\\\\?\\UNC\\" + path[2:]
    return "\\\\?\\" + path


_get_compressed_size: Any = None


def _compressed_size(path: str) -> int | None:
    """Actual allocation of a compressed/sparse file (Windows), or None if unavailable."""
    assert sys.platform == "win32"
    global _get_compressed_size
    if _get_compressed_size is None:
        import ctypes
        from ctypes import wintypes

        fn = ctypes.WinDLL("kernel32", use_last_error=True).GetCompressedFileSizeW  # type: ignore[attr-defined,unused-ignore]
        fn.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.DWORD)]
        fn.restype = wintypes.DWORD
        _get_compressed_size = (fn, ctypes, wintypes)
    fn, ct, wt = _get_compressed_size
    high = wt.DWORD(0)
    low = fn(_long_path(path), ct.byref(high))
    if low == 0xFFFFFFFF and ct.get_last_error() not in (0,):
        return None
    return int((high.value << 32) | low)


def _list_windows(path: str, root: RootOptions, acc: _Acc) -> list[tuple[int, str]]:
    subdirs: list[tuple[int, str]] = []
    cluster = max(1, root.cluster_size)
    base = len(acc.kind)
    with os.scandir(_long_path(path)) as it:
        for entry in it:
            name = entry.name
            flags = 0
            try:
                st: Any = entry.stat(follow_symlinks=False)
            except OSError:
                acc.names.append(name.encode("utf-8", NAME_ERRORS))
                acc.kind.append(KIND_OTHER)
                acc.flags.append(F_ERROR)
                acc.size.append(0)
                acc.alloc.append(0)
                acc.mtime.append(0)
                acc.ctime.append(0)
                acc.atime.append(0)
                acc.ino.append(0)
                acc.nlink.append(1)
                acc.dev.append(0)
                acc.ext.append(0)
                continue
            attrs = st.st_file_attributes
            for bit, flag in _ATTR_FLAGS:
                if attrs & bit:
                    flags |= flag
            is_dir = bool(attrs & _FA_DIRECTORY)
            if attrs & _FA_REPARSE and st.st_reparse_tag & _REPARSE_NAME_SURROGATE:
                kind = KIND_LINK
            elif is_dir:
                kind = KIND_DIR
            else:
                kind = KIND_FILE
            size = 0 if kind == KIND_DIR else st.st_size
            if kind != KIND_FILE:
                alloc = 0
            elif attrs & (_FA_RECALL_ON_DATA_ACCESS | _FA_RECALL_ON_OPEN):
                flags |= F_CLOUD
                alloc = 0
            elif attrs & (_FA_COMPRESSED | _FA_SPARSE):
                real = _compressed_size(os.path.join(path, name))
                alloc = -(-(real if real is not None else size) // cluster) * cluster
            elif size <= root.resident_max:
                alloc = 0
            else:
                alloc = -(-size // cluster) * cluster
            acc.names.append(name.encode("utf-8", NAME_ERRORS))
            acc.kind.append(kind)
            acc.flags.append(flags)
            acc.size.append(size)
            acc.alloc.append(alloc)
            acc.mtime.append(_secs(st.st_mtime))
            acc.ctime.append(_secs(getattr(st, "st_birthtime", st.st_ctime)))
            acc.atime.append(_secs(st.st_atime))
            acc.ino.append(0)
            acc.nlink.append(1)
            acc.dev.append(0)
            acc.ext.append(acc.ext_id(name) if kind == KIND_FILE else 0)
            if kind == KIND_DIR:
                subdirs.append((len(acc.kind) - 1 - base, os.path.join(path, name)))
    return subdirs


def _list_posix(path: str, root: RootOptions, acc: _Acc) -> list[tuple[int, str]]:
    assert sys.platform != "win32"
    subdirs: list[tuple[int, str]] = []
    base = len(acc.kind)
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0))
    try:
        with os.scandir(fd) as it:
            for entry in it:
                name = entry.name
                raw = os.fsencode(name)
                flags = 0
                try:
                    st: Any = entry.stat(follow_symlinks=False)
                except OSError:
                    acc.names.append(raw)
                    acc.kind.append(KIND_OTHER)
                    acc.flags.append(F_ERROR)
                    for col in (
                        acc.size,
                        acc.alloc,
                        acc.mtime,
                        acc.ctime,
                        acc.atime,
                        acc.ino,
                        acc.dev,
                        acc.ext,
                    ):
                        col.append(0)
                    acc.nlink.append(1)
                    continue
                mode = st.st_mode
                descend = False
                if stat.S_ISDIR(mode):
                    kind = KIND_DIR
                    if (st.st_dev != root.dev and not root.cross_mounts) or (
                        root.skip and posixpath.join(path, name) in root.skip
                    ):
                        flags |= F_MOUNT
                    else:
                        descend = True
                elif stat.S_ISLNK(mode):
                    kind = KIND_LINK
                elif stat.S_ISREG(mode):
                    kind = KIND_FILE
                else:
                    kind = KIND_OTHER
                if name.startswith("."):
                    flags |= F_HIDDEN
                acc.names.append(raw)
                acc.kind.append(kind)
                acc.flags.append(flags)
                acc.size.append(0 if kind == KIND_DIR else st.st_size)
                acc.alloc.append(st.st_blocks * 512)
                acc.mtime.append(_secs(st.st_mtime))
                acc.ctime.append(_secs(getattr(st, "st_birthtime", 0.0)))
                acc.atime.append(_secs(st.st_atime))
                acc.ino.append(st.st_ino)
                acc.nlink.append(min(st.st_nlink, 65535))
                acc.dev.append(st.st_dev)
                acc.ext.append(acc.ext_id(name) if kind == KIND_FILE else 0)
                if descend:
                    subdirs.append((len(acc.kind) - 1 - base, os.path.join(path, name)))
    finally:
        os.close(fd)
    return subdirs


_list_dir = _list_windows if IS_WINDOWS else _list_posix


def scan_slice(tasks: list[Task], opts: SliceOptions) -> SliceResult:
    """List directories depth-first, starting with ``tasks``, until the budget is spent."""
    started = time.perf_counter()
    deadline = started + opts.budget_s
    acc = _Acc()
    errors: list[tuple[Ref, str, str]] = []
    stack: list[Task] = list(reversed(tasks))
    while stack:
        if acc.refs and (time.perf_counter() >= deadline or len(acc.kind) >= opts.max_entries):
            break
        ref, path, root_idx = stack.pop()
        listing = len(acc.refs)
        try:
            subdirs = _list_dir(path, opts.roots[root_idx], acc)
        except OSError as exc:
            # Roll back entries appended before the failure (e.g. scandir died mid-way).
            del acc.names[acc.bounds[-1] :]
            for col in (acc.kind, acc.flags, acc.size, acc.alloc, acc.mtime, acc.ctime, acc.atime):
                del col[acc.bounds[-1] :]
            for col in (acc.ino, acc.nlink, acc.dev, acc.ext):
                del col[acc.bounds[-1] :]
            errors.append((ref, path, exc.strerror or type(exc).__name__))
            continue
        acc.refs.append(ref)
        acc.paths.append(path)
        acc.bounds.append(len(acc.kind))
        for j, sub_path in reversed(subdirs):
            stack.append(((1, listing, j), sub_path, root_idx))
    frontier = list(reversed(stack))
    count = len(acc.kind)
    cols: dict[str, np.ndarray] = {
        "kind": np.array(acc.kind, dtype=np.uint8),
        "flags": np.array(acc.flags, dtype=np.uint32),
        "size": np.array(acc.size, dtype=np.int64),
        "alloc": np.array(acc.alloc, dtype=np.int64),
        "mtime": np.array(acc.mtime, dtype=np.uint32),
        "ctime": np.array(acc.ctime, dtype=np.uint32),
        "atime": np.array(acc.atime, dtype=np.uint32),
        "ino": np.array(acc.ino, dtype=np.uint64),
        "nlink": np.array(acc.nlink, dtype=np.uint16),
        "dev": np.array(acc.dev, dtype=np.uint64),
    }
    return SliceResult(
        refs=np.array(acc.refs, dtype=np.int64).reshape(-1, 3),
        paths=acc.paths,
        bounds=np.array(acc.bounds, dtype=np.int64),
        name_blob=b"".join(acc.names),
        name_lens=np.fromiter((len(n) for n in acc.names), dtype=np.uint16, count=count),
        cols=cols,
        ext_table=acc.ext_table,
        ext_ids=np.array(acc.ext, dtype=np.uint16),
        frontier=frontier,
        errors=errors,
        elapsed=time.perf_counter() - started,
    )


def warm_up() -> int:
    """Pool initializer target: import everything once so the first real slice is fast."""
    return os.getpid()

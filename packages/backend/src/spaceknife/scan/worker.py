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

import errno
import os
import posixpath
import stat
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from functools import partial
from typing import Any

import numpy as np

KIND_FILE, KIND_DIR, KIND_LINK, KIND_OTHER = 0, 1, 2, 3

# Mirrors spaceknife.store.nodes.Flag (kept literal so workers don't import the store).
F_ACCESS_DENIED = 1 << 0
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
SF_DATALESS = 0x40000000  # macOS st_flags: contents in the cloud (iCloud Drive, File Provider)
DARWIN = sys.platform == "darwin"

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

# Listing errors of folders their name does not reach (Windows: a case-only sibling's name opens
# the other entry, docs/native.md 2.2). The native walker has the same texts (hardscan/case.rs);
# events.denied_reason gives them their wording in the log. The worker cannot open by file id, so
# such folders are errors here (CAP and INCOMPLETE are the native walker's only).
CASE_ERROR = (
    "cannot be opened by its name: another entry in this folder has the same name in a different case"
)
DUP_ERROR = "cannot be opened by its name: another entry in this folder has the same name as Windows shows it"
BADNAME_ERROR = "cannot be opened by its name: it contains a character Windows names cannot hold"
CAP_ERROR = "not read: too many entries in one pass in folders that can only be opened by file id"
INCOMPLETE_ERROR = "listed incompletely when opened by its file id"


@dataclass(frozen=True, slots=True)
class RootOptions:
    dev: int = 0  # st_dev of the root (POSIX): directories on another device are not crossed
    cluster_size: int = 4096  # Windows hard scan: allocation = size rounded up to clusters
    resident_max: int = 0  # NTFS: files this small live inside the MFT record (0 clusters)
    cross_mounts: bool = False
    # folders not to enter, as mount points (macOS: the other half of the root's APFS volume
    # group, already in the tree through the firmlinks; scan.volume.volume_group_twins)
    skip: tuple[str, ...] = ()
    # folders not to enter, as not accessible (macOS: places that would ask for consent one by
    # one without Full Disk Access, when the user chose to leave them out; spaceknife.privacy)
    protect: tuple[str, ...] = ()
    path: str = ""  # the scan root as given (tasks are checked by their names below it)
    # Windows: names fold onto case-sensitive storage here (WinBtrfs), so an open by name can reach
    # a case-only sibling: subfolders sharing a name and every task are checked by the names the
    # file system stores (`_unreachable`)
    verify_names: bool = False


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
    elapsed: float = 0.0  # seconds spent listing (not waiting to start, nor handing the result over)


def _shift(ref: Ref | np.ndarray, listings: int) -> Ref:
    kind, j, i = (int(x) for x in ref)
    return (kind, j, i) if kind == 0 else (kind, j + listings, i)


def merge_slices(results: list[SliceResult]) -> SliceResult:
    """Several slices as one, their listings one after another: the store folds them in one call
    and pays its per-call costs once. Listings of different slices never name each other, so only
    the references inside each slice (``(1, listing, entry)``) move. Extensions get one table."""
    if len(results) == 1:
        return results[0]
    refs: list[np.ndarray] = []
    bounds: list[np.ndarray] = [np.zeros(1, dtype=np.int64)]
    ext_ids: list[np.ndarray] = []
    frontier: list[Task] = []
    errors: list[tuple[Ref, str, str]] = []
    table: dict[str, int] = {}
    listings = entries = 0
    for r in results:
        rr = np.array(r.refs, dtype=np.int64).reshape(-1, 3)
        rr[rr[:, 0] != 0, 1] += listings
        refs.append(rr)
        bounds.append(np.asarray(r.bounds, dtype=np.int64)[1:] + entries)
        remap = np.array([table.setdefault(e, len(table)) for e in r.ext_table], dtype=np.uint16)
        ext_ids.append(remap[np.asarray(r.ext_ids, dtype=np.intp)])
        if listings:
            frontier += [(_shift(ref, listings), path, root) for ref, path, root in r.frontier]
            errors += [(_shift(ref, listings), path, msg) for ref, path, msg in r.errors]
        else:
            frontier += r.frontier
            errors += r.errors
        listings += len(rr)
        entries += len(r.name_lens)
    return SliceResult(
        refs=np.concatenate(refs),
        paths=[p for r in results for p in r.paths],
        bounds=np.concatenate(bounds),
        name_blob=b"".join(r.name_blob for r in results),
        name_lens=np.concatenate([r.name_lens for r in results]),
        cols={key: np.concatenate([r.cols[key] for r in results]) for key in results[0].cols},
        ext_table=list(table),
        ext_ids=np.concatenate(ext_ids),
        frontier=frontier,
        errors=errors,
        elapsed=sum(r.elapsed for r in results),
    )


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
    """``path`` as Win32 calls must get it: with the ``\\\\?\\`` prefix when it is long, or when a
    name in it ends in a dot or space (Win32 trims those: the folder ``odd.`` would list ``odd``)."""
    if not IS_WINDOWS or path.startswith("\\\\?\\"):
        return path
    if len(path) < 240 and not (".\\" in path or " \\" in path or path.endswith((".", " "))):
        return path
    if path.startswith("\\\\"):
        return "\\\\?\\UNC\\" + path[2:]
    return "\\\\?\\" + path


_final_name_fns: Any = None


def final_name(path: str) -> str | None:
    """The stored name of what ``path`` opens (Windows: its path below the volume's root as the
    file system stores it, e.g. ``\\usr\\share\\terminfo\\A``; normalised, FileNormalizedNameInfo,
    where the file system has it: NTFS gives long names for 8.3 short names), or None when it
    cannot be read, and on other systems. Opened with no access rights and without following a
    reparse point at the end, as the open by name the listing does would."""
    if not IS_WINDOWS:
        return None
    global _final_name_fns
    if _final_name_fns is None:
        import ctypes
        from ctypes import wintypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
        create = k32.CreateFileW
        create.argtypes = [
            wintypes.LPCWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            ctypes.c_void_p,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.HANDLE,
        ]
        create.restype = wintypes.HANDLE
        info = k32.GetFileInformationByHandleEx
        info.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
        info.restype = wintypes.BOOL
        close = k32.CloseHandle
        close.argtypes = [wintypes.HANDLE]
        _final_name_fns = (create, info, close, ctypes)
    create, info, close, ct = _final_name_fns
    share_all, open_existing = 7, 3
    backup_semantics, open_reparse_point = 0x02000000, 0x00200000
    file_normalized_name_info, file_name_info = 24, 2
    h = create(
        _long_path(path), 0, share_all, None, open_existing, backup_semantics | open_reparse_point, None
    )
    if h is None or h == ct.c_void_p(-1).value:
        return None
    try:
        # exactly 64 KiB (room for any name of an NT path): WinBtrfs refuses a buffer of a few
        # bytes more with ERROR_MORE_DATA (measured 2026-10-22)
        buf = ct.create_string_buffer(65536)
        if not info(h, file_normalized_name_info, buf, len(buf)) and not info(
            h, file_name_info, buf, len(buf)
        ):
            return None
        n = int.from_bytes(buf.raw[:4], "little")
        return str(buf.raw[4 : 4 + n].decode("utf-16-le", "surrogatepass"))
    finally:
        close(h)


def names_below(path: str, root: str) -> list[str] | None:
    """The names of ``path`` below the scan root ``root`` as given (the root part of a stored name
    is the root's own stored name, ``task_is``: a root typed in another case, a SUBST drive, a
    mount folder), or None for the root itself, a path not below it, or no root (hardscan/case.rs
    ``below_root``)."""
    if not root or not path.startswith(root):
        return None
    rest = path[len(root) :]
    if not (root.endswith(("\\", "/")) or rest.startswith(("\\", "/"))):
        return None  # C:\data2 is not below C:\data
    below = [n for n in rest.replace("/", "\\").split("\\") if n]
    return below or None


def _names(path: str) -> list[str]:
    return [n for n in path.replace("/", "\\").split("\\") if n]


def task_is(stored: str, root: str | None, below: list[str]) -> bool:
    """Whether a folder is stored under the names its path gives it below the scan root, exactly:
    its stored name is the root's own (``root``) followed by ``below``. Without the root's, its last
    names must be ``below``; a stored name with fewer names than that was reached some other way
    (through a symlink that won the name). hardscan/case.rs ``task_is``."""
    names = _names(stored)
    if root is not None:
        return names == _names(root) + below
    return len(names) >= len(below) and names[len(names) - len(below) :] == below


def case_marks(names: list[str], dirs: list[int]) -> dict[int, str]:
    """How the subfolders ``dirs`` (indexes into ``names``, one listing's entry names) must be
    opened, on roots that ``verify_names``: ``strict`` when another entry has the same name under
    ``str.upper()`` (it groups every pair the Windows upcase does, and a few more: checked by
    test; the stored name decides), ``dup`` for a subfolder after the first entry of the very same
    name (WinBtrfs shows invalid UTF-8 as U+FFFD), ``bad`` for a name no Windows path can hold
    (:func:`unnameable`). Unmarked subfolders are not in the result."""
    groups: dict[str, list[int]] = {}
    for j, name in enumerate(names):
        groups.setdefault(name.upper(), []).append(j)
    is_dir = set(dirs)
    marks: dict[int, str] = {}
    for group in groups.values():
        if len(group) < 2:
            continue
        seen: set[str] = set()
        for j in group:
            if j in is_dir:
                marks[j] = "dup" if names[j] in seen else "strict"
            seen.add(names[j])
    for j in dirs:
        if unnameable(names[j]):
            marks[j] = "bad"
    return marks


_UNNAMEABLE = frozenset('\\/:*?"<>|') | frozenset(map(chr, range(1, 32)))


def unnameable(name: str) -> bool:
    """A name no Windows path can hold (hardscan/case.rs ``unnameable``): a separator, the stream
    separator ``:``, a wildcard or DOS wildcard (``* ? " < >``), ``|`` or a control character.
    WinBtrfs lists Linux names that have them as they are."""
    return any(c in _UNNAMEABLE for c in name)


_NO_NAME = {"dup": DUP_ERROR, "bad": BADNAME_ERROR}


def _unreachable(path: str, root: RootOptions, mark: str, root_name: Callable[[], str | None]) -> str | None:
    """Why the folder ``path`` names cannot be listed by that path (roots that ``verify_names``):
    an exact duplicate's or a ``\\`` name's never can; a task, or a subfolder whose name another
    entry shares, only when the file system stores it under those names below the root (else the
    open reached a case-only sibling, something below one, or what a sibling that is a symlink
    points to). ``root_name``: the root's own stored name (read once per slice). Checked when the
    folder's turn comes, not at its parent's listing: a relist of the parent alone (the monitor's)
    stays free of such errors, and every task that comes back from the frontier is checked again.
    When the stored name cannot be read, the folder is listed as before and the listing gives any
    error."""
    if mark in _NO_NAME:
        return _NO_NAME[mark]
    if mark in ("strict", "task") and (below := names_below(path, root.path)) is not None:
        stored = final_name(path)
        if stored is not None and not task_is(stored, root_name(), below):
            return CASE_ERROR
    return None


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


_ERROR_FILENAME_EXCED_RANGE = 206


def _too_long(path: str) -> OSError:
    """The error a Win32 call gives a path past 32,767 units (os.scandir raises ValueError before
    calling one); its strerror is CPython's for that code, as the native walker's win_message."""
    assert sys.platform == "win32"
    import ctypes

    text = ctypes.FormatError(_ERROR_FILENAME_EXCED_RANGE)  # type: ignore[attr-defined,unused-ignore]
    while text and (text[-1] <= " " or text[-1] == "."):  # trimmed like OSError.strerror
        text = text[:-1]
    return OSError(0, text, path, _ERROR_FILENAME_EXCED_RANGE)


def _list_windows(path: str, root: RootOptions, acc: _Acc) -> list[tuple[int, str, str]]:
    """Lists ``path`` into ``acc``; returns the subfolders to descend as ``(j, path, mark)`` (marks:
    ``case_marks``, only on roots that ``verify_names``)."""
    subdirs: list[tuple[int, str, str]] = []
    names: list[str] = []  # the listing's names, to find the ones that share a name
    cluster = max(1, root.cluster_size)
    base = len(acc.kind)
    try:
        listing = os.scandir(_long_path(path))
    except ValueError:  # too long for Windows (a folder renamed longer above a deep tree)
        raise _too_long(path) from None
    with listing as it:
        for entry in it:
            name = entry.name
            if root.verify_names:
                names.append(name)
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
                subdirs.append((len(acc.kind) - 1 - base, os.path.join(path, name), ""))
    if root.verify_names and subdirs and len(names) >= 2:
        marks = case_marks(names, [j for j, _, _ in subdirs])
        if marks:
            subdirs = [(j, p, marks.get(j, "")) for j, p, _ in subdirs]
    return subdirs


_PIECE = 1000  # bytes of path per open in _open_dir: well under PATH_MAX everywhere (macOS: 1,024)


def _open_dir(path: str, flags: int) -> int:
    """``os.open(path, flags)``, also for a path of PATH_MAX bytes or more, which open() refuses
    (ENAMETOOLONG) although the folder is there to list (posix.rs `open_long`): opened a piece of
    whole names at a time, each relative to the folder the one before reached. Resolution is that
    of one open of the whole path: symlinks in the middle are followed, ``flags`` apply to the last
    name only, and the folders on the way need search permission only (O_PATH, Linux)."""
    try:
        return os.open(path, flags)
    except OSError as exc:
        if exc.errno != errno.ENAMETOOLONG:
            raise
    middle = getattr(os, "O_PATH", os.O_RDONLY) | os.O_DIRECTORY | os.O_CLOEXEC
    rest = os.fsencode(path)
    fd: int | None = None
    try:
        while True:
            if len(rest) <= _PIECE:
                piece, tail = rest, b""
            else:
                cut = rest.rfind(b"/", 1, _PIECE + 1)  # a leading "/" stays with the first piece
                if cut < 0:
                    raise OSError(errno.ENAMETOOLONG, os.strerror(errno.ENAMETOOLONG), path)
                piece, tail = rest[:cut], rest[cut + 1 :]
            tail = tail.lstrip(b"/")
            opened = os.open(piece, flags if not tail else middle, dir_fd=fd)
            if fd is not None:
                os.close(fd)
            fd = opened
            if not tail:
                return opened
            rest = tail
    except BaseException:
        if fd is not None:
            os.close(fd)
        raise


def _list_posix(path: str, root: RootOptions, acc: _Acc) -> list[tuple[int, str, str]]:
    assert sys.platform != "win32"
    subdirs: list[tuple[int, str, str]] = []
    base = len(acc.kind)
    fd = _open_dir(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0))
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
                if DARWIN and getattr(st, "st_flags", 0) & SF_DATALESS:
                    flags |= (
                        F_CLOUD  # takes no local space; a folder is not entered: listing would download it
                    )
                if stat.S_ISDIR(mode):
                    kind = KIND_DIR
                    if (st.st_dev != root.dev and not root.cross_mounts) or (
                        root.skip and posixpath.join(path, name) in root.skip
                    ):
                        flags |= F_MOUNT
                    elif root.protect and posixpath.join(path, name) in root.protect:
                        flags |= F_ACCESS_DENIED
                    else:
                        descend = not flags & F_CLOUD
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
                    subdirs.append((len(acc.kind) - 1 - base, os.path.join(path, name), ""))
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
    # (ref, path, root index, mark): tasks are "task", subfolders as case_marks marked them
    stack: list[tuple[Ref, str, int, str]] = [
        (ref, path, root, "task") for ref, path, root in reversed(tasks)
    ]
    root_names: dict[int, str | None] = {}  # each root's own stored name, once it is needed

    def root_name(i: int) -> str | None:
        if i not in root_names:
            root_names[i] = final_name(opts.roots[i].path)
        return root_names[i]

    while stack:
        if acc.refs and (time.perf_counter() >= deadline or len(acc.kind) >= opts.max_entries):
            break
        ref, path, root_idx, mark = stack.pop()
        root = opts.roots[root_idx]
        why = _unreachable(path, root, mark, partial(root_name, root_idx)) if root.verify_names else None
        if why is not None:
            errors.append((ref, path, why))
            continue
        listing = len(acc.refs)
        try:
            subdirs = _list_dir(path, root, acc)
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
        for j, sub_path, sub_mark in reversed(subdirs):
            stack.append(((1, listing, j), sub_path, root_idx, sub_mark))
    frontier: list[Task] = []
    for ref, path, root_idx, mark in reversed(stack):
        if mark in _NO_NAME:  # no path reaches it: never a task
            errors.append((ref, path, _NO_NAME[mark]))
        else:
            frontier.append((ref, path, root_idx))
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
    """Pool initializer target: import everything once so the first real slice is fast; on macOS
    the worker process never downloads cloud files either (io.priority.no_cloud_downloads)."""
    from ..io.priority import no_cloud_downloads

    no_cloud_downloads()
    return os.getpid()

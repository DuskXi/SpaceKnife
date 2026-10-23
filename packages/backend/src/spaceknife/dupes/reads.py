"""The IO of the duplicate-file stages, native or in Python (docs/dupes.md §12).

:class:`Ops` is what the engine calls, batch by batch, on a disk queue's thread:

- ``identity(paths)`` (S1, metadata only), ``extents(paths)`` (S1b, block maps, may be None);
- ``reader(...)``: an object with ``probe`` (S2), ``step`` (S3), ``suspend``, ``drop``,
  ``open_handles``, ``members`` and ``counters``.

The native implementation (``spaceknife_native``: BLAKE3, unbuffered reads, Windows attribute
handles, USN, block maps) is used when it is built; :class:`FallbackOps` does the same in Python
(BLAKE2b, buffered reads, no block maps, no USN). The two never share digests: every cache entry
and every job records its algorithm (codes.ALG_*).
"""

from __future__ import annotations

import contextlib
import errno
import hashlib
import os
import stat
import sys
import threading
import time
from typing import Any, Protocol

import numpy as np

from .. import native
from . import codes as C

ID_FIELDS: tuple[tuple[str, str], ...] = (
    ("reason", "u1"),
    ("dev", "<u8"),
    ("fid_lo", "<u8"),
    ("fid_hi", "<u8"),
    ("nlink", "<u4"),
    ("size", "<i8"),
    ("alloc", "<i8"),
    ("mtime", "<i8"),
    ("change", "<i8"),
    ("atime", "<i8"),
    ("btime", "<i8"),
    ("attrs", "<u4"),
    ("tag", "<u4"),
    ("usn", "<i8"),
    ("case_ok", "u1"),
)

_MASK64 = (1 << 64) - 1
_WIN_RECALL = 0x0004_0000 | 0x0040_0000 | 0x0000_1000
_WIN_DIR = 0x10
_WIN_REPARSE = 0x400
_SF_DATALESS = 0x4000_0000


class Reader(Protocol):
    def probe(
        self,
        keys: np.ndarray,
        paths: list[str],
        dev: np.ndarray,
        fid_lo: np.ndarray,
        fid_hi: np.ndarray,
        size: np.ndarray,
        attrs: np.ndarray,
        hand_off: bool,
    ) -> dict[str, Any]: ...

    def step(self, keys: np.ndarray, until: np.ndarray, max_bytes: int, max_s: float) -> dict[str, Any]: ...

    def suspend(self, keys: list[int] | None = None) -> None: ...

    def drop(self, keys: list[int] | None = None) -> None: ...

    def open_handles(self) -> int: ...

    def members(self) -> int: ...

    def counters(self) -> dict[str, int]: ...


class Ops(Protocol):
    alg: int
    name: str
    has_usn: bool

    def identity(self, paths: list[str], want_usn: bool = False) -> dict[str, np.ndarray]: ...

    def extents(self, paths: list[str]) -> dict[str, Any] | None: ...

    def reader(
        self, *, unbuffered: bool, keep_atime: str, small_whole: int, lease_s: float, chunk: int
    ) -> Reader: ...


class NativeOps:
    alg = C.ALG_BLAKE3
    name = "native"
    has_usn = sys.platform == "win32"

    def __init__(self) -> None:
        m = native.module()
        assert m is not None
        self._m: Any = m

    def identity(self, paths: list[str], want_usn: bool = False) -> dict[str, np.ndarray]:
        out: dict[str, np.ndarray] = self._m.dupes_identity(paths, want_usn)
        return out

    def extents(self, paths: list[str]) -> dict[str, Any] | None:
        out: dict[str, Any] = self._m.dupes_extents(paths)
        return out

    def reader(
        self, *, unbuffered: bool, keep_atime: str, small_whole: int, lease_s: float, chunk: int
    ) -> Reader:
        r: Reader = self._m.DupesReader(
            unbuffered=unbuffered,
            keep_atime=keep_atime,
            small_whole=small_whole,
            lease_s=lease_s,
            chunk=chunk,
        )
        return r


def _empty_identity(n: int) -> dict[str, np.ndarray]:
    return {k: np.zeros(n, dtype=dt) for k, dt in ID_FIELDS}


def _reason_of(exc: OSError) -> int:
    if isinstance(exc, FileNotFoundError | NotADirectoryError):
        return C.GONE
    if isinstance(exc, PermissionError):
        return C.DENIED
    if exc.errno == errno.ELOOP:
        return C.SURROGATE
    if exc.errno == errno.EISDIR:
        return C.NOT_FILE
    winerror = getattr(exc, "winerror", None)
    if winerror in (32, 33):
        return C.IN_USE
    if winerror == 1920 or (isinstance(winerror, int) and 362 <= winerror <= 404):
        return C.CLOUD
    return C.READ_ERROR


def _stat_row(out: dict[str, np.ndarray], i: int, st: os.stat_result) -> None:
    ino = int(st.st_ino)
    out["dev"][i] = int(st.st_dev) & _MASK64
    out["fid_lo"][i] = ino & _MASK64
    out["fid_hi"][i] = (ino >> 64) & _MASK64
    out["nlink"][i] = min(int(st.st_nlink), 2**32 - 1)
    out["size"][i] = st.st_size
    blocks = getattr(st, "st_blocks", None)
    out["alloc"][i] = blocks * 512 if blocks is not None else st.st_size
    out["mtime"][i] = st.st_mtime_ns
    # Windows has no change time here (st_ctime is the creation time): 0, so the cache is weak
    out["change"][i] = 0 if sys.platform == "win32" else st.st_ctime_ns
    out["atime"][i] = st.st_atime_ns
    birth = getattr(st, "st_birthtime_ns", None)
    out["btime"][i] = int(birth) if birth is not None else 0
    out["attrs"][i] = getattr(st, "st_file_attributes", 0) or getattr(st, "st_flags", 0)
    out["tag"][i] = getattr(st, "st_reparse_tag", 0)
    out["case_ok"][i] = 1


def _kind_reason(st: os.stat_result) -> int:
    if stat.S_ISLNK(st.st_mode):
        return C.SURROGATE
    if not stat.S_ISREG(st.st_mode):
        return C.NOT_FILE
    attrs = getattr(st, "st_file_attributes", 0)
    if attrs & _WIN_RECALL:
        return C.CLOUD
    tag = getattr(st, "st_reparse_tag", 0)
    if attrs & _WIN_REPARSE and tag & 0xFFFF0FFF == 0x9000001A:
        return C.CLOUD
    if getattr(st, "st_flags", 0) & _SF_DATALESS:
        return C.CLOUD
    return C.OK


def _stamp(st: os.stat_result) -> tuple[int, int, int, int, int]:
    change = 0 if sys.platform == "win32" else st.st_ctime_ns
    return (int(st.st_dev) & _MASK64, int(st.st_ino), st.st_size, st.st_mtime_ns, change)


class _Fd:
    """A data descriptor opened without moving the access time where the OS allows it."""

    __slots__ = ("_counter", "atime_unkept", "fd")

    def __init__(self, fd: int, atime_unkept: bool, counter: list[int]) -> None:
        self.fd = fd
        self.atime_unkept = atime_unkept
        self._counter = counter
        counter[0] += 1

    def close(self) -> None:
        if self.fd >= 0:
            with contextlib.suppress(OSError):
                os.close(self.fd)
            self.fd = -1
            self._counter[0] -= 1

    def read_at(self, n: int, off: int) -> bytes:
        if hasattr(os, "pread"):
            parts: list[bytes] = []
            got = 0
            while got < n:
                b = os.pread(self.fd, n - got, off + got)
                if not b:
                    break
                parts.append(b)
                got += len(b)
            return b"".join(parts)
        os.lseek(self.fd, off, os.SEEK_SET)  # Windows: no pread; one thread per member at a time
        parts = []
        got = 0
        while got < n:
            b = os.read(self.fd, n - got)
            if not b:
                break
            parts.append(b)
            got += len(b)
        return b"".join(parts)


def _open_posix(path: str, keep_read: bool, counter: list[int]) -> _Fd:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    noatime = getattr(os, "O_NOATIME", 0)
    if noatime:
        try:
            return _Fd(os.open(path, flags | noatime), False, counter)
        except PermissionError as exc:
            if exc.errno != errno.EPERM:
                raise
            if not keep_read and not _mounted_noatime(path):
                raise _Skip(C.ATIME) from exc
            return _Fd(os.open(path, flags), not _mounted_noatime(path), counter)
    # macOS and others: no per-open switch in Python (the native reader turns the thread's
    # access-time updates off); keep_atime decides
    if not keep_read:
        raise _Skip(C.ATIME)
    return _Fd(os.open(path, flags), True, counter)


def _mounted_noatime(path: str) -> bool:
    if sys.platform == "win32":
        return False
    try:
        return bool(os.statvfs(path).f_flag & getattr(os, "ST_NOATIME", 1024))
    except OSError:
        return False


def _open_windows(path: str, keep_read: bool, counter: list[int]) -> _Fd:  # pragma: no cover - Windows
    import ctypes
    import msvcrt
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    k32.CreateFileW.restype = wintypes.HANDLE
    k32.CreateFileW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.LPVOID,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    ]
    invalid = wintypes.HANDLE(-1).value
    if path.startswith("\\\\?\\"):
        long = path
    elif path.startswith("\\\\"):
        long = "\\\\?\\UNC\\" + path[2:]
    else:
        long = "\\\\?\\" + path
    read_data, read_attrs, write_attrs, sync = 0x1, 0x80, 0x100, 0x100000
    no_recall = 0x00100000
    unkept = False
    h = k32.CreateFileW(long, read_data | read_attrs | write_attrs | sync, 7, None, 3, no_recall, None)
    if h == invalid:
        err = ctypes.get_last_error()  # type: ignore[attr-defined,unused-ignore]
        if err != 5:
            raise ctypes.WinError(err)  # type: ignore[attr-defined,unused-ignore]
        if not keep_read:
            raise _Skip(C.ATIME)
        unkept = True
        h = k32.CreateFileW(long, read_data | read_attrs | sync, 7, None, 3, no_recall, None)
        if h == invalid:
            raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]
    else:

        class FILETIME(ctypes.Structure):
            _fields_ = (("lo", wintypes.DWORD), ("hi", wintypes.DWORD))

        keep = FILETIME(0xFFFFFFFF, 0xFFFFFFFF)
        if not k32.SetFileTime(wintypes.HANDLE(h), None, ctypes.byref(keep), None):
            if not keep_read:
                k32.CloseHandle(wintypes.HANDLE(h))
                raise _Skip(C.ATIME)
            unkept = True
    fd = msvcrt.open_osfhandle(h, os.O_RDONLY | getattr(os, "O_BINARY", 0))  # type: ignore[attr-defined,unused-ignore]
    return _Fd(fd, unkept, counter)


class _Skip(Exception):
    def __init__(self, reason: int) -> None:
        super().__init__(reason)
        self.reason = reason


class _Member:
    __slots__ = ("attrs", "fd", "hasher", "offset", "opened", "path", "size", "stamp")

    def __init__(self, path: str, size: int, stamp: tuple[int, ...], hasher: Any, fd: _Fd | None) -> None:
        self.path = path
        self.size = size
        self.stamp = stamp
        self.hasher = hasher
        self.offset = C.BLOCK
        self.fd = fd
        self.opened = time.monotonic()


def _blake2b(data: bytes = b"") -> Any:
    return hashlib.blake2b(data, digest_size=32)


def _quick(size: int, parts: list[bytes]) -> int:
    h = hashlib.blake2b(digest_size=8)
    h.update(size.to_bytes(8, "little"))
    h.update(C.PROBE_PARAMS.to_bytes(2, "little"))
    for p in parts:
        h.update(p)
    return int.from_bytes(h.digest(), "little")


class FallbackReader:
    """The native DupesReader in Python: buffered reads, BLAKE2b-256 (docs/dupes.md §12.2)."""

    def __init__(self, *, keep_atime: str, small_whole: int, lease_s: float, chunk: int) -> None:
        if keep_atime not in ("skip", "read"):
            raise ValueError(f"keep_atime: {keep_atime}")
        self.keep_read = keep_atime == "read"
        self.small_whole = small_whole
        self.lease = lease_s
        self.chunk = chunk
        self._members: dict[int, _Member] = {}
        self._lock = threading.Lock()
        self._open = [0]
        self._c = {
            "data_opens": 0,
            "reopens": 0,
            "bytes_probe": 0,
            "bytes_full": 0,
            "atime_unkept": 0,
            "changed": 0,
        }
        self._clock = threading.Lock()

    def _count(self, key: str, n: int = 1) -> None:
        with self._clock:
            self._c[key] += n

    def _open_data(self, path: str) -> _Fd:
        if sys.platform == "win32":  # pragma: no cover - Windows
            fd = _open_windows(path, self.keep_read, self._open)
        else:
            fd = _open_posix(path, self.keep_read, self._open)
        self._count("data_opens")
        if fd.atime_unkept:
            self._count("atime_unkept")
        return fd

    def probe(
        self,
        keys: np.ndarray,
        paths: list[str],
        dev: np.ndarray,
        fid_lo: np.ndarray,
        fid_hi: np.ndarray,
        size: np.ndarray,
        attrs: np.ndarray,
        hand_off: bool,
    ) -> dict[str, Any]:
        n = len(keys)
        reason = np.zeros(n, "u1")
        quick = np.zeros(n, "<u8")
        has_full = np.zeros(n, "u1")
        full = bytearray(32 * n)
        mtime = np.zeros(n, "<i8")
        change = np.zeros(n, "<i8")
        unkept = np.zeros(n, "u1")
        nbytes = np.zeros(n, "<u8")
        keep: list[tuple[int, _Member]] = []
        for i in range(n):
            sz = int(size[i])
            fd: _Fd | None = None
            try:
                fd = self._open_data(paths[i])
                unkept[i] = fd.atime_unkept
                st = os.fstat(fd.fd)
                r = _kind_reason(st)
                if r:
                    raise _Skip(r)
                s0 = _stamp(st)
                want = (int(dev[i]), int(fid_lo[i]) | (int(fid_hi[i]) << 64))
                if (s0[0], s0[1]) != want or s0[2] != sz:
                    raise _Skip(C.CHANGED)
                mtime[i], change[i] = s0[3], s0[4]
                whole = sz <= max(self.small_whole, 2 * C.BLOCK)
                if whole:
                    data = fd.read_at(sz + 1, 0)
                    if len(data) != sz:
                        raise _Skip(C.CHANGED)
                    parts = [data[a:z] for a, z in C.probe_ranges(sz)]
                    got = sz
                else:
                    parts = []
                    got = 0
                    for a, z in C.probe_ranges(sz):
                        b = fd.read_at(C.BLOCK, a)
                        if len(b) != z - a:
                            raise _Skip(C.CHANGED)
                        parts.append(b)
                        got += len(b)
                nbytes[i] = got
                self._count("bytes_probe", got)
                if _stamp(os.fstat(fd.fd)) != s0:
                    raise _Skip(C.CHANGED)
                quick[i] = _quick(sz, parts)
                if whole:
                    has_full[i] = 1
                    full[32 * i : 32 * i + 32] = _blake2b(data).digest()
                else:
                    m = _Member(paths[i], sz, s0, _blake2b(parts[0]), fd if hand_off else None)
                    if hand_off:
                        fd = None  # owned by the member now
                    keep.append((int(keys[i]), m))
            except _Skip as s:
                reason[i] = s.reason
                if s.reason == C.CHANGED:
                    self._count("changed")
            except OSError as exc:
                reason[i] = _reason_of(exc)
            finally:
                if fd is not None:
                    fd.close()
        with self._lock:
            for k, m in keep:
                self._members[k] = m
        return {
            "key": np.asarray(keys, "<u8").copy(),
            "reason": reason,
            "quick": quick,
            "has_full": has_full,
            "full": bytes(full),
            "mtime": mtime,
            "change": change,
            "atime_unkept": unkept,
            "bytes": nbytes,
        }

    def _reopen(self, m: _Member) -> None:
        fd = self._open_data(m.path)
        try:
            st = os.fstat(fd.fd)
            r = _kind_reason(st)
            if r:
                raise _Skip(r)
            if _stamp(st) != m.stamp:
                self._count("changed")
                raise _Skip(C.CHANGED)
        except BaseException:
            fd.close()
            raise
        self._count("reopens")
        m.fd = fd
        m.opened = time.monotonic()

    def step(self, keys: np.ndarray, until: np.ndarray, max_bytes: int, max_s: float) -> dict[str, Any]:
        t0 = time.monotonic()
        n = len(keys)
        with self._lock:
            work = [self._members.pop(int(k), None) for k in keys]
        reason = np.zeros(n, "u1")
        offset = np.zeros(n, "<u8")
        at = np.zeros(n, "u1")
        interim = bytearray(16 * n)
        fin = bytearray(32 * n)
        nbytes = np.zeros(n, "<u8")
        reopens = np.zeros(n, "<u4")
        for i, w in enumerate(work):
            if w is None:
                reason[i] = C.UNKNOWN_KEY
        budget = max_bytes
        stop = False
        while not stop:
            progressed = False
            for i, m in enumerate(work):
                if m is None or reason[i]:
                    continue
                target = min(int(until[i]), m.size)
                if m.offset >= target:
                    continue
                if budget <= 0 or time.monotonic() - t0 >= max_s:
                    stop = True
                    break
                try:
                    if m.fd is None:
                        self._reopen(m)
                        reopens[i] += 1
                    assert m.fd is not None
                    ln = min(self.chunk, target - m.offset)
                    last = m.offset + ln == m.size
                    b = m.fd.read_at(ln + 1 if last else ln, m.offset)
                    if len(b) != ln:
                        self._count("changed")
                        raise _Skip(C.CHANGED)
                    m.hasher.update(b)
                    m.offset += ln
                    nbytes[i] += ln
                    budget -= ln
                    progressed = True
                    if m.offset == m.size and _stamp(os.fstat(m.fd.fd)) != m.stamp:
                        self._count("changed")
                        raise _Skip(C.CHANGED)
                    if time.monotonic() - m.opened >= self.lease:
                        m.fd.close()
                        m.fd = None
                except _Skip as s:
                    reason[i] = s.reason
                except OSError as exc:
                    reason[i] = _reason_of(exc)
            if not progressed:
                break
        back: list[tuple[int, _Member]] = []
        for i, m in enumerate(work):
            if m is None:
                continue
            offset[i] = m.offset
            if reason[i]:
                if m.fd is not None:
                    m.fd.close()
                continue
            target = min(int(until[i]), m.size)
            if m.offset == m.size:
                at[i] = 2
                fin[32 * i : 32 * i + 32] = m.hasher.digest()
                if m.fd is not None:
                    m.fd.close()
                continue
            if m.offset == target:
                at[i] = 1
                interim[16 * i : 16 * i + 16] = m.hasher.copy().digest()[:16]
            back.append((int(keys[i]), m))
        self._count("bytes_full", int(nbytes.sum()))
        with self._lock:
            for k, m in back:
                self._members[k] = m
        return {
            "key": np.asarray(keys, "<u8").copy(),
            "reason": reason,
            "offset": offset,
            "at": at,
            "interim": bytes(interim),
            "fin": bytes(fin),
            "bytes": nbytes,
            "reopens": reopens,
        }

    def suspend(self, keys: list[int] | None = None) -> None:
        with self._lock:
            ms = (
                list(self._members.values())
                if keys is None
                else [self._members[k] for k in keys if k in self._members]
            )
        for m in ms:
            if m.fd is not None:
                m.fd.close()
                m.fd = None

    def drop(self, keys: list[int] | None = None) -> None:
        with self._lock:
            if keys is None:
                gone = list(self._members.values())
                self._members.clear()
            else:
                gone = [m for k in keys if (m := self._members.pop(k, None)) is not None]
        for m in gone:
            if m.fd is not None:
                m.fd.close()

    def open_handles(self) -> int:
        return self._open[0]

    def members(self) -> int:
        with self._lock:
            return len(self._members)

    def counters(self) -> dict[str, int]:
        with self._clock:
            out = dict(self._c)
        out["open_handles"] = self._open[0]
        return out


class FallbackOps:
    alg = C.ALG_BLAKE2B
    name = "python"
    has_usn = False

    def identity(self, paths: list[str], want_usn: bool = False) -> dict[str, np.ndarray]:
        out = _empty_identity(len(paths))
        for i, p in enumerate(paths):
            try:
                st = os.stat(p, follow_symlinks=False)
            except OSError as exc:
                out["reason"][i] = _reason_of(exc)
                continue
            _stat_row(out, i, st)
            out["reason"][i] = _kind_reason(st)
        return out

    def extents(self, paths: list[str]) -> dict[str, Any] | None:
        return None  # no block maps: the ledger says "may share blocks" on clonable volumes

    def reader(
        self, *, unbuffered: bool, keep_atime: str, small_whole: int, lease_s: float, chunk: int
    ) -> Reader:
        return FallbackReader(keep_atime=keep_atime, small_whole=small_whole, lease_s=lease_s, chunk=chunk)


def default_ops() -> Ops:
    """The native stages when built (and not disabled), else the Python ones."""
    if native.has("DupesReader"):
        return NativeOps()
    return FallbackOps()

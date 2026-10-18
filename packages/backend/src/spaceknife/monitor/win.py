"""Windows watcher: one recursive ``ReadDirectoryChangesExW`` per scan root (docs/monitor.md §2.1).

A folder handle opened for overlapped I/O and one worker thread that keeps a read pending and
waits on its event (or on the stop event). Every completed read becomes one batch for the sink;
the next read is issued before the batch is decoded, so the kernel keeps buffering meanwhile.
The extended information class (Windows 10 1709+) carries each file's size, allocation and last
write time, so ``modified`` changes arrive with numbers; systems and filesystems without it fall
back to ``ReadDirectoryChangesW`` (``modified`` without numbers, ``is_dir`` unknown).

Lost events are never silent: a read that completes with no bytes (the kernel's buffer
overflowed, ``STATUS_NOTIFY_ENUM_DIR``) becomes ``overflow`` for the root and watching goes on.
When the root goes away (deleted, moved or renamed, its volume or share gone) the watcher reports
``failed``, emits ``overflow`` for the root and ends. A watch that never armed (the folder could
not be opened, the filesystem has no change notifications) only reports ``failed``: nothing was
watched, so nothing was lost; the caller falls back to polling.
"""

from __future__ import annotations

import ctypes
import os
import struct
import sys
import threading
import time
from collections.abc import Iterable
from ctypes import wintypes
from dataclasses import dataclass
from typing import Any

from loguru import logger

from .base import Change, Sink, WatchState, WatchStatus

assert sys.platform == "win32"

HANDLE = wintypes.HANDLE
DWORD = wintypes.DWORD
BOOL = wintypes.BOOL
LPDWORD = ctypes.POINTER(DWORD)
INVALID_HANDLE = ctypes.c_void_p(-1).value

FILE_LIST_DIRECTORY = 0x1
FILE_SHARE_ALL = 0x7  # read | write | delete: the watch never stands in the way of the user's work
OPEN_EXISTING = 3
FILE_FLAG_BACKUP_SEMANTICS = 0x02000000  # needed to open a folder
FILE_FLAG_OVERLAPPED = 0x40000000

# file name, folder name, attributes, size, last write, creation
NOTIFY_FILTER = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x40
READ_DIRECTORY_NOTIFY_INFORMATION = 1
READ_DIRECTORY_NOTIFY_EXTENDED_INFORMATION = 2

FILE_ACTION_ADDED = 1
FILE_ACTION_REMOVED = 2
FILE_ACTION_MODIFIED = 3
FILE_ACTION_RENAMED_OLD_NAME = 4
FILE_ACTION_RENAMED_NEW_NAME = 5

FILE_ATTRIBUTE_DIRECTORY = 0x10
_FA_PACKED = 0x200 | 0x800  # sparse | compressed: the real allocation needs GetCompressedFileSizeW
_FA_RECALL = 0x40000 | 0x400000  # cloud placeholders (recall on open / on data access)

ERROR_INVALID_FUNCTION = 1
ERROR_NOT_SUPPORTED = 50
ERROR_INVALID_PARAMETER = 87
ERROR_INVALID_LEVEL = 124
ERROR_OPERATION_ABORTED = 995
ERROR_IO_INCOMPLETE = 996
ERROR_NOTIFY_ENUM_DIR = 1022
_UNSUPPORTED = frozenset(
    {ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_INVALID_PARAMETER, ERROR_INVALID_LEVEL}
)

WAIT_OBJECT_0 = 0
WAIT_TIMEOUT = 0x102
DRIVE_REMOTE = 4
FILE_STANDARD_INFO_CLASS = 1

LOCAL_BUFFER = 1 << 20
NETWORK_BUFFER = 64 << 10  # a change notification over SMB is capped at 64 KiB
MIN_BUFFER = 256
HOLD_RENAME_S = 0.05  # a rename's old name waits this long for its new name (from the next read)
CHECK_LOCAL_S = 1.0  # how often the root's identity is checked (calls on the handle only)
CHECK_NETWORK_S = 5.0
CANCEL_WAIT_MS = 2000

ROOT_DELETED = "监视的文件夹已被删除"
ROOT_MOVED = "监视的文件夹已被移动或改名"
NO_EXTENDED = "此卷不提供扩展变化信息，文件大小变化需重新列出目录"
NO_NOTIFY = "此文件系统不支持变化通知"

_EPOCH_SECS = 11_644_473_600  # 1601-01-01 → 1970-01-01
# FILE_NOTIFY_EXTENDED_INFORMATION up to FileName: NextEntryOffset, Action, LastModificationTime,
# AllocatedLength, FileSize, FileAttributes, FileNameLength (the times, FileIds … skipped)
_EXT = struct.Struct("<II8xq16xqqI20xI")
_STD = struct.Struct("<III")  # FILE_NOTIFY_INFORMATION: NextEntryOffset, Action, FileNameLength


class OVERLAPPED(ctypes.Structure):
    _fields_ = (
        ("Internal", ctypes.c_size_t),
        ("InternalHigh", ctypes.c_size_t),
        ("Offset", DWORD),
        ("OffsetHigh", DWORD),
        ("hEvent", HANDLE),
    )


class FILE_STANDARD_INFO(ctypes.Structure):
    _fields_ = (
        ("AllocationSize", ctypes.c_int64),
        ("EndOfFile", ctypes.c_int64),
        ("NumberOfLinks", DWORD),
        ("DeletePending", ctypes.c_ubyte),
        ("Directory", ctypes.c_ubyte),
    )


_k32 = ctypes.WinDLL("kernel32", use_last_error=True)


def _proto(fn: Any, restype: object, *argtypes: object) -> Any:
    fn.restype = restype
    fn.argtypes = argtypes
    return fn


_LPOV = ctypes.POINTER(OVERLAPPED)
_RDCW_ARGS = (HANDLE, ctypes.c_void_p, DWORD, BOOL, DWORD, LPDWORD, _LPOV, ctypes.c_void_p)
_proto(_k32.CreateFileW, HANDLE, wintypes.LPCWSTR, DWORD, DWORD, ctypes.c_void_p, DWORD, DWORD, HANDLE)
_proto(_k32.ReadDirectoryChangesW, BOOL, *_RDCW_ARGS)
try:  # Windows 10 1709+
    _rdcw_ex: Any = _proto(_k32.ReadDirectoryChangesExW, BOOL, *_RDCW_ARGS, ctypes.c_int)
except AttributeError:
    _rdcw_ex = None
_proto(_k32.GetOverlappedResult, BOOL, HANDLE, _LPOV, LPDWORD, BOOL)
_proto(_k32.CancelIoEx, BOOL, HANDLE, _LPOV)
_proto(_k32.CreateEventW, HANDLE, ctypes.c_void_p, BOOL, BOOL, wintypes.LPCWSTR)
_proto(_k32.SetEvent, BOOL, HANDLE)
_proto(_k32.ResetEvent, BOOL, HANDLE)
_proto(_k32.WaitForMultipleObjects, DWORD, DWORD, ctypes.POINTER(HANDLE), BOOL, DWORD)
_proto(_k32.WaitForSingleObject, DWORD, HANDLE, DWORD)
_proto(_k32.CloseHandle, BOOL, HANDLE)
_proto(_k32.GetDriveTypeW, wintypes.UINT, wintypes.LPCWSTR)
_proto(_k32.GetVolumePathNameW, BOOL, wintypes.LPCWSTR, wintypes.LPWSTR, DWORD)
_proto(_k32.GetDiskFreeSpaceW, BOOL, wintypes.LPCWSTR, LPDWORD, LPDWORD, LPDWORD, LPDWORD)
_proto(_k32.GetFinalPathNameByHandleW, DWORD, HANDLE, wintypes.LPWSTR, DWORD, DWORD)
_proto(_k32.GetFileInformationByHandleEx, BOOL, HANDLE, ctypes.c_int, ctypes.c_void_p, DWORD)

# Buffers of reads whose cancellation never completed (a hung redirector): the kernel may still
# write into them, so they are never freed.
_leaked: list[object] = []


def _last_error() -> int:
    return int(ctypes.get_last_error())


def _valid(handle: int | None) -> bool:
    return handle is not None and handle != INVALID_HANDLE and handle != 0


def _message(err: int) -> str:
    return ctypes.FormatError(err).strip().rstrip("。.") or f"错误 {err}"


def _unix(ft: int) -> int:
    """FILETIME (100 ns since 1601) → Unix seconds; 0 (unknown) stays 0, nothing below 0."""
    return 0 if ft <= 0 else max(0, ft // 10_000_000 - _EPOCH_SECS)


def _long_path(path: str) -> str:
    if len(path) < 240 or path.startswith("\\\\?\\"):
        return path
    if path.startswith("\\\\"):
        return "\\\\?\\UNC\\" + path[2:]
    return "\\\\?\\" + path


def _volume_path(path: str) -> str:
    buf = ctypes.create_unicode_buffer(1024)
    if _k32.GetVolumePathNameW(_long_path(path), buf, 1024) and buf.value:
        return str(buf.value)
    drive = os.path.splitdrive(path)[0]
    return drive + "\\" if drive else ""


def is_network_path(path: str) -> bool:
    """UNC paths and folders on a network drive (decided from the string first: no lookup)."""
    p = path.replace("/", "\\")
    if p.upper().startswith("\\\\?\\UNC\\"):
        return True
    if p.startswith("\\\\") and not p.startswith(("\\\\?\\", "\\\\.\\")):
        return True
    volume = _volume_path(path)
    return bool(volume) and int(_k32.GetDriveTypeW(volume)) == DRIVE_REMOTE


def _cluster_size(path: str) -> int:
    volume = _volume_path(path)
    spc, bps, free, total = DWORD(0), DWORD(0), DWORD(0), DWORD(0)
    if volume and _k32.GetDiskFreeSpaceW(
        volume, ctypes.byref(spc), ctypes.byref(bps), ctypes.byref(free), ctypes.byref(total)
    ):
        return int(spc.value * bps.value)
    return 0


def _final_path(handle: int) -> str:
    size = 1024
    for _ in range(2):
        buf = ctypes.create_unicode_buffer(size)
        got = int(_k32.GetFinalPathNameByHandleW(handle, buf, size, 0))
        if got == 0:
            return ""
        if got < size:
            return str(buf.value)
        size = got + 1
    return ""


@dataclass(slots=True)
class NotifyDecoder:
    """Notification buffers → changes. One per watcher: it keeps a rename's old name when the
    new name has not arrived yet (it comes with the next read)."""

    prefix: str  # the root, ending in a separator
    extended: bool = True  # FILE_NOTIFY_EXTENDED_INFORMATION records (else FILE_NOTIFY_INFORMATION)
    cluster: int = 0  # allocations that are not a multiple of it are resident in the MFT: 0
    held: tuple[str, bool | None] | None = None  # RENAMED_OLD_NAME waiting for RENAMED_NEW_NAME

    def flush(self) -> list[Change]:
        """The held old name as ``deleted`` (its new name never came: the name is gone anyway)."""
        if self.held is None:
            return []
        path, is_dir = self.held
        self.held = None
        return [Change("deleted", path, is_dir=is_dir)]

    def decode(self, data: bytes) -> list[Change]:
        out: list[Change] = []
        head = _EXT if self.extended else _STD
        end = len(data)
        off = 0
        modified = 0
        while off + head.size <= end:
            ft = alloc = size = attrs = 0
            if self.extended:
                nxt, action, ft, alloc, size, attrs, nlen = _EXT.unpack_from(data, off)
            else:
                nxt, action, nlen = _STD.unpack_from(data, off)
            start = off + head.size
            if start + nlen > end:
                break  # torn record (never seen in practice): nothing after it is readable
            path = self.prefix + data[start : start + nlen].decode("utf-16-le", "surrogatepass")
            is_dir = bool(attrs & FILE_ATTRIBUTE_DIRECTORY) if self.extended else None
            if self.held is not None and action != FILE_ACTION_RENAMED_NEW_NAME:
                out += self.flush()
            if action == FILE_ACTION_ADDED:
                out.append(Change("created", path, is_dir=is_dir))
            elif action == FILE_ACTION_REMOVED:
                out.append(Change("deleted", path, is_dir=is_dir))
            elif action == FILE_ACTION_MODIFIED:
                if not self.extended:
                    out.append(Change("modified", path))
                    modified += 1
                elif not is_dir:  # a folder's own times changed: its entries tell the rest
                    out.append(
                        Change(
                            "modified",
                            path,
                            is_dir=False,
                            size=max(0, size),
                            alloc=self._alloc(alloc, attrs),
                            mtime=_unix(ft),
                        )
                    )
                    modified += 1
            elif action == FILE_ACTION_RENAMED_OLD_NAME:
                self.held = (path, is_dir)
            elif action == FILE_ACTION_RENAMED_NEW_NAME:
                old, old_dir = self.held if self.held is not None else ("", None)
                self.held = None
                out.append(
                    Change("renamed", path, old_path=old, is_dir=old_dir if is_dir is None else is_dir)
                )
            if nxt == 0:
                break
            off += nxt
        if modified > 1:  # repeated modified for one path: the last one has the newest numbers
            last = {c.path: i for i, c in enumerate(out) if c.kind == "modified"}
            out = [c for i, c in enumerate(out) if c.kind != "modified" or last[c.path] == i]
        return out

    def _alloc(self, alloc: int, attrs: int) -> int | None:
        if attrs & _FA_PACKED:
            return None  # AllocatedLength is not what a compressed or sparse file occupies
        if attrs & _FA_RECALL:
            return 0  # cloud placeholder: counted as 0, like the scan does
        if self.cluster > 0 and alloc % self.cluster:
            return 0  # resident data lives in the MFT record and takes no clusters
        return max(0, alloc)


class RdcwWatcher:
    """Recursive watch of ``root`` with ``ReadDirectoryChangesExW`` (see the module docstring).

    ``buffer_size``: bytes per read; by default 1 MiB on local volumes and 64 KiB on network
    paths (the kernel keeps a buffer of the same size between reads; overflowing it = overflow).
    ``status()`` turns ``running`` once the first read is pending: changes from then on arrive.
    """

    name = "rdcw"

    def __init__(self, root: str, *, buffer_size: int | None = None) -> None:
        self.root = root
        self.prefix = root if root.endswith(("\\", "/")) else root + "\\"
        self._explicit_buffer = buffer_size is not None
        self.buffer_size = -(-max(MIN_BUFFER, buffer_size) // 8) * 8 if buffer_size is not None else 0
        self.extended = _rdcw_ex is not None  # tests clear it before start() to get the fallback
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._sink: Sink | None = None
        self._state: WatchState = "starting"
        self._detail = ""
        self._overflows = 0
        self._events = 0
        self._handle: int | None = None
        self._io_event: int | None = None
        self._stop_event: int | None = None
        self._ov = OVERLAPPED()
        self._buf: ctypes.Array[ctypes.c_char] | None = None
        self._pending = False  # a read is outstanding: its buffer and OVERLAPPED are in use
        self._armed = False  # a read was issued once: from here on lost changes mean overflow
        self._attempts: list[tuple[bool, int]] = []  # (extended, buffer size) still to try
        self._completed = 0  # reads that completed successfully
        self._final = ""  # the root's final path when opened (moved or deleted → it changes)
        self._check_every = CHECK_LOCAL_S
        self._decoder = NotifyDecoder(self.prefix)

    # ----------------------------------------------------------------------------- Watcher API

    def start(self, sink: Sink) -> None:
        with self._lock:
            if self._thread is not None or self._stop.is_set():
                raise RuntimeError("RdcwWatcher.start() may only be called once, before stop()")
            io_event = _k32.CreateEventW(None, True, False, None)
            stop_event = _k32.CreateEventW(None, True, False, None)
            if not _valid(io_event) or not _valid(stop_event):
                err = _last_error()
                for h in (io_event, stop_event):
                    if _valid(h):
                        _k32.CloseHandle(h)
                raise ctypes.WinError(err)
            self._io_event, self._stop_event = int(io_event), int(stop_event)
            self._ov.hEvent = self._io_event
            self._sink = sink
            self._thread = threading.Thread(target=self._run, name=f"rdcw {self.root}", daemon=True)
            self._thread.start()

    def add_dirs(self, dirs: Iterable[str]) -> None:
        """Recursive watch: every folder below the root is covered already."""

    def stop(self) -> None:
        with self._lock:
            self._stop.set()
            if self._stop_event is not None:
                _k32.SetEvent(self._stop_event)
            if self._handle is not None:  # nothing pending: a harmless no-op
                _k32.CancelIoEx(self._handle, ctypes.byref(self._ov))
            thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join(1.0)
            if thread.is_alive():
                logger.warning("文件监视线程未能在 1 秒内结束：{}", self.root)
        with self._lock:
            if self._state != "failed":
                self._state, self._detail = "stopped", ""

    def status(self) -> WatchStatus:
        with self._lock:
            return WatchStatus(
                state=self._state,
                detail=self._detail,
                watched_dirs=-1 if self._state == "running" else 0,
                overflows=self._overflows,
                events=self._events,
            )

    # ----------------------------------------------------------------------------- worker

    def _run(self) -> None:
        try:
            if self._open() and self._arm():
                self._loop()
        except Exception as exc:
            logger.exception("文件监视线程异常：{}", self.root)
            self._fail(f"监视意外中断：{exc}")
        finally:
            self._close()

    def _open(self) -> bool:
        handle = _k32.CreateFileW(
            _long_path(self.root),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_ALL,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
        if not _valid(handle):
            self._fail(f"无法监视此文件夹：{_message(_last_error())}")
            return False
        with self._lock:
            self._handle = int(handle)
        network = is_network_path(self.root)
        size = self.buffer_size if self._explicit_buffer else NETWORK_BUFFER if network else LOCAL_BUFFER
        sizes = [size] if self._explicit_buffer or size <= NETWORK_BUFFER else [size, NETWORK_BUFFER]
        self._attempts = [(ext, s) for ext in ((True, False) if self.extended else (False,)) for s in sizes]
        self._check_every = CHECK_NETWORK_S if network else CHECK_LOCAL_S
        self._decoder.cluster = _cluster_size(self.root)
        self._final = _final_path(self._handle)
        return not self._stop.is_set()

    def _arm(self) -> bool:
        """Issue a read with the next (extended, buffer size) the volume accepts: a share may
        refuse a big buffer, an old or foreign filesystem the extended class."""
        err = ERROR_NOT_SUPPORTED
        while self._attempts:
            extended, size = self._attempts.pop(0)
            if self._buf is None or size != self.buffer_size:
                self._buf = ctypes.create_string_buffer(size)
            self.extended = self._decoder.extended = extended
            self.buffer_size = size
            err = self._issue()
            if err not in _UNSUPPORTED:
                break
        if err:
            if not self._stop.is_set():
                self._fail(NO_NOTIFY if err in _UNSUPPORTED else f"无法监视此文件夹：{_message(err)}")
            return False
        with self._lock:
            self._armed = True
            if not self._stop.is_set():
                self._state, self._detail = "running", "" if self.extended else NO_EXTENDED
        logger.debug(
            "开始监视 {}（{}，缓冲区 {} KiB）",
            self.root,
            "ReadDirectoryChangesExW" if self.extended else "ReadDirectoryChangesW",
            self.buffer_size >> 10,
        )
        return True

    def _issue(self) -> int:
        """Start the next read; 0 or the Win32 error. No lock is held across the call (it may
        reach a network redirector); stop() sets the stop event, which the wait sees anyway."""
        if self._stop.is_set() or self._handle is None or self._buf is None:
            return ERROR_OPERATION_ABORTED
        _k32.ResetEvent(self._io_event)
        args = (
            self._handle,
            self._buf,
            self.buffer_size,
            True,
            NOTIFY_FILTER,
            None,
            ctypes.byref(self._ov),
            None,
        )
        ctypes.set_last_error(0)
        if self.extended:
            ok = _rdcw_ex(*args, READ_DIRECTORY_NOTIFY_EXTENDED_INFORMATION)
        else:
            ok = _k32.ReadDirectoryChangesW(*args)
        if not ok:
            return _last_error() or ERROR_INVALID_FUNCTION
        self._pending = True
        return 0

    def _loop(self) -> None:
        handles = (HANDLE * 2)(self._io_event, self._stop_event)
        next_check = time.monotonic() + self._check_every
        while not self._stop.is_set():
            held = self._decoder.held is not None
            wait = HOLD_RENAME_S if held else max(0.0, next_check - time.monotonic())
            r = int(_k32.WaitForMultipleObjects(2, handles, False, int(wait * 1000)))
            if self._stop.is_set() or r == WAIT_OBJECT_0 + 1:
                return
            if r == WAIT_TIMEOUT:
                self._deliver(self._decoder.flush())
            elif r != WAIT_OBJECT_0:
                self._fail(f"监视意外中断：{_message(_last_error())}")
                return
            elif not self._completion():
                return
            if time.monotonic() >= next_check:
                next_check = time.monotonic() + self._check_every
                problem = self._root_problem(lookup=False)
                if problem:
                    self._fail(problem)
                    return

    def _completion(self) -> bool:
        """Reap the finished read, issue the next one, hand the batch over; False: watch ended."""
        got = DWORD(0)
        ctypes.set_last_error(0)
        ok = _k32.GetOverlappedResult(self._handle, ctypes.byref(self._ov), ctypes.byref(got), False)
        err = 0 if ok else _last_error()
        if err != ERROR_IO_INCOMPLETE:  # (only our own completion sets the event)
            self._pending = False
        if self._stop.is_set():
            return False
        if err in _UNSUPPORTED and self._completed == 0 and self._attempts:
            # refused only once the request reached the volume (a share): take the next fallback;
            # whatever happened in between is unknown
            if not self._arm():
                return False
            self._deliver([*self._decoder.flush(), self._overflow()])
            return True
        overflow = err == ERROR_NOTIFY_ENUM_DIR or (err == 0 and got.value == 0)
        if err and not overflow:
            self._fail(self._failure(err))
            return False
        assert self._buf is not None
        data = ctypes.string_at(self._buf, got.value) if got.value else b""
        self._completed += 1
        reissue = self._issue()  # keep a read pending while this batch is decoded and delivered
        if overflow:
            logger.debug("文件监视缓冲区溢出：{}", self.root)
            batch = [*self._decoder.flush(), self._overflow()]
        else:
            batch = self._decoder.decode(data)
        self._deliver(batch)
        if reissue and not self._stop.is_set():
            self._fail(self._failure(reissue))
            return False
        return not self._stop.is_set()

    def _overflow(self) -> Change:
        return Change("overflow", self.root, is_dir=True)

    def _deliver(self, batch: list[Change]) -> None:
        sink = self._sink
        if not batch or sink is None or self._stop.is_set():
            return
        with self._lock:
            self._events += len(batch)
            self._overflows += sum(1 for c in batch if c.kind == "overflow")
        try:
            sink(batch)
        except Exception:
            logger.exception("文件变化回调失败：{}", self.root)

    def _fail(self, detail: str) -> None:
        if self._stop.is_set():
            return  # stopping: an aborted read is not a failure
        with self._lock:
            self._state, self._detail = "failed", detail
            armed = self._armed
        logger.warning("文件监视失败：{}：{}", self.root, detail)
        if armed:
            self._deliver([*self._decoder.flush(), self._overflow()])

    def _failure(self, err: int) -> str:
        problem = self._root_problem(lookup=True)
        if problem:
            return problem
        if err in _UNSUPPORTED:
            return NO_NOTIFY
        return f"监视中断：{_message(err)}"

    def _root_problem(self, *, lookup: bool) -> str:
        """Whether the root is gone: deleted (delete pending, or POSIX-style) or moved. Without
        ``lookup`` only the handle is asked (cheap even on a share); with it, the path as well."""
        handle = self._handle
        if handle is None:
            return ""
        info = FILE_STANDARD_INFO()
        if (
            _k32.GetFileInformationByHandleEx(
                handle, FILE_STANDARD_INFO_CLASS, ctypes.byref(info), ctypes.sizeof(info)
            )
            and info.DeletePending
        ):
            return ROOT_DELETED
        final = _final_path(handle)
        if self._final and final and os.path.normcase(final) != os.path.normcase(self._final):
            # a POSIX-style delete moves the still-open folder to \$Extend\$Deleted
            return ROOT_DELETED if "\\$extend\\$deleted\\" in os.path.normcase(final) else ROOT_MOVED
        if lookup and not os.path.isdir(self.root):
            return ROOT_DELETED
        return ""

    def _close(self) -> None:
        with self._lock:
            handle, self._handle = self._handle, None  # stop() no longer touches it
            pending = self._pending
        if handle is not None and pending:
            _k32.CancelIoEx(handle, ctypes.byref(self._ov))
            # the buffer and OVERLAPPED must outlive the read: wait for the cancellation
            if int(_k32.WaitForSingleObject(self._io_event, CANCEL_WAIT_MS)) != WAIT_OBJECT_0:
                logger.warning("文件监视的读取未能取消，保留其缓冲区：{}", self.root)
                _leaked.append((self._buf, self._ov))
        if handle is not None:
            _k32.CloseHandle(handle)
        with self._lock:
            self._pending = False
            for h in (self._io_event, self._stop_event):
                if h is not None:
                    _k32.CloseHandle(h)
            self._io_event = self._stop_event = None
            self._sink = None
            if self._state in ("starting", "running"):
                self._state, self._detail = "stopped", ""


__all__ = ["NotifyDecoder", "RdcwWatcher", "is_network_path"]

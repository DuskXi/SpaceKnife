"""Windows file primitives of the cleanup executor, by handle (docs/dupes.md §16.9.3).

Every open is relative to a folder handle checked before (``NtCreateFile`` with a root directory),
does not follow reparse points, never recalls a cloud file; identities are FILE_ID_INFO (the same
as the search's native reads: volume serial + 128-bit file id) and the stamps FILE_BASIC_INFO's
last write and change times. Renames are by handle and never replace a name
(``FileRenameInformation`` with ReplaceIfExists = FALSE), hard links are made from the keeper's
handle (``FileLinkInformation``), our own temporary names are deleted on a handle whose identity
was just checked (POSIX semantics where the file system has them). Block cloning is
``FSCTL_DUPLICATE_EXTENTS_TO_FILE`` on handles that deny others write access, after comparing each
range byte for byte (§16.9.4).

Only imported on Windows (the module itself loads on every system for the type checks).
"""

from __future__ import annotations

import ctypes
import sys
from ctypes import wintypes
from dataclasses import dataclass
from typing import Any

_dll: Any = getattr(ctypes, "WinDLL", None)
k32: Any = _dll("kernel32", use_last_error=True) if _dll is not None and sys.platform == "win32" else None
ntdll: Any = _dll("ntdll") if _dll is not None and sys.platform == "win32" else None
_last_error: Any = getattr(ctypes, "get_last_error", lambda: 0)

HANDLE = wintypes.HANDLE
INVALID = HANDLE(-1).value

DELETE = 0x00010000
READ_CONTROL = 0x00020000
SYNCHRONIZE = 0x00100000
FILE_READ_DATA = 0x0001
FILE_LIST_DIRECTORY = 0x0001
FILE_WRITE_DATA = 0x0002
FILE_ADD_FILE = 0x0002
FILE_TRAVERSE = 0x0020
FILE_READ_ATTRIBUTES = 0x0080
FILE_WRITE_ATTRIBUTES = 0x0100
SHARE_READ, SHARE_WRITE, SHARE_DELETE = 1, 2, 4
SHARE_ALL = 7

FILE_OPEN = 1
FILE_CREATE = 2
FILE_DIRECTORY_FILE = 0x1
FILE_SYNCHRONOUS_IO_NONALERT = 0x20
FILE_NON_DIRECTORY_FILE = 0x40
FILE_OPEN_REPARSE_POINT = 0x200000
FILE_OPEN_NO_RECALL = 0x400000
OBJ_CASE_INSENSITIVE = 0x40

FileRenameInformation = 10
FileLinkInformation = 11

ATTR_READONLY = 0x1
ATTR_DIRECTORY = 0x10
ATTR_REPARSE = 0x400
ATTR_SPARSE = 0x200
ATTR_ENCRYPTED = 0x4000
IO_REPARSE_TAG_SYMLINK = 0xA000000C

FSCTL_GET_REPARSE_POINT = 0x000900A8
FSCTL_READ_FILE_USN_DATA = 0x000900EB
FSCTL_DUPLICATE_EXTENTS_TO_FILE = 0x00098344
FSCTL_GET_INTEGRITY_INFORMATION = 0x0009027C

FILE_SUPPORTS_HARD_LINKS = 0x00400000
FILE_SUPPORTS_REPARSE_POINTS = 0x00000080
FILE_SUPPORTS_BLOCK_REFCOUNTING = 0x08000000
FILE_SUPPORTS_POSIX_UNLINK_RENAME = 0x00000400

STATUS_OBJECT_NAME_COLLISION = 0xC0000035
STATUS_OBJECT_NAME_NOT_FOUND = 0xC0000034
STATUS_OBJECT_PATH_NOT_FOUND = 0xC000003A
STATUS_SHARING_VIOLATION = 0xC0000043
STATUS_ACCESS_DENIED = 0xC0000022
STATUS_NOT_SAME_DEVICE = 0xC00000D4
STATUS_TOO_MANY_LINKS = 0xC0000265
STATUS_DISK_FULL = 0xC000007F
STATUS_DELETE_PENDING = 0xC0000056
STATUS_NOT_SUPPORTED = 0xC00000BB
STATUS_INVALID_DEVICE_REQUEST = 0xC0000010
STATUS_FILE_IS_A_DIRECTORY = 0xC00000BA

_UNIX_EPOCH_FT = 116444736000000000


class WinFsError(OSError):
    """A failed call: ``status`` is the NTSTATUS (or 0), ``winerror`` the Win32 error."""

    def __init__(self, what: str, *, status: int = 0, winerror: int = 0) -> None:
        super().__init__(winerror or status, f"{what}: status 0x{status:08X} error {winerror}")
        self.status = status
        self.win = winerror

    @property
    def code(self) -> str:
        s, w = self.status, self.win
        if s in (STATUS_OBJECT_NAME_COLLISION,) or w in (80, 183):
            return "exists"
        if s in (STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND) or w in (2, 3):
            return "missing"
        if s == STATUS_SHARING_VIOLATION or w in (32, 33):
            return "locked"
        if s == STATUS_ACCESS_DENIED or w == 5:
            return "denied"
        if s == STATUS_NOT_SAME_DEVICE or w == 17:
            return "exdev"
        if s == STATUS_TOO_MANY_LINKS or w == 1142:
            return "link_limit"
        if s == STATUS_DISK_FULL or w in (39, 112):
            return "enospc"
        if w == 1314:
            return "no_privilege"
        if s in (STATUS_NOT_SUPPORTED, STATUS_INVALID_DEVICE_REQUEST) or w in (1, 50):
            return "unsupported"
        if w == 347:  # ERROR_BLOCK_TOO_MANY_REFERENCES
            return "fs_limit"
        if s == STATUS_DELETE_PENDING or w == 303:
            return "missing"
        return "error"


class UNICODE_STRING(ctypes.Structure):
    _fields_ = [("Length", wintypes.USHORT), ("MaximumLength", wintypes.USHORT), ("Buffer", ctypes.c_wchar_p)]


class OBJECT_ATTRIBUTES(ctypes.Structure):
    _fields_ = [
        ("Length", wintypes.ULONG),
        ("RootDirectory", HANDLE),
        ("ObjectName", ctypes.POINTER(UNICODE_STRING)),
        ("Attributes", wintypes.ULONG),
        ("SecurityDescriptor", ctypes.c_void_p),
        ("SecurityQualityOfService", ctypes.c_void_p),
    ]


class IO_STATUS_BLOCK(ctypes.Structure):
    _fields_ = [("Status", ctypes.c_void_p), ("Information", ctypes.c_size_t)]


if k32 is not None and ntdll is not None:
    ntdll.NtCreateFile.restype = ctypes.c_ulong
    ntdll.NtCreateFile.argtypes = [
        ctypes.POINTER(HANDLE),
        wintypes.DWORD,
        ctypes.POINTER(OBJECT_ATTRIBUTES),
        ctypes.POINTER(IO_STATUS_BLOCK),
        ctypes.c_void_p,
        wintypes.ULONG,
        wintypes.ULONG,
        wintypes.ULONG,
        wintypes.ULONG,
        ctypes.c_void_p,
        wintypes.ULONG,
    ]
    ntdll.NtSetInformationFile.restype = ctypes.c_ulong
    ntdll.NtSetInformationFile.argtypes = [
        HANDLE,
        ctypes.POINTER(IO_STATUS_BLOCK),
        ctypes.c_void_p,
        wintypes.ULONG,
        ctypes.c_int,
    ]
    k32.CreateFileW.restype = HANDLE
    k32.CreateFileW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        wintypes.DWORD,
        HANDLE,
    ]
    k32.CloseHandle.argtypes = [HANDLE]
    k32.GetFileInformationByHandleEx.argtypes = [HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    k32.SetFileInformationByHandle.argtypes = [HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    k32.DeviceIoControl.argtypes = [
        HANDLE,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    ]
    k32.CreateSymbolicLinkW.restype = wintypes.BOOLEAN
    k32.CreateSymbolicLinkW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD]
    k32.GetFinalPathNameByHandleW.argtypes = [HANDLE, wintypes.LPWSTR, wintypes.DWORD, wintypes.DWORD]
    k32.ReadFile.argtypes = [
        HANDLE,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    ]
    k32.GetVolumeInformationByHandleW.argtypes = [
        HANDLE,
        wintypes.LPWSTR,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.POINTER(wintypes.DWORD),
        ctypes.POINTER(wintypes.DWORD),
        wintypes.LPWSTR,
        wintypes.DWORD,
    ]
    k32.FindFirstFileExW.restype = HANDLE
    k32.FindFirstFileExW.argtypes = [
        wintypes.LPCWSTR,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        wintypes.DWORD,
    ]
    k32.FindNextFileW.argtypes = [HANDLE, ctypes.c_void_p]
    k32.FindClose.argtypes = [HANDLE]
    ntdll.RtlNtStatusToDosError.argtypes = [ctypes.c_ulong]
    ntdll.RtlNtStatusToDosError.restype = wintypes.ULONG


def verbatim(path: str) -> str:
    """``\\\\?\\`` form: trailing dots and spaces kept, long paths without special handling."""
    p = path.replace("/", "\\")
    if p.startswith(("\\\\?\\", "\\\\.\\")):
        return p
    if p.startswith("\\\\"):
        return "\\\\?\\UNC\\" + p[2:]
    return "\\\\?\\" + p


def _ft_ns(t: int) -> int:
    return 0 if t <= _UNIX_EPOCH_FT else (t - _UNIX_EPOCH_FT) * 100


def _check(status: int, what: str) -> None:
    if status != 0 and status & 0xC0000000 == 0xC0000000:
        raise WinFsError(what, status=status & 0xFFFFFFFF, winerror=int(ntdll.RtlNtStatusToDosError(status)))


def _last(what: str) -> WinFsError:
    return WinFsError(what, winerror=_last_error())


@dataclass(frozen=True, slots=True)
class St:
    dev: int
    fid_lo: int
    fid_hi: int
    size: int
    mtime: int
    change: int
    btime: int
    atime: int
    nlink: int
    attrs: int
    tag: int
    delete_pending: bool
    directory: bool

    @property
    def ident(self) -> tuple[int, int, int]:
        return (self.dev, self.fid_lo, self.fid_hi)

    @property
    def is_link(self) -> bool:
        return bool(self.attrs & ATTR_REPARSE) and self.tag == IO_REPARSE_TAG_SYMLINK


def stat_handle(h: int) -> St:
    idb = ctypes.create_string_buffer(24)
    if not k32.GetFileInformationByHandleEx(h, 18, idb, 24):  # FileIdInfo
        raise _last("FileIdInfo")
    basic = ctypes.create_string_buffer(40)
    if not k32.GetFileInformationByHandleEx(h, 0, basic, 40):  # FileBasicInfo
        raise _last("FileBasicInfo")
    std = ctypes.create_string_buffer(24)
    if not k32.GetFileInformationByHandleEx(h, 1, std, 24):  # FileStandardInfo
        raise _last("FileStandardInfo")
    tagb = ctypes.create_string_buffer(8)
    tag = 0
    if k32.GetFileInformationByHandleEx(h, 9, tagb, 8):  # FileAttributeTagInfo
        tag = int.from_bytes(tagb.raw[4:8], "little")
    raw = idb.raw
    dev = int.from_bytes(raw[0:8], "little")
    fid = int.from_bytes(raw[8:24], "little")
    b = basic.raw
    ct, at, wt, cht = (int.from_bytes(b[i : i + 8], "little", signed=True) for i in (0, 8, 16, 24))
    attrs = int.from_bytes(b[32:36], "little")
    s = std.raw
    return St(
        dev=dev,
        fid_lo=fid & ((1 << 64) - 1),
        fid_hi=fid >> 64,
        size=int.from_bytes(s[8:16], "little", signed=True),
        mtime=_ft_ns(wt),
        change=_ft_ns(cht),
        btime=_ft_ns(ct),
        atime=_ft_ns(at),
        nlink=int.from_bytes(s[16:20], "little"),
        attrs=attrs,
        tag=tag if attrs & ATTR_REPARSE else 0,
        delete_pending=bool(s[20]),
        directory=bool(s[21]) or bool(attrs & ATTR_DIRECTORY),
    )


def usn_of(h: int) -> int:
    inp = (2).to_bytes(2, "little") + (3).to_bytes(2, "little")
    out = ctypes.create_string_buffer(1024)
    got = wintypes.DWORD()
    if not k32.DeviceIoControl(h, FSCTL_READ_FILE_USN_DATA, inp, 4, out, 1024, ctypes.byref(got), None):
        return 0
    raw = out.raw[: got.value]
    if len(raw) < 8:
        return 0
    major = int.from_bytes(raw[4:6], "little")
    at = {2: 24, 3: 40}.get(major)
    if at is None or len(raw) < at + 8:
        return 0
    return int.from_bytes(raw[at : at + 8], "little", signed=True)


def close(h: int | None) -> None:
    if h:
        k32.CloseHandle(h)


def open_path(path: str, access: int, share: int, *, follow: bool = False) -> int:
    flags = 0x02000000  # FILE_FLAG_BACKUP_SEMANTICS: folders open too
    if not follow:
        flags |= 0x00200000  # FILE_FLAG_OPEN_REPARSE_POINT
    flags |= 0x00100000  # FILE_FLAG_OPEN_NO_RECALL
    h = k32.CreateFileW(verbatim(path), access, share, None, 3, flags, None)  # OPEN_EXISTING
    if h in (None, INVALID):
        raise _last(f"open {path}")
    return int(h)


def open_dir(path: str) -> int:
    """A folder handle (never a reparse point: a junction or link on the last component refuses)."""
    h = open_path(path, FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE, SHARE_ALL)
    try:
        st = stat_handle(h)
        if not st.directory or st.attrs & ATTR_REPARSE:
            raise WinFsError(f"not a plain folder: {path}", winerror=267)
    except BaseException:
        close(h)
        raise
    return h


def open_at(
    dh: int, name: str, access: int, share: int, *, directory: bool = False, create: bool = False
) -> int:
    """Open ``name`` inside the folder handle ``dh``, as listed (no follow, no recall)."""
    buf = ctypes.create_unicode_buffer(name)
    us = UNICODE_STRING(len(name) * 2, len(name) * 2 + 2, ctypes.cast(buf, ctypes.c_wchar_p))
    oa = OBJECT_ATTRIBUTES(
        ctypes.sizeof(OBJECT_ATTRIBUTES), dh, ctypes.pointer(us), OBJ_CASE_INSENSITIVE, None, None
    )
    iosb = IO_STATUS_BLOCK()
    h = HANDLE()
    opts = FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT | FILE_OPEN_NO_RECALL
    opts |= FILE_DIRECTORY_FILE if directory else FILE_NON_DIRECTORY_FILE
    status = ntdll.NtCreateFile(
        ctypes.byref(h),
        access | SYNCHRONIZE,
        ctypes.byref(oa),
        ctypes.byref(iosb),
        None,
        0x80 if create else 0,  # FILE_ATTRIBUTE_NORMAL
        share,
        FILE_CREATE if create else FILE_OPEN,
        opts,
        None,
        0,
    )
    _check(status, f"open {name}")
    return int(h.value or 0)


def _name_info(root: int, name: str) -> ctypes.Array[ctypes.c_char]:
    """FILE_RENAME_INFORMATION / FILE_LINK_INFORMATION (same layout on x64): ReplaceIfExists FALSE."""
    data = name.encode("utf-16-le")
    buf = ctypes.create_string_buffer(20 + len(data) + 2)
    ctypes.memmove(ctypes.addressof(buf) + 8, ctypes.byref(HANDLE(root)), 8)
    ctypes.memmove(ctypes.addressof(buf) + 16, ctypes.byref(wintypes.ULONG(len(data))), 4)
    ctypes.memmove(ctypes.addressof(buf) + 20, data, len(data))
    return buf


def rename_handle(h: int, dst_dir: int, name: str) -> None:
    """Rename the file of ``h`` (opened with DELETE) to ``dst_dir\\name``; never replaces."""
    info = _name_info(dst_dir, name)
    iosb = IO_STATUS_BLOCK()
    _check(
        ntdll.NtSetInformationFile(h, ctypes.byref(iosb), info, len(info), FileRenameInformation),
        f"rename to {name}",
    )


def link_handle(h: int, dst_dir: int, name: str) -> None:
    """Another name ``dst_dir\\name`` for the file of ``h``; never replaces."""
    info = _name_info(dst_dir, name)
    iosb = IO_STATUS_BLOCK()
    _check(
        ntdll.NtSetInformationFile(h, ctypes.byref(iosb), info, len(info), FileLinkInformation),
        f"link {name}",
    )


def delete_handle(h: int) -> None:
    """Delete the file of ``h`` (opened with DELETE): POSIX semantics, else the classic way."""
    flags = wintypes.ULONG(0x1 | 0x2)  # FILE_DISPOSITION_FLAG_DELETE | POSIX_SEMANTICS
    if k32.SetFileInformationByHandle(h, 21, ctypes.byref(flags), 4):  # FileDispositionInfoEx
        return
    yes = wintypes.BOOLEAN(True)
    if not k32.SetFileInformationByHandle(h, 4, ctypes.byref(yes), 1):  # FileDispositionInfo
        raise _last("delete")


def final_path(h: int) -> str:
    buf = ctypes.create_unicode_buffer(32768)
    n = k32.GetFinalPathNameByHandleW(h, buf, 32768, 0)  # FILE_NAME_NORMALIZED | VOLUME_NAME_DOS
    if not n:
        raise _last("final path")
    p = buf.value
    if p.startswith("\\\\?\\UNC\\"):
        return "\\\\" + p[8:]
    return p[4:] if p.startswith("\\\\?\\") else p


def create_symlink(link_path: str, text: str, *, unprivileged: bool) -> None:
    flags = 0x2 if unprivileged else 0  # SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE
    if not k32.CreateSymbolicLinkW(verbatim(link_path), text, flags):
        err = _last_error()
        if err == 87 and unprivileged:  # older Windows: the flag is unknown
            if k32.CreateSymbolicLinkW(verbatim(link_path), text, 0):
                return
            err = _last_error()
        raise WinFsError("symlink", winerror=err)


def read_symlink(h: int) -> tuple[str, bool] | None:
    """(substitute name as written, relative) of a symbolic link handle, else None."""
    out = ctypes.create_string_buffer(16 * 1024 + 64)
    got = wintypes.DWORD()
    if not k32.DeviceIoControl(h, FSCTL_GET_REPARSE_POINT, None, 0, out, len(out), ctypes.byref(got), None):
        return None
    raw = out.raw[: got.value]
    if len(raw) < 20 or int.from_bytes(raw[0:4], "little") != IO_REPARSE_TAG_SYMLINK:
        return None
    so, sl, po, pl = (int.from_bytes(raw[8 + 2 * i : 10 + 2 * i], "little") for i in range(4))
    flags = int.from_bytes(raw[16:20], "little")
    path = raw[20:]
    sub = path[so : so + sl].decode("utf-16-le")
    printed = path[po : po + pl].decode("utf-16-le")
    rel = bool(flags & 1)
    text = printed or sub
    if not rel and text.startswith("\\??\\"):
        text = text[4:]
    return text, rel


def volume_flags(h: int) -> int:
    flags = wintypes.DWORD()
    serial = wintypes.DWORD()
    maxlen = wintypes.DWORD()
    if not k32.GetVolumeInformationByHandleW(
        h, None, 0, ctypes.byref(serial), ctypes.byref(maxlen), ctypes.byref(flags), None, 0
    ):
        return 0
    return int(flags.value)


def cluster_size(path: str) -> int:
    root = ctypes.create_unicode_buffer(1024)
    if not k32.GetVolumePathNameW(ctypes.c_wchar_p(path), root, 1024):
        return 4096
    spc, bps, free, total = wintypes.DWORD(), wintypes.DWORD(), wintypes.DWORD(), wintypes.DWORD()
    if not k32.GetDiskFreeSpaceW(
        root, ctypes.byref(spc), ctypes.byref(bps), ctypes.byref(free), ctypes.byref(total)
    ):
        return 4096
    return int(spc.value) * int(bps.value) or 4096


def integrity(h: int) -> tuple[int, int] | None:
    out = ctypes.create_string_buffer(16)
    got = wintypes.DWORD()
    if not k32.DeviceIoControl(h, FSCTL_GET_INTEGRITY_INFORMATION, None, 0, out, 16, ctypes.byref(got), None):
        return None
    raw = out.raw
    return int.from_bytes(raw[0:2], "little"), int.from_bytes(raw[4:8], "little")


def suspend_times(h: int) -> bool:
    """Ask the file system not to update the handle's times (−1 for last write, change, access)."""
    b = ctypes.create_string_buffer(40)
    minus = (-1).to_bytes(8, "little", signed=True)
    ctypes.memmove(ctypes.addressof(b) + 8, minus, 8)
    ctypes.memmove(ctypes.addressof(b) + 16, minus, 8)
    ctypes.memmove(ctypes.addressof(b) + 24, minus, 8)
    return bool(k32.SetFileInformationByHandle(h, 0, b, 40))


def keep_access_time(h: int) -> bool:
    b = ctypes.create_string_buffer(40)
    ctypes.memmove(ctypes.addressof(b) + 8, (-1).to_bytes(8, "little", signed=True), 8)
    return bool(k32.SetFileInformationByHandle(h, 0, b, 40))


def set_mtime(h: int, mtime_ns: int) -> bool:
    b = ctypes.create_string_buffer(40)
    ft = mtime_ns // 100 + _UNIX_EPOCH_FT
    ctypes.memmove(ctypes.addressof(b) + 16, ft.to_bytes(8, "little", signed=True), 8)
    return bool(k32.SetFileInformationByHandle(h, 0, b, 40))


class _OVERLAPPED(ctypes.Structure):
    _fields_ = [
        ("Internal", ctypes.c_size_t),
        ("InternalHigh", ctypes.c_size_t),
        ("Offset", wintypes.DWORD),
        ("OffsetHigh", wintypes.DWORD),
        ("hEvent", HANDLE),
    ]


def read_at(h: int, n: int, off: int) -> bytes:
    buf = ctypes.create_string_buffer(n)
    got = wintypes.DWORD()
    ov = _OVERLAPPED(0, 0, off & 0xFFFFFFFF, off >> 32, None)
    if not k32.ReadFile(h, buf, n, ctypes.byref(got), ctypes.byref(ov)):
        err = _last_error()
        if err == 38:  # ERROR_HANDLE_EOF
            return b""
        raise WinFsError("read", winerror=err)
    return buf.raw[: got.value]


def duplicate_extents(target: int, source: int, src_off: int, dst_off: int, n: int) -> None:
    data = ctypes.create_string_buffer(32)
    ctypes.memmove(ctypes.addressof(data), ctypes.byref(HANDLE(source)), 8)
    ctypes.memmove(ctypes.addressof(data) + 8, src_off.to_bytes(8, "little", signed=True), 8)
    ctypes.memmove(ctypes.addressof(data) + 16, dst_off.to_bytes(8, "little", signed=True), 8)
    ctypes.memmove(ctypes.addressof(data) + 24, n.to_bytes(8, "little", signed=True), 8)
    got = wintypes.DWORD()
    if not k32.DeviceIoControl(
        target, FSCTL_DUPLICATE_EXTENTS_TO_FILE, data, 32, None, 0, ctypes.byref(got), None
    ):
        raise _last("duplicate extents")


class _FIND(ctypes.Structure):
    _fields_ = [
        ("dwFileAttributes", wintypes.DWORD),
        ("ftCreationTime", wintypes.FILETIME),
        ("ftLastAccessTime", wintypes.FILETIME),
        ("ftLastWriteTime", wintypes.FILETIME),
        ("nFileSizeHigh", wintypes.DWORD),
        ("nFileSizeLow", wintypes.DWORD),
        ("dwReserved0", wintypes.DWORD),
        ("dwReserved1", wintypes.DWORD),
        ("cFileName", ctypes.c_wchar * 260),
        ("cAlternateFileName", ctypes.c_wchar * 14),
    ]


def case_twins(folder: str, name: str) -> bool:
    """Whether ``folder`` holds two or more entries matching ``name`` ignoring case (a
    case-sensitive folder, a WinBtrfs tree made on Linux): opening by name may reach another."""
    data = _FIND()
    h = k32.FindFirstFileExW(verbatim(folder.rstrip("\\") + "\\" + name), 1, ctypes.byref(data), 0, None, 0)
    if h in (None, INVALID):
        return False
    try:
        n = 1
        while n < 2 and k32.FindNextFileW(h, ctypes.byref(data)):
            n += 1
        return n >= 2
    finally:
        k32.FindClose(h)

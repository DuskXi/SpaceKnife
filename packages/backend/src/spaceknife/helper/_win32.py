"""Win32 plumbing for the helper (ctypes): overlapped named pipes, DACLs, admin rights, UAC launch.

Everything blocking takes a timeout; :class:`Pipe` I/O can be aborted from any thread.
"""

from __future__ import annotations

import ctypes
import sys
import threading
from collections.abc import Callable
from ctypes import wintypes
from typing import TYPE_CHECKING, Any

import numpy as np

from ..i18n import _

if TYPE_CHECKING:
    from .launch import ShellExecuteParams

assert sys.platform == "win32"

_k32 = ctypes.WinDLL("kernel32", use_last_error=True)
_adv = ctypes.WinDLL("advapi32", use_last_error=True)
_shell32 = ctypes.WinDLL("shell32", use_last_error=True)
_ole32 = ctypes.WinDLL("ole32")
_ntdll = ctypes.WinDLL("ntdll")

HANDLE = wintypes.HANDLE
DWORD = wintypes.DWORD
BOOL = wintypes.BOOL
LPDWORD = ctypes.POINTER(DWORD)
INVALID_HANDLE = ctypes.c_void_p(-1).value

GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
FILE_SHARE_READ = 0x1
FILE_SHARE_WRITE = 0x2
CREATE_NEW = 1
OPEN_EXISTING = 3
FILE_ATTRIBUTE_TEMPORARY = 0x100
FILE_FLAG_OVERLAPPED = 0x40000000
SECURITY_SQOS_PRESENT = 0x00100000
SECURITY_IDENTIFICATION = 0x00010000  # the pipe server may identify, never impersonate, the helper
PIPE_ACCESS_DUPLEX = 0x3
FILE_FLAG_FIRST_PIPE_INSTANCE = 0x00080000
PIPE_REJECT_REMOTE_CLIENTS = 0x8  # byte type, byte read mode, blocking (all 0) + local only
PIPE_BUFFER = 1 << 20

ERROR_FILE_NOT_FOUND = 2
ERROR_ACCESS_DENIED = 5
ERROR_BROKEN_PIPE = 109
ERROR_PIPE_BUSY = 231
ERROR_NO_DATA = 232
ERROR_PIPE_NOT_CONNECTED = 233
ERROR_PIPE_CONNECTED = 535
ERROR_OPERATION_ABORTED = 995
ERROR_IO_PENDING = 997
ERROR_CANCELLED = 1223
_CLOSED_ERRORS = frozenset({ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED})

WAIT_OBJECT_0 = 0
WAIT_TIMEOUT = 0x102
INFINITE = 0xFFFFFFFF
STILL_ACTIVE = 259
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
SYNCHRONIZE = 0x00100000
TOKEN_QUERY = 0x8
TOKEN_USER_CLASS = 1

COINIT_APARTMENTTHREADED = 0x2
COINIT_DISABLE_OLE1DDE = 0x4

DRIVE_REMOVABLE, DRIVE_FIXED, DRIVE_RAMDISK = 2, 3, 6
FSCTL_GET_NTFS_VOLUME_DATA = 0x00090064
MAX_IO_CHUNK = 32 << 20


class OVERLAPPED(ctypes.Structure):
    _fields_ = (
        ("Internal", ctypes.c_void_p),
        ("InternalHigh", ctypes.c_void_p),
        ("Offset", DWORD),
        ("OffsetHigh", DWORD),
        ("hEvent", HANDLE),
    )


class SECURITY_ATTRIBUTES(ctypes.Structure):
    _fields_ = (("nLength", DWORD), ("lpSecurityDescriptor", ctypes.c_void_p), ("bInheritHandle", BOOL))


class SHELLEXECUTEINFOW(ctypes.Structure):
    _fields_ = (
        ("cbSize", DWORD),
        ("fMask", ctypes.c_ulong),
        ("hwnd", wintypes.HWND),
        ("lpVerb", wintypes.LPCWSTR),
        ("lpFile", wintypes.LPCWSTR),
        ("lpParameters", wintypes.LPCWSTR),
        ("lpDirectory", wintypes.LPCWSTR),
        ("nShow", ctypes.c_int),
        ("hInstApp", wintypes.HINSTANCE),
        ("lpIDList", ctypes.c_void_p),
        ("lpClass", wintypes.LPCWSTR),
        ("hkeyClass", wintypes.HKEY),
        ("dwHotKey", DWORD),
        ("hIconOrMonitor", HANDLE),
        ("hProcess", HANDLE),
    )


class PROCESS_BASIC_INFORMATION(ctypes.Structure):
    _fields_ = (
        ("ExitStatus", ctypes.c_long),
        ("PebBaseAddress", ctypes.c_void_p),
        ("AffinityMask", ctypes.c_size_t),
        ("BasePriority", ctypes.c_long),
        ("UniqueProcessId", ctypes.c_size_t),
        ("InheritedFromUniqueProcessId", ctypes.c_size_t),
    )


class NTFS_VOLUME_DATA_BUFFER(ctypes.Structure):
    _fields_ = (
        ("VolumeSerialNumber", ctypes.c_int64),
        ("NumberSectors", ctypes.c_int64),
        ("TotalClusters", ctypes.c_int64),
        ("FreeClusters", ctypes.c_int64),
        ("TotalReserved", ctypes.c_int64),
        ("BytesPerSector", DWORD),
        ("BytesPerCluster", DWORD),
        ("BytesPerFileRecordSegment", DWORD),
        ("ClustersPerFileRecordSegment", DWORD),
        ("MftValidDataLength", ctypes.c_int64),
        ("MftStartLcn", ctypes.c_int64),
        ("Mft2StartLcn", ctypes.c_int64),
        ("MftZoneStart", ctypes.c_int64),
        ("MftZoneEnd", ctypes.c_int64),
    )


def _proto(fn: Any, restype: object, *argtypes: object) -> None:
    fn.restype = restype
    fn.argtypes = argtypes


_LPOV = ctypes.POINTER(OVERLAPPED)
_proto(
    _k32.CreateNamedPipeW, HANDLE, wintypes.LPCWSTR, DWORD, DWORD, DWORD, DWORD, DWORD, DWORD, ctypes.c_void_p
)
_proto(_k32.ConnectNamedPipe, BOOL, HANDLE, _LPOV)
_proto(_k32.CreateFileW, HANDLE, wintypes.LPCWSTR, DWORD, DWORD, ctypes.c_void_p, DWORD, DWORD, HANDLE)
_proto(_k32.ReadFile, BOOL, HANDLE, ctypes.c_void_p, DWORD, LPDWORD, _LPOV)
_proto(_k32.WriteFile, BOOL, HANDLE, ctypes.c_void_p, DWORD, LPDWORD, _LPOV)
_proto(_k32.GetOverlappedResult, BOOL, HANDLE, _LPOV, LPDWORD, BOOL)
_proto(_k32.CancelIoEx, BOOL, HANDLE, _LPOV)
_proto(_k32.CreateEventW, HANDLE, ctypes.c_void_p, BOOL, BOOL, wintypes.LPCWSTR)
_proto(_k32.SetEvent, BOOL, HANDLE)
_proto(_k32.WaitForMultipleObjects, DWORD, DWORD, ctypes.POINTER(HANDLE), BOOL, DWORD)
_proto(_k32.WaitForSingleObject, DWORD, HANDLE, DWORD)
_proto(_k32.WaitNamedPipeW, BOOL, wintypes.LPCWSTR, DWORD)
_proto(_k32.CloseHandle, BOOL, HANDLE)
_proto(_k32.GetNamedPipeClientProcessId, BOOL, HANDLE, ctypes.POINTER(wintypes.ULONG))
_proto(_k32.GetNamedPipeServerProcessId, BOOL, HANDLE, ctypes.POINTER(wintypes.ULONG))
_proto(_k32.GetCurrentProcess, HANDLE)
_proto(_k32.OpenProcess, HANDLE, DWORD, BOOL, DWORD)
_proto(_k32.GetProcessId, DWORD, HANDLE)
_proto(_k32.GetExitCodeProcess, BOOL, HANDLE, LPDWORD)
_proto(_k32.TerminateProcess, BOOL, HANDLE, wintypes.UINT)
_proto(_k32.LocalFree, ctypes.c_void_p, ctypes.c_void_p)
_proto(_k32.GetDriveTypeW, wintypes.UINT, wintypes.LPCWSTR)
_proto(_k32.QueryDosDeviceW, DWORD, wintypes.LPCWSTR, wintypes.LPWSTR, DWORD)
_proto(
    _k32.GetVolumeInformationW,
    BOOL,
    wintypes.LPCWSTR,
    wintypes.LPWSTR,
    DWORD,
    LPDWORD,
    LPDWORD,
    LPDWORD,
    wintypes.LPWSTR,
    DWORD,
)
_proto(_k32.GetDiskFreeSpaceW, BOOL, wintypes.LPCWSTR, LPDWORD, LPDWORD, LPDWORD, LPDWORD)
_proto(
    _k32.DeviceIoControl, BOOL, HANDLE, DWORD, ctypes.c_void_p, DWORD, ctypes.c_void_p, DWORD, LPDWORD, _LPOV
)
_proto(_adv.OpenProcessToken, BOOL, HANDLE, DWORD, ctypes.POINTER(HANDLE))
_proto(_adv.GetTokenInformation, BOOL, HANDLE, ctypes.c_int, ctypes.c_void_p, DWORD, LPDWORD)
_proto(_adv.ConvertSidToStringSidW, BOOL, ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR))
_proto(
    _adv.ConvertStringSecurityDescriptorToSecurityDescriptorW,
    BOOL,
    wintypes.LPCWSTR,
    DWORD,
    ctypes.POINTER(ctypes.c_void_p),
    LPDWORD,
)
_proto(_adv.CreateWellKnownSid, BOOL, ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p, LPDWORD)
_proto(_adv.CheckTokenMembership, BOOL, HANDLE, ctypes.c_void_p, ctypes.POINTER(BOOL))
_proto(_shell32.ShellExecuteExW, BOOL, ctypes.POINTER(SHELLEXECUTEINFOW))
_proto(_ole32.CoInitializeEx, ctypes.c_long, ctypes.c_void_p, DWORD)
_proto(_ole32.CoUninitialize, None)
_proto(
    _ntdll.NtQueryInformationProcess,
    ctypes.c_long,
    HANDLE,
    ctypes.c_int,
    ctypes.c_void_p,
    wintypes.ULONG,
    ctypes.POINTER(wintypes.ULONG),
)


def _winerror(err: int | None = None) -> OSError:
    return ctypes.WinError(ctypes.get_last_error() if err is None else err)


def _valid(handle: int | None) -> bool:
    return handle is not None and handle != INVALID_HANDLE and handle != 0


def _event() -> int:
    handle = _k32.CreateEventW(None, True, False, None)
    if not _valid(handle):
        raise _winerror()
    return int(handle)


def _millis(timeout: float | None) -> int:
    return INFINITE if timeout is None else min(INFINITE - 1, max(0, int(timeout * 1000)))


# --------------------------------------------------------------------------- identity & DACLs


def current_user_sid() -> str:
    """String SID (S-1-5-21-…) of the user this process runs as."""
    token = HANDLE()
    if not _adv.OpenProcessToken(_k32.GetCurrentProcess(), TOKEN_QUERY, ctypes.byref(token)):
        raise _winerror()
    try:
        size = DWORD(0)
        _adv.GetTokenInformation(token, TOKEN_USER_CLASS, None, 0, ctypes.byref(size))
        buf = ctypes.create_string_buffer(size.value)
        if not _adv.GetTokenInformation(token, TOKEN_USER_CLASS, buf, size, ctypes.byref(size)):
            raise _winerror()
        psid = ctypes.cast(buf, ctypes.POINTER(ctypes.c_void_p))[0]  # TOKEN_USER.User.Sid
        text = wintypes.LPWSTR()
        if not _adv.ConvertSidToStringSidW(psid, ctypes.byref(text)):
            raise _winerror()
        try:
            return str(text.value)
        finally:
            _k32.LocalFree(text)
    finally:
        _k32.CloseHandle(token)


def owner_only_sddl() -> str:
    """Protected DACL: full access for the current user and BUILTIN\\Administrators, nobody else.

    The Administrators ACE only matches *elevated* tokens (a UAC-filtered token carries the group
    as deny-only), so it lets an over-the-shoulder elevation (another admin account typed into
    the UAC prompt) reach the pipe and token file without opening them to anyone who could not
    read them anyway.
    """
    return f"D:P(A;;GA;;;{current_user_sid()})(A;;GA;;;BA)"


class SecurityAttributes:
    """SECURITY_ATTRIBUTES built from SDDL; use as a context manager around the Create* call."""

    def __init__(self, sddl: str) -> None:
        self._sd = ctypes.c_void_p()
        if not _adv.ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl, 1, ctypes.byref(self._sd), None
        ):
            raise _winerror()
        self.struct = SECURITY_ATTRIBUTES(ctypes.sizeof(SECURITY_ATTRIBUTES), self._sd, False)

    def __enter__(self) -> SECURITY_ATTRIBUTES:
        return self.struct

    def __exit__(self, *exc: object) -> None:
        _k32.LocalFree(self._sd)


def write_private_file(path: str, data: bytes) -> None:
    """Create ``path`` (must not exist) with :func:`owner_only_sddl` and write ``data``."""
    with SecurityAttributes(owner_only_sddl()) as sa:
        handle = _k32.CreateFileW(
            path, GENERIC_WRITE, 0, ctypes.byref(sa), CREATE_NEW, FILE_ATTRIBUTE_TEMPORARY, None
        )
    if not _valid(handle):
        raise _winerror()
    try:
        written = DWORD(0)
        ok = _k32.WriteFile(handle, data, len(data), ctypes.byref(written), None)
        if not ok or written.value != len(data):
            raise _winerror()
    finally:
        _k32.CloseHandle(handle)


def is_admin() -> bool:
    """Whether BUILTIN\\Administrators is *enabled* in this process's token.

    That is what raw volume access needs. Unlike ``TokenElevation`` it is also false for a
    Safer/restricted token where the group is deny-only.
    """
    size = DWORD(68)  # SECURITY_MAX_SID_SIZE
    sid = ctypes.create_string_buffer(size.value)
    if not _adv.CreateWellKnownSid(26, None, sid, ctypes.byref(size)):  # WinBuiltinAdministratorsSid
        raise _winerror()
    member = BOOL(False)
    if not _adv.CheckTokenMembership(None, sid, ctypes.byref(member)):
        raise _winerror()
    return bool(member.value)


def parent_pid(pid: int) -> int | None:
    """Parent process id of ``pid`` (None when the process cannot be queried)."""
    handle = _k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not _valid(handle):
        return None
    try:
        info = PROCESS_BASIC_INFORMATION()
        status = _ntdll.NtQueryInformationProcess(handle, 0, ctypes.byref(info), ctypes.sizeof(info), None)
        return int(info.InheritedFromUniqueProcessId) if status == 0 else None
    finally:
        _k32.CloseHandle(handle)


# --------------------------------------------------------------------------- named pipes


class Pipe:
    """One end of a byte-mode named pipe opened for overlapped I/O.

    ``read_into`` / ``write`` block the calling thread for at most ``timeout`` seconds without
    progress; ``close`` may be called from any thread and aborts pending I/O at once. An
    OVERLAPPED never outlives its operation: abandoned I/O is cancelled and waited for before
    the call returns, and the handle is closed only when no call is using it.
    """

    def __init__(self, handle: int) -> None:
        self._handle = handle
        self._cancel = _event()
        self._lock = threading.Lock()
        self._users = 0
        self._closed = False

    @property
    def closed(self) -> bool:
        return self._closed

    def close(self) -> None:
        with self._lock:
            if self._closed:
                return
            self._closed = True
            _k32.SetEvent(self._cancel)
            if self._users:
                _k32.CancelIoEx(self._handle, None)  # the last user releases the handles
                return
        self._release()

    def _release(self) -> None:
        _k32.CloseHandle(self._handle)
        _k32.CloseHandle(self._cancel)

    def _enter(self) -> int:
        with self._lock:
            if self._closed:
                raise ConnectionAbortedError(_("管道已关闭"))
            self._users += 1
            return self._handle

    def _leave(self) -> None:
        with self._lock:
            self._users -= 1
            release = self._closed and self._users == 0
        if release:
            self._release()

    def _run(self, start: Callable[[int, OVERLAPPED], bool], timeout: float | None) -> int:
        """Start one overlapped operation and wait for it; returns the byte count."""
        handle = self._enter()
        ov = OVERLAPPED()
        ov.hEvent = _event()
        try:
            if not start(handle, ov):
                err = ctypes.get_last_error()
                if err == ERROR_PIPE_CONNECTED:  # ConnectNamedPipe: the client was faster
                    return 0
                if err != ERROR_IO_PENDING:
                    raise _closed_or_error(err)
            waits = (HANDLE * 2)(ov.hEvent, self._cancel)
            outcome = _k32.WaitForMultipleObjects(2, waits, False, _millis(timeout))
            if outcome != WAIT_OBJECT_0:
                _k32.CancelIoEx(handle, ctypes.byref(ov))
            count = DWORD(0)
            if _k32.GetOverlappedResult(handle, ctypes.byref(ov), ctypes.byref(count), True):
                return int(count.value)  # completed (possibly racing the cancel): keep the data
            err = ctypes.get_last_error()
            if err == ERROR_OPERATION_ABORTED:
                if outcome == WAIT_TIMEOUT:
                    raise TimeoutError(_("管道读写超时"))
                raise ConnectionAbortedError(_("管道已关闭"))
            raise _closed_or_error(err)
        finally:
            _k32.CloseHandle(ov.hEvent)
            self._leave()

    def accept(self, timeout: float | None) -> None:
        """Server end: wait for the client to connect."""
        self._run(lambda h, ov: bool(_k32.ConnectNamedPipe(h, ctypes.byref(ov))), timeout)

    def read_into(self, view: memoryview, timeout: float | None) -> int:
        """Read at least one byte into ``view``; EOFError when the other end has closed."""
        arr = np.frombuffer(view, dtype=np.uint8)
        size = min(len(arr), MAX_IO_CHUNK)
        addr = arr.ctypes.data
        try:
            got = self._run(lambda h, ov: bool(_k32.ReadFile(h, addr, size, None, ctypes.byref(ov))), timeout)
        except BrokenPipeError:
            raise EOFError(_("管道已断开")) from None
        if got == 0:
            raise EOFError(_("管道已断开"))
        return got

    def write(self, view: memoryview, timeout: float | None) -> int:
        arr = np.frombuffer(view, dtype=np.uint8)
        size = min(len(arr), MAX_IO_CHUNK)
        addr = arr.ctypes.data
        return self._run(lambda h, ov: bool(_k32.WriteFile(h, addr, size, None, ctypes.byref(ov))), timeout)

    def _pid(self, query: Callable[[int, object], int]) -> int:
        handle = self._enter()
        try:
            pid = wintypes.ULONG(0)
            if not query(handle, ctypes.byref(pid)):
                raise _winerror()
            return int(pid.value)
        finally:
            self._leave()

    def client_pid(self) -> int:
        return self._pid(_k32.GetNamedPipeClientProcessId)

    def server_pid(self) -> int:
        return self._pid(_k32.GetNamedPipeServerProcessId)


def _closed_or_error(err: int) -> OSError:
    if err in _CLOSED_ERRORS:
        return BrokenPipeError(err, _("管道已断开"))
    return _winerror(err)


def create_pipe_server(name: str) -> Pipe:
    """The single instance of a new local-only pipe, accessible to this user (and elevated admins)."""
    with SecurityAttributes(owner_only_sddl()) as sa:
        handle = _k32.CreateNamedPipeW(
            name,
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            ctypes.byref(sa),
        )
    if not _valid(handle):
        raise _winerror()
    return Pipe(int(handle))


def connect_pipe(name: str, timeout: float) -> Pipe:
    """Client end (the helper). Identification-level only: the server can never impersonate us."""
    flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION
    handle = _k32.CreateFileW(name, GENERIC_READ | GENERIC_WRITE, 0, None, OPEN_EXISTING, flags, None)
    if not _valid(handle) and ctypes.get_last_error() == ERROR_PIPE_BUSY:
        _k32.WaitNamedPipeW(name, _millis(timeout))
        handle = _k32.CreateFileW(name, GENERIC_READ | GENERIC_WRITE, 0, None, OPEN_EXISTING, flags, None)
    if not _valid(handle):
        raise _winerror()
    return Pipe(int(handle))


# --------------------------------------------------------------------------- processes


class ProcessHandle:
    """A process we started through a raw Win32 handle (ShellExecuteEx, CreateProcessAsUser)."""

    def __init__(self, handle: int, pid: int | None = None) -> None:
        self._handle: int | None = handle
        self._code: int | None = None  # exit code, remembered past close()
        self.pid = pid if pid is not None else int(_k32.GetProcessId(handle))

    def poll(self) -> int | None:
        if self._handle is None or self._code is not None:
            return self._code
        if _k32.WaitForSingleObject(self._handle, 0) != WAIT_OBJECT_0:
            return None
        code = DWORD(0)
        _k32.GetExitCodeProcess(self._handle, ctypes.byref(code))
        self._code = int(code.value)
        return self._code

    def wait(self, timeout: float) -> int | None:
        if self._handle is not None:
            _k32.WaitForSingleObject(self._handle, _millis(timeout))
        return self.poll()

    def terminate(self) -> None:
        if self._handle is not None and self.poll() is None:
            _k32.TerminateProcess(self._handle, 1)

    def close(self) -> None:
        if self._handle is not None:
            self.poll()
            _k32.CloseHandle(self._handle)
            self._handle = None


def shell_execute_info(params: ShellExecuteParams) -> SHELLEXECUTEINFOW:
    info = SHELLEXECUTEINFOW()
    info.cbSize = ctypes.sizeof(SHELLEXECUTEINFOW)
    info.fMask = params.mask
    info.lpVerb = params.verb
    info.lpFile = params.file
    info.lpParameters = params.parameters
    info.lpDirectory = params.directory
    info.nShow = params.show
    return info


def shell_execute(params: ShellExecuteParams) -> ProcessHandle:
    """ShellExecuteExW on the calling thread (blocks while the UAC prompt is open).

    Raises PermissionError(ERROR_CANCELLED) when the user declines the prompt.
    """
    com = _ole32.CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
    try:
        info = shell_execute_info(params)
        if not _shell32.ShellExecuteExW(ctypes.byref(info)):
            err = ctypes.get_last_error()
            if err == ERROR_CANCELLED:
                raise PermissionError(ERROR_CANCELLED, _("用户拒绝了管理员授权"))
            raise _winerror(err)
        if not _valid(info.hProcess):
            raise OSError(_("ShellExecuteExW 没有返回进程句柄"))
        return ProcessHandle(int(info.hProcess))
    finally:
        if com >= 0:  # S_OK / S_FALSE
            _ole32.CoUninitialize()


# --------------------------------------------------------------------------- volumes


def volume_details(volume: str) -> dict[str, int | str]:
    """Unprivileged facts about ``X:``: drive type, DOS device target, filesystem, geometry."""
    root = volume.upper() + "\\"
    target = ctypes.create_unicode_buffer(1024)
    device = target.value if _k32.QueryDosDeviceW(volume.upper(), target, 1024) else ""
    fs = ctypes.create_unicode_buffer(261)
    label = ctypes.create_unicode_buffer(261)
    serial, maxlen, fsflags = DWORD(0), DWORD(0), DWORD(0)
    ok = _k32.GetVolumeInformationW(
        root, label, 261, ctypes.byref(serial), ctypes.byref(maxlen), ctypes.byref(fsflags), fs, 261
    )
    spc, bps, nfree, nclusters = DWORD(0), DWORD(0), DWORD(0), DWORD(0)
    geometry = _k32.GetDiskFreeSpaceW(
        root, ctypes.byref(spc), ctypes.byref(bps), ctypes.byref(nfree), ctypes.byref(nclusters)
    )
    return {
        "drive_type": int(_k32.GetDriveTypeW(root)),
        "device": device,
        "fs_type": fs.value if ok else "",
        "label": label.value if ok else "",
        "serial": f"{serial.value:08X}" if ok else "",
        "bytes_per_sector": int(bps.value) if geometry else 0,
        "cluster_size": int(spc.value * bps.value) if geometry else 0,
    }


def ntfs_volume_data(volume: str) -> dict[str, int]:
    """FSCTL_GET_NTFS_VOLUME_DATA on ``\\\\.\\X:`` opened read-only (needs administrator rights)."""
    handle = _k32.CreateFileW(
        "\\\\.\\" + volume.upper(),
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        0,
        None,
    )
    if not _valid(handle):
        raise _winerror()
    try:
        data = NTFS_VOLUME_DATA_BUFFER()
        got = DWORD(0)
        if not _k32.DeviceIoControl(
            handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            ctypes.byref(data),
            ctypes.sizeof(data),
            ctypes.byref(got),
            None,
        ):
            raise _winerror()
    finally:
        _k32.CloseHandle(handle)
    return {
        "record_size": int(data.BytesPerFileRecordSegment),
        "total_clusters": int(data.TotalClusters),
        "free_clusters": int(data.FreeClusters),
        "mft_start_lcn": int(data.MftStartLcn),
        "mft_valid_data_length": int(data.MftValidDataLength),
    }

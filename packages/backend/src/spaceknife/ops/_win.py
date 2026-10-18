"""Win32 plumbing for file operations (ctypes; imported lazily, only called on Windows).

* Restart Manager: which processes hold files open (``RmGetList``) and their friendly names.
* The shell's copy engine (``IFileOperation``) moving items to the recycle bin without any window:
  ``FOF_NO_UI`` plus ``FOF_ALLOWUNDO``, never ``FOF_WANTNUKEWARNING``. A progress sink refuses every
  deletion the engine announces without "recycle" (``PreDeleteItem`` lacking
  ``TSF_DELETE_RECYCLE_IF_POSSIBLE``: the item would be destroyed) and reports where each recycled
  item landed (``PostDeleteItem``'s newly created item: the ``$R`` file).
* Recycle-bin facts: per-volume settings (HKCU ``BitBucket``), the current user's SID, the bin's
  current usage. Nothing here shows a window or asks for elevation.
"""

from __future__ import annotations

import ctypes
import os
import sys
import threading
import uuid
from collections.abc import Callable, Sequence
from ctypes import wintypes
from dataclasses import dataclass, field
from functools import cache
from typing import Any

S_OK = 0
E_ABORT = -2147467260  # 0x80004004
E_NOINTERFACE = -2147467262  # 0x80004002
ERROR_MORE_DATA = 234
ERROR_SHARING_VIOLATION = 32
ERROR_LOCK_VIOLATION = 33
ERROR_ACCESS_DENIED = 5
ERROR_FILE_NOT_FOUND = 2
ERROR_PATH_NOT_FOUND = 3

DRIVE_REMOVABLE = 2
DRIVE_FIXED = 3
DRIVE_REMOTE = 4

TSF_DELETE_RECYCLE_IF_POSSIBLE = 0x80
FOF_SILENT = 0x4
FOF_NOCONFIRMATION = 0x10
FOF_ALLOWUNDO = 0x40
FOF_NOCONFIRMMKDIR = 0x200
FOF_NOERRORUI = 0x400
FOF_NORECURSEREPARSE = 0x8000
FOFX_RECYCLEONDELETE = 0x00080000
RECYCLE_FLAGS = (
    FOF_ALLOWUNDO
    | FOF_NOCONFIRMATION
    | FOF_NOERRORUI
    | FOF_SILENT
    | FOF_NOCONFIRMMKDIR
    | FOF_NORECURSEREPARSE
    | FOFX_RECYCLEONDELETE
)  # no FOF_WANTNUKEWARNING: it would show a dialog (and overrides FOF_NOCONFIRMATION)
SIGDN_FILESYSPATH = 0x80058000
COINIT_APARTMENTTHREADED = 0x2
COINIT_DISABLE_OLE1DDE = 0x4
CLSCTX_ALL = 0x17

CCH_RM_SESSION_KEY = 32
CCH_RM_MAX_APP_NAME = 255
CCH_RM_MAX_SVC_NAME = 63
RM_REGISTER_CHUNK = 1000  # file names per RmRegisterResources call


def hresult_code(hr: int) -> int:
    """The Win32 error inside an ``HRESULT_FROM_WIN32`` value (else the HRESULT itself)."""
    value = hr & 0xFFFFFFFF
    return value & 0xFFFF if value >> 16 == 0x8007 else value


# --------------------------------------------------------------------------- DLLs


@cache
def _k32() -> Any:
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    k32.GetVolumePathNameW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD]
    k32.GetVolumePathNameW.restype = wintypes.BOOL
    k32.GetVolumeNameForVolumeMountPointW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD]
    k32.GetVolumeNameForVolumeMountPointW.restype = wintypes.BOOL
    k32.GetDriveTypeW.argtypes = [wintypes.LPCWSTR]
    k32.GetDriveTypeW.restype = wintypes.UINT
    k32.GetCurrentProcess.restype = wintypes.HANDLE
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    k32.LocalFree.argtypes = [ctypes.c_void_p]
    k32.OpenProcess.restype = wintypes.HANDLE
    k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    k32.QueryFullProcessImageNameW.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        wintypes.LPWSTR,
        ctypes.POINTER(wintypes.DWORD),
    ]
    k32.QueryFullProcessImageNameW.restype = wintypes.BOOL
    return k32


@cache
def _advapi() -> Any:
    adv = ctypes.WinDLL("advapi32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    adv.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    adv.OpenProcessToken.restype = wintypes.BOOL
    adv.GetTokenInformation.argtypes = [
        wintypes.HANDLE,
        ctypes.c_int,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
    ]
    adv.GetTokenInformation.restype = wintypes.BOOL
    adv.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
    adv.ConvertSidToStringSidW.restype = wintypes.BOOL
    return adv


@cache
def _rm() -> Any:
    rm = ctypes.WinDLL("rstrtmgr")  # type: ignore[attr-defined,unused-ignore]
    rm.RmStartSession.argtypes = [ctypes.POINTER(wintypes.DWORD), wintypes.DWORD, wintypes.LPWSTR]
    rm.RmStartSession.restype = wintypes.DWORD
    rm.RmRegisterResources.argtypes = [
        wintypes.DWORD,
        wintypes.UINT,
        ctypes.POINTER(wintypes.LPCWSTR),
        wintypes.UINT,
        ctypes.c_void_p,
        wintypes.UINT,
        ctypes.c_void_p,
    ]
    rm.RmRegisterResources.restype = wintypes.DWORD
    rm.RmGetList.argtypes = [
        wintypes.DWORD,
        ctypes.POINTER(wintypes.UINT),
        ctypes.POINTER(wintypes.UINT),
        ctypes.c_void_p,
        ctypes.POINTER(wintypes.DWORD),
    ]
    rm.RmGetList.restype = wintypes.DWORD
    rm.RmEndSession.argtypes = [wintypes.DWORD]
    rm.RmEndSession.restype = wintypes.DWORD
    return rm


@cache
def _ole32() -> Any:
    ole = ctypes.WinDLL("ole32")  # type: ignore[attr-defined,unused-ignore]
    ole.CoInitializeEx.argtypes = [ctypes.c_void_p, wintypes.DWORD]
    ole.CoInitializeEx.restype = ctypes.c_long
    ole.CoUninitialize.argtypes = []
    ole.CoUninitialize.restype = None
    ole.CoCreateInstance.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    ole.CoCreateInstance.restype = ctypes.c_long
    ole.CoTaskMemFree.argtypes = [ctypes.c_void_p]
    ole.CoTaskMemFree.restype = None
    return ole


@cache
def _shell32() -> Any:
    sh = ctypes.WinDLL("shell32")  # type: ignore[attr-defined,unused-ignore]
    sh.SHCreateItemFromParsingName.argtypes = [
        wintypes.LPCWSTR,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    sh.SHCreateItemFromParsingName.restype = ctypes.c_long
    sh.SHQueryRecycleBinW.argtypes = [wintypes.LPCWSTR, ctypes.c_void_p]
    sh.SHQueryRecycleBinW.restype = ctypes.c_long
    sh.SHChangeNotify.argtypes = [ctypes.c_long, wintypes.UINT, ctypes.c_void_p, ctypes.c_void_p]
    sh.SHChangeNotify.restype = None
    return sh


def _last_error() -> int:
    return int(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]


# --------------------------------------------------------------------------- volumes


def volume_root(path: str) -> str:
    """Mount point of the volume holding ``path`` (``C:\\``, ``C:\\mnt\\disk\\``, ``\\\\host\\share\\``)."""
    buf = ctypes.create_unicode_buffer(1024)
    if not _k32().GetVolumePathNameW(os.path.abspath(path), buf, 1024):
        raise ctypes.WinError(_last_error())  # type: ignore[attr-defined,unused-ignore]
    root = buf.value
    return root if root.endswith("\\") else root + "\\"


def volume_guid(root: str) -> str:
    """``{GUID}`` of a mounted volume, or "" (network shares, SUBST drives)."""
    buf = ctypes.create_unicode_buffer(128)
    if not _k32().GetVolumeNameForVolumeMountPointW(root, buf, 128):
        return ""
    name = buf.value  # \\?\Volume{GUID}\
    start, end = name.find("{"), name.find("}")
    return name[start : end + 1] if 0 <= start < end else ""


def drive_type(root: str) -> int:
    return int(_k32().GetDriveTypeW(root))


@dataclass(frozen=True, slots=True)
class BinSettings:
    """What the shell does with an item deleted on one volume."""

    nuke: bool  # NukeOnDelete or the NoRecycleFiles policy: deleted items are destroyed
    max_bytes: int  # the bin's capacity on the volume: a larger item is destroyed, not recycled
    configured: bool  # read from the registry (False: Windows' default for the volume size)


_BITBUCKET = r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume"
_POLICIES = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer"


def _dword(hive: int, key: str, name: str) -> int | None:
    if sys.platform == "win32":  # the registry (and its type stubs) exist on Windows only
        import winreg

        try:
            with winreg.OpenKey(hive, key) as handle:
                value, kind = winreg.QueryValueEx(handle, name)
        except OSError:
            return None
        return int(value) if kind == winreg.REG_DWORD else None
    return None


def default_bin_bytes(total: int) -> int:
    """Windows' default bin capacity: 10 % of the first 40 GB plus 5 % of the rest (MB granular)."""
    mb = total // 2**20
    first = min(mb, 40 * 1024)
    return (first // 10 + (mb - first) // 20) * 2**20


def bin_settings(guid: str, total: int) -> BinSettings:
    """The recycle-bin settings of the volume ``{guid}`` (``total`` bytes): HKCU BitBucket, policies."""
    if sys.platform != "win32":  # the registry (and its type stubs) exist on Windows only
        return BinSettings(nuke=False, max_bytes=default_bin_bytes(total), configured=False)
    import winreg

    nuke = any(
        _dword(hive, _POLICIES, "NoRecycleFiles") == 1
        for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE)
    )
    key = rf"{_BITBUCKET}\{guid}"
    nuke_value = _dword(winreg.HKEY_CURRENT_USER, key, "NukeOnDelete")
    max_mb = _dword(winreg.HKEY_CURRENT_USER, key, "MaxCapacity")
    configured = max_mb is not None
    max_bytes = max_mb * 2**20 if max_mb is not None else default_bin_bytes(total)
    for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
        percent = _dword(hive, _POLICIES, "RecycleBinSize")  # policy: capacity in % of the volume
        if percent is not None:
            max_bytes = min(max_bytes, total * max(0, min(percent, 100)) // 100)
    return BinSettings(nuke=nuke or nuke_value == 1, max_bytes=max_bytes, configured=configured)


class _SHQUERYRBINFO(ctypes.Structure):
    _pack_ = 1 if ctypes.sizeof(ctypes.c_void_p) == 4 else 8  # shellapi.h packs it on 32-bit only
    _fields_ = (
        ("cbSize", wintypes.DWORD),
        ("i64Size", ctypes.c_longlong),
        ("i64NumItems", ctypes.c_longlong),
    )


def bin_usage(root: str) -> tuple[int, int]:
    """(bytes, items) in the recycle bin of the volume at ``root``; (-1, -1) when unknown."""
    info = _SHQUERYRBINFO()
    info.cbSize = ctypes.sizeof(info)
    if _shell32().SHQueryRecycleBinW(root, ctypes.byref(info)) != S_OK:
        return -1, -1
    return int(info.i64Size), int(info.i64NumItems)


@cache
def user_sid() -> str:
    """String SID of the user this process runs as (the name of its folder in ``$Recycle.Bin``)."""
    k32, adv = _k32(), _advapi()
    token = wintypes.HANDLE()
    if not adv.OpenProcessToken(k32.GetCurrentProcess(), 0x0008, ctypes.byref(token)):  # TOKEN_QUERY
        raise ctypes.WinError(_last_error())  # type: ignore[attr-defined,unused-ignore]
    try:
        need = wintypes.DWORD(0)
        adv.GetTokenInformation(token, 1, None, 0, ctypes.byref(need))  # TokenUser
        buf = ctypes.create_string_buffer(max(need.value, 64))
        if not adv.GetTokenInformation(token, 1, buf, len(buf), ctypes.byref(need)):
            raise ctypes.WinError(_last_error())  # type: ignore[attr-defined,unused-ignore]
        sid = ctypes.cast(buf, ctypes.POINTER(ctypes.c_void_p))[0]  # TOKEN_USER.User.Sid
        text = ctypes.c_void_p()
        if not adv.ConvertSidToStringSidW(sid, ctypes.byref(text)):
            raise ctypes.WinError(_last_error())  # type: ignore[attr-defined,unused-ignore]
        try:
            return ctypes.wstring_at(text.value or 0)
        finally:
            k32.LocalFree(text)
    finally:
        k32.CloseHandle(token)


def notify_changed(path: str, *, folder: bool) -> None:
    """Tell Explorer windows that ``path`` appeared (SHChangeNotify; no window, never blocks)."""
    sh = _shell32()
    event = 0x00000008 if folder else 0x00000002  # SHCNE_MKDIR / SHCNE_CREATE
    sh.SHChangeNotify(event, 0x0005, ctypes.c_wchar_p(path), None)  # SHCNF_PATHW
    sh.SHChangeNotify(0x00001000, 0x0005, ctypes.c_wchar_p(os.path.dirname(path)), None)  # SHCNE_UPDATEDIR


# --------------------------------------------------------------------------- Restart Manager


class _RM_UNIQUE_PROCESS(ctypes.Structure):
    _fields_ = (("dwProcessId", wintypes.DWORD), ("ProcessStartTime", wintypes.FILETIME))


class _RM_PROCESS_INFO(ctypes.Structure):
    _fields_ = (
        ("Process", _RM_UNIQUE_PROCESS),
        ("strAppName", ctypes.c_wchar * (CCH_RM_MAX_APP_NAME + 1)),
        ("strServiceShortName", ctypes.c_wchar * (CCH_RM_MAX_SVC_NAME + 1)),
        ("ApplicationType", ctypes.c_int),
        ("AppStatus", wintypes.ULONG),
        ("TSSessionId", wintypes.DWORD),
        ("bRestartable", wintypes.BOOL),
    )


def _image_name(pid: int) -> str:
    k32 = _k32()
    handle = k32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        return ""
    try:
        buf = ctypes.create_unicode_buffer(1024)
        size = wintypes.DWORD(1024)
        if not k32.QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size)):
            return ""
        return os.path.basename(buf.value)
    finally:
        k32.CloseHandle(handle)


def rm_lockers(files: Sequence[str]) -> list[tuple[int, str]]:
    """``(pid, friendly name)`` of every process using any of ``files`` (one Restart Manager session).

    Raises OSError when the Restart Manager cannot be used (no session available, bad names).
    """
    if not files:
        return []
    rm = _rm()
    session = wintypes.DWORD(0)
    key = ctypes.create_unicode_buffer(CCH_RM_SESSION_KEY + 1)
    err = rm.RmStartSession(ctypes.byref(session), 0, key)
    if err:
        raise OSError(err, f"RmStartSession failed ({err})")
    try:
        for at in range(0, len(files), RM_REGISTER_CHUNK):
            chunk = files[at : at + RM_REGISTER_CHUNK]
            names = (wintypes.LPCWSTR * len(chunk))(*chunk)
            err = rm.RmRegisterResources(session, len(chunk), names, 0, None, 0, None)
            if err:
                raise OSError(err, f"RmRegisterResources failed ({err})")
        needed, count, reasons = wintypes.UINT(0), wintypes.UINT(0), wintypes.DWORD(0)
        for _ in range(8):  # the list may grow between the calls
            buf = (_RM_PROCESS_INFO * max(1, needed.value))()
            count.value = needed.value
            err = rm.RmGetList(session, ctypes.byref(needed), ctypes.byref(count), buf, ctypes.byref(reasons))
            if err == 0:
                out = []
                for info in buf[: count.value]:
                    pid = int(info.Process.dwProcessId)
                    name = info.strAppName or info.strServiceShortName or _image_name(pid) or f"PID {pid}"
                    out.append((pid, name))
                return out
            if err != ERROR_MORE_DATA:
                raise OSError(err, f"RmGetList failed ({err})")
        raise OSError(ERROR_MORE_DATA, "RmGetList kept growing")
    finally:
        rm.RmEndSession(session)


# --------------------------------------------------------------------------- COM helpers


class _GUID(ctypes.Structure):
    _fields_ = (
        ("Data1", wintypes.DWORD),
        ("Data2", wintypes.WORD),
        ("Data3", wintypes.WORD),
        ("Data4", ctypes.c_ubyte * 8),
    )


def _guid(text: str) -> _GUID:
    guid = _GUID()
    ctypes.memmove(ctypes.byref(guid), uuid.UUID(text).bytes_le, 16)
    return guid


CLSID_FileOperation = _guid("3ad05575-8857-4850-9277-11b85bdb8e09")
IID_IFileOperation = _guid("947aab5f-0a5c-4c13-b4d6-4bf7836fc9f8")
IID_IFileOperationProgressSink = _guid("04b0f1a7-9490-44bc-96e1-4296a31252e2")
IID_IShellItem = _guid("43826d1e-e718-42ee-bc55-a1e261c37bfe")
IID_IUnknown = _guid("00000000-0000-0000-c000-000000000046")


def _vcall(obj: int, index: int, argtypes: tuple[Any, ...], *args: Any) -> int:
    """Call method ``index`` of the COM object at ``obj``; returns the HRESULT (or ULONG)."""
    vtbl = ctypes.cast(obj, ctypes.POINTER(ctypes.c_void_p))[0]
    fn = ctypes.cast(vtbl, ctypes.POINTER(ctypes.c_void_p))[index]
    proto = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, *argtypes)  # type: ignore[attr-defined,unused-ignore]
    return int(proto(fn)(obj, *args))


def _release(obj: int | None) -> None:
    if obj:
        _vcall(obj, 2, ())


def _item_path(item: int | None) -> str | None:
    """File-system path of an IShellItem (borrowed pointer), or None."""
    if not item:
        return None
    out = ctypes.c_void_p()
    if (
        _vcall(
            item, 5, (ctypes.c_uint, ctypes.POINTER(ctypes.c_void_p)), SIGDN_FILESYSPATH, ctypes.byref(out)
        )
        < 0
    ):
        return None
    try:
        return ctypes.wstring_at(out.value or 0)
    finally:
        _ole32().CoTaskMemFree(out)


_V = ctypes.c_void_p
_W = ctypes.c_wchar_p
_D = wintypes.DWORD
_H = ctypes.c_long
# IFileOperationProgressSink, in vtable order: (name, argument types after `this`)
_SINK_METHODS: tuple[tuple[str, tuple[Any, ...]], ...] = (
    ("QueryInterface", (_V, ctypes.POINTER(_V))),
    ("AddRef", ()),
    ("Release", ()),
    ("StartOperations", ()),
    ("FinishOperations", (_H,)),
    ("PreRenameItem", (_D, _V, _W)),
    ("PostRenameItem", (_D, _V, _W, _H, _V)),
    ("PreMoveItem", (_D, _V, _V, _W)),
    ("PostMoveItem", (_D, _V, _V, _W, _H, _V)),
    ("PreCopyItem", (_D, _V, _V, _W)),
    ("PostCopyItem", (_D, _V, _V, _W, _H, _V)),
    ("PreDeleteItem", (_D, _V)),
    ("PostDeleteItem", (_D, _V, _H, _V)),
    ("PreNewItem", (_D, _V, _W)),
    ("PostNewItem", (_D, _V, _W, _W, _D, _H, _V)),
    ("UpdateProgress", (ctypes.c_uint, ctypes.c_uint)),
    ("ResetTimer", ()),
    ("PauseTimer", ()),
    ("ResumeTimer", ()),
)


@dataclass(slots=True)
class Recycled:
    """What the engine did with one item handed to :func:`recycle`."""

    path: str
    hr: int  # HRESULT of the deletion (0 = done); E_ABORT when refused or cancelled
    recycled: str | None = None  # the item's new path in the bin ($R…), None if not recycled
    refused: bool = False  # the engine wanted to destroy it (no recycle): refused, untouched
    reached: bool = False  # the engine got to this item at all


class _Sink:
    """An IFileOperationProgressSink in ctypes: guards against destruction, records locations."""

    def __init__(self, cancel: Callable[[], bool] | None, on_item: Callable[[Recycled], None] | None) -> None:
        self.cancel = cancel
        self.on_item = on_item
        self.cancelled = False
        self.items: dict[str, Recycled] = {}
        self.refused_other: list[str] = []  # destruction refused for an item we did not queue
        self.unmatched: list[tuple[str, str | None]] = []  # deletions reported for paths we did not queue
        self._callbacks: list[Any] = []
        impl: dict[str, Callable[..., int]] = {
            "QueryInterface": self._query_interface,
            "AddRef": lambda this: 1,
            "Release": lambda this: 1,
            "PreDeleteItem": self._pre_delete,
            "PostDeleteItem": self._post_delete,
        }
        addresses = []
        for name, argtypes in _SINK_METHODS:
            proto = ctypes.WINFUNCTYPE(ctypes.c_long, _V, *argtypes)  # type: ignore[attr-defined,unused-ignore]
            fn = impl.get(name, self._ok)
            callback = proto(fn)
            self._callbacks.append(callback)
            addresses.append(ctypes.cast(callback, _V).value)
        self._vtable = (_V * len(addresses))(*addresses)
        self._object = _V(ctypes.addressof(self._vtable))

    @property
    def pointer(self) -> int:
        return ctypes.addressof(self._object)

    @staticmethod
    def _ok(*_args: Any) -> int:
        return S_OK

    def _query_interface(self, this: int, riid: int, ppv: Any) -> int:
        try:
            wanted = ctypes.string_at(riid, 16)
            if wanted in (bytes(IID_IUnknown), bytes(IID_IFileOperationProgressSink)):
                ppv[0] = this
                return S_OK
            ppv[0] = None
            return E_NOINTERFACE
        except BaseException:
            return E_NOINTERFACE

    def _pre_delete(self, this: int, flags: int, item: int) -> int:
        try:
            path = _item_path(item) or ""
            entry = self.items.get(_key(path))
            if entry is not None:
                entry.reached = True
            if self.cancel is not None and self.cancel():
                self.cancelled = True
                return E_ABORT
            if not flags & TSF_DELETE_RECYCLE_IF_POSSIBLE:  # would be destroyed: never let it happen
                if entry is not None:
                    entry.refused = True
                else:
                    self.refused_other.append(path)
                return E_ABORT
            return S_OK
        except BaseException:  # never let an error turn into "go ahead"
            return E_ABORT

    def _post_delete(self, this: int, flags: int, item: int, hr: int, created: int) -> int:
        try:
            path = _item_path(item) or ""
            entry = self.items.get(_key(path))
            if entry is None:
                self.unmatched.append((path, _item_path(created)))
            elif not entry.refused:
                entry.reached = True
                entry.hr = int(hr)
                entry.recycled = _item_path(created)
                if self.on_item is not None:
                    self.on_item(entry)
            return S_OK
        except BaseException:
            return S_OK


def _key(path: str) -> str:
    return os.path.normcase(os.path.normpath(path)) if path else ""


@dataclass(slots=True)
class RecycleRun:
    """One shell operation: an entry per path, the operation's HRESULT and what was refused."""

    items: list[Recycled]
    hr: int
    cancelled: bool = False
    refused: list[str] = field(default_factory=list)  # paths the engine wanted to destroy (any level)
    unmatched: list[tuple[str, str | None]] = field(default_factory=list)  # (path, new path) not queued


def _recycle_sta(
    paths: Sequence[str],
    cancel: Callable[[], bool] | None,
    on_item: Callable[[Recycled], None] | None,
) -> RecycleRun:
    ole, sh = _ole32(), _shell32()
    init = ole.CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
    sink = _Sink(cancel, on_item)
    entries = [Recycled(path=p, hr=E_ABORT) for p in paths]
    for entry in entries:
        sink.items[_key(entry.path)] = entry
    try:
        op = ctypes.c_void_p()
        hr = ole.CoCreateInstance(
            ctypes.byref(CLSID_FileOperation),
            None,
            CLSCTX_ALL,
            ctypes.byref(IID_IFileOperation),
            ctypes.byref(op),
        )
        if hr < 0 or not op.value:
            return RecycleRun(entries, int(hr))
        obj = op.value
        try:
            hr = _vcall(obj, 5, (_D,), RECYCLE_FLAGS)  # SetOperationFlags
            if hr < 0:
                return RecycleRun(entries, hr)
            cookie = _D(0)
            hr = _vcall(obj, 3, (_V, ctypes.POINTER(_D)), sink.pointer, ctypes.byref(cookie))  # Advise
            if hr < 0:
                return RecycleRun(entries, hr)
            queued = 0
            for entry in entries:
                item = ctypes.c_void_p()
                hr = sh.SHCreateItemFromParsingName(
                    entry.path, None, ctypes.byref(IID_IShellItem), ctypes.byref(item)
                )
                if hr < 0 or not item.value:
                    entry.hr = int(hr)
                    entry.reached = True
                    continue
                try:
                    hr = _vcall(obj, 18, (_V, _V), item.value, None)  # DeleteItem
                finally:
                    _release(item.value)
                if hr < 0:
                    entry.hr = hr
                    entry.reached = True
                else:
                    queued += 1
            result = _vcall(obj, 21, ()) if queued else S_OK  # PerformOperations
            _vcall(obj, 4, (_D,), cookie.value)  # Unadvise
            refused = [e.path for e in entries if e.refused] + sink.refused_other
            return RecycleRun(entries, result, sink.cancelled, refused, sink.unmatched)
        finally:
            _release(obj)
    finally:
        if init >= 0:  # S_OK / S_FALSE: balance it (RPC_E_CHANGED_MODE: not ours to undo)
            ole.CoUninitialize()


def recycle(
    paths: Sequence[str],
    *,
    cancel: Callable[[], bool] | None = None,
    on_item: Callable[[Recycled], None] | None = None,
) -> RecycleRun:
    """Move ``paths`` to the recycle bin in one shell operation, on a fresh STA thread.

    Returns one :class:`Recycled` per path (same order) and the operation's HRESULT. ``on_item``
    is called on that thread right after each item was recycled. ``cancel`` is polled before each
    item: once it returns True the rest is not touched. A refused destruction aborts the whole
    operation (what was not reached yet is untouched; the caller retries it without the refused).
    """
    box: list[RecycleRun] = []
    errors: list[BaseException] = []

    def run() -> None:
        try:
            box.append(_recycle_sta(paths, cancel, on_item))
        except BaseException as exc:
            errors.append(exc)

    thread = threading.Thread(target=run, name="spaceknife-recycle", daemon=True)
    thread.start()
    thread.join()
    if errors:
        raise errors[0]
    return box[0]

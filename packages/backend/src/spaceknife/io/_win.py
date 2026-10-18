"""Shared Win32 plumbing for the disk probes (ctypes prototypes, device IOCTLs without elevation).

Imported lazily, and only called on Windows.
"""

from __future__ import annotations

import ctypes
from ctypes import wintypes
from functools import cache
from typing import Any

INVALID_HANDLE = wintypes.HANDLE(-1).value
ERROR_INSUFFICIENT_BUFFER = 122
ERROR_MORE_DATA = 234

IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS = 0x00560000
IOCTL_STORAGE_GET_DEVICE_NUMBER = 0x002D1080
IOCTL_DISK_PERFORMANCE = 0x00070020
FILE_DEVICE_DISK = 7


@cache
def kernel32() -> Any:
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    k32.CreateFileW.restype = wintypes.HANDLE
    k32.CreateFileW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    ]
    k32.DeviceIoControl.restype = wintypes.BOOL
    k32.DeviceIoControl.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    ]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    k32.GetVolumePathNameW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD]
    k32.GetVolumeNameForVolumeMountPointW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD]
    k32.GetDriveTypeW.argtypes = [wintypes.LPCWSTR]
    k32.GetDriveTypeW.restype = wintypes.UINT
    k32.GetLogicalDrives.restype = wintypes.DWORD
    k32.GetCurrentProcess.restype = wintypes.HANDLE
    k32.GetCurrentThread.restype = wintypes.HANDLE
    k32.OpenProcess.restype = wintypes.HANDLE
    k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    k32.SetPriorityClass.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    k32.SetThreadPriority.argtypes = [wintypes.HANDLE, ctypes.c_int]
    return k32


def last_error() -> int:
    return int(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]


def device_ioctl(path: str, code: int, size: int = 1024, *, grow: bool = True) -> bytes | None:
    """``DeviceIoControl`` on a device opened with access 0 (enough for FILE_ANY_ACCESS IOCTLs).

    Returns the output bytes, or None when the device cannot be opened or the IOCTL fails.
    """
    k32 = kernel32()
    h = k32.CreateFileW(path, 0, 3, None, 3, 0, None)  # share read|write, OPEN_EXISTING
    if not h or h == INVALID_HANDLE:
        return None
    try:
        while True:
            out = ctypes.create_string_buffer(size)
            got = wintypes.DWORD(0)
            if k32.DeviceIoControl(h, code, None, 0, out, size, ctypes.byref(got), None):
                return out.raw[: got.value]
            if grow and last_error() in (ERROR_MORE_DATA, ERROR_INSUFFICIENT_BUFFER) and size < 1 << 16:
                size *= 4
                continue
            return None
    finally:
        k32.CloseHandle(h)

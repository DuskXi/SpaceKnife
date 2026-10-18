"""Memory of this process: private (committed) bytes and resident bytes, on every platform.

``private`` is what the process holds on its own — the Windows commit charge (Task Manager's
「内存」 column counts the resident part of it), anonymous resident plus swapped pages on Linux,
the physical footprint on macOS. ``None`` where a number cannot be read.
"""

from __future__ import annotations

import ctypes
import sys
from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class ProcessMemory:
    private: int | None
    resident: int | None


def process_memory() -> ProcessMemory:
    try:
        if sys.platform == "win32":
            return _windows()
        if sys.platform == "darwin":
            return _macos()
        return _linux()
    except (OSError, ValueError, AttributeError):
        return ProcessMemory(None, None)


def _windows() -> ProcessMemory:
    if sys.platform != "win32":  # also tells type checkers on other platforms to skip the rest
        raise OSError("not Windows")
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
            ("PrivateUsage", ctypes.c_size_t),
        ]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD]
    c = Counters()
    c.cb = ctypes.sizeof(Counters)
    if not psapi.GetProcessMemoryInfo(kernel32.GetCurrentProcess(), ctypes.byref(c), c.cb):
        raise ctypes.WinError(ctypes.get_last_error())
    return ProcessMemory(int(c.PrivateUsage), int(c.WorkingSetSize))


def _linux() -> ProcessMemory:
    fields: dict[str, int] = {}
    with open("/proc/self/status", encoding="ascii", errors="replace") as fh:
        for line in fh:
            key, _, rest = line.partition(":")
            if key in ("VmRSS", "RssAnon", "VmSwap"):
                fields[key] = int(rest.split()[0]) * 1024  # kB
    anon = fields.get("RssAnon")
    private = anon + fields.get("VmSwap", 0) if anon is not None else None
    return ProcessMemory(private, fields.get("VmRSS"))


def _macos() -> ProcessMemory:
    # task_info(mach_task_self(), TASK_VM_INFO): phys_footprint is what Activity Monitor shows
    class VmInfo(ctypes.Structure):
        _fields_ = [
            ("virtual_size", ctypes.c_uint64),
            ("region_count", ctypes.c_int32),
            ("page_size", ctypes.c_int32),
            ("resident_size", ctypes.c_uint64),
            ("resident_size_peak", ctypes.c_uint64),
            ("device", ctypes.c_uint64),
            ("device_peak", ctypes.c_uint64),
            ("internal", ctypes.c_uint64),
            ("internal_peak", ctypes.c_uint64),
            ("external", ctypes.c_uint64),
            ("external_peak", ctypes.c_uint64),
            ("reusable", ctypes.c_uint64),
            ("reusable_peak", ctypes.c_uint64),
            ("purgeable_volatile_pmap", ctypes.c_uint64),
            ("purgeable_volatile_resident", ctypes.c_uint64),
            ("purgeable_volatile_virtual", ctypes.c_uint64),
            ("compressed", ctypes.c_uint64),
            ("compressed_peak", ctypes.c_uint64),
            ("compressed_lifetime", ctypes.c_uint64),
            ("phys_footprint", ctypes.c_uint64),
        ]

    libc = ctypes.CDLL("/usr/lib/libSystem.dylib")
    task = ctypes.c_uint32.in_dll(libc, "mach_task_self_")
    info = VmInfo()
    count = ctypes.c_uint32(ctypes.sizeof(VmInfo) // 4)
    task_vm_info = 22
    if libc.task_info(task, task_vm_info, ctypes.byref(info), ctypes.byref(count)) != 0:
        raise OSError("task_info failed")
    return ProcessMemory(int(info.phys_footprint), int(info.resident_size))


def physical_memory() -> int | None:
    """Installed physical memory in bytes, or None."""
    try:
        if sys.platform == "win32":
            from ctypes import wintypes

            class Status(ctypes.Structure):
                _fields_ = [
                    ("dwLength", wintypes.DWORD),
                    ("dwMemoryLoad", wintypes.DWORD),
                    ("ullTotalPhys", ctypes.c_uint64),
                    ("ullAvailPhys", ctypes.c_uint64),
                    ("ullTotalPageFile", ctypes.c_uint64),
                    ("ullAvailPageFile", ctypes.c_uint64),
                    ("ullTotalVirtual", ctypes.c_uint64),
                    ("ullAvailVirtual", ctypes.c_uint64),
                    ("ullAvailExtendedVirtual", ctypes.c_uint64),
                ]

            st = Status()
            st.dwLength = ctypes.sizeof(Status)
            if not ctypes.WinDLL("kernel32").GlobalMemoryStatusEx(ctypes.byref(st)):
                return None
            return int(st.ullTotalPhys)
        import os

        return int(os.sysconf("SC_PHYS_PAGES")) * int(os.sysconf("SC_PAGE_SIZE"))
    except (OSError, ValueError, AttributeError):
        return None


__all__ = ["ProcessMemory", "physical_memory", "process_memory"]

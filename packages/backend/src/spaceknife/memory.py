"""Memory of this process: private (committed) bytes and resident bytes, on every platform.

``private`` is what the process holds on its own — the Windows commit charge (Task Manager's
「内存」 column counts the resident part of it), anonymous resident plus swapped pages on Linux,
the physical footprint on macOS. ``None`` where a number cannot be read.
"""

from __future__ import annotations

import ctypes
import os
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


def release() -> None:
    """Hand memory the allocator keeps but no longer uses back to the system (after a big scan was
    closed). The C heap keeps freed space for reuse: after a scan of millions of items that is a few
    hundred MB the next scan reuses, without this the process keeps it until then (docs/memory.md
    §8). Best effort: Windows decommits free heap pages (HeapOptimizeResources, Windows 8.1+),
    glibc trims its arenas, macOS relieves its malloc zones; elsewhere nothing happens."""
    try:
        if sys.platform == "win32":
            from ctypes import wintypes

            class Optimize(ctypes.Structure):  # HEAP_OPTIMIZE_RESOURCES_INFORMATION
                _fields_ = [("Version", wintypes.DWORD), ("Flags", wintypes.DWORD)]

            info = Optimize(1, 0)
            heap_optimize_resources = 3
            ctypes.WinDLL("kernel32").HeapSetInformation(
                None, heap_optimize_resources, ctypes.byref(info), ctypes.sizeof(info)
            )
        elif sys.platform == "darwin":
            libc = ctypes.CDLL("/usr/lib/libSystem.dylib")
            libc.malloc_zone_pressure_relief.argtypes = (ctypes.c_void_p, ctypes.c_size_t)
            libc.malloc_zone_pressure_relief(None, 0)
        else:
            trim = getattr(ctypes.CDLL(None), "malloc_trim", None)  # glibc only (not musl)
            if trim is not None:
                trim(0)
    except (OSError, AttributeError):
        pass


# glibc's mallopt parameters (malloc.h)
M_MMAP_THRESHOLD = -3
M_ARENA_MAX = -8
MMAP_THRESHOLD = 4 << 20  # blocks from 4 MiB on are mapped on their own, and unmapped when freed
ARENA_MAX = 4


def _glibc() -> ctypes.CDLL | None:
    """The C library when it is glibc (it has ``gnu_get_libc_version``; musl and others do not
    have mallopt's parameters or malloc_trim), else None."""
    if not sys.platform.startswith("linux"):
        return None
    try:
        libc = ctypes.CDLL(None)
    except OSError:
        return None
    if not all(hasattr(libc, name) for name in ("gnu_get_libc_version", "mallopt", "malloc_trim")):
        return None
    return libc


def tune_allocator() -> bool:
    """Linux with glibc, at the start of the backend before its threads: a fixed mmap threshold of
    4 MiB and at most 4 malloc arenas (docs/memory.md §8). By default glibc raises the threshold to
    the size of every mapped block that is freed (up to 32 MiB), so the scan's temporary arrays and
    slices of 128 KiB–32 MiB end up in the arenas of 30-odd threads, between long-lived blocks,
    where freed space stays with the process; a fixed threshold turns that off (man mallopt).
    ``SPACEKNIFE_GLIBC_DEFAULTS=1`` keeps glibc's own settings. Elsewhere nothing happens. Returns
    whether both were set."""
    if os.environ.get("SPACEKNIFE_GLIBC_DEFAULTS") == "1":
        return False
    libc = _glibc()
    if libc is None:
        return False
    try:
        mallopt = libc.mallopt
        mallopt.argtypes = (ctypes.c_int, ctypes.c_int)
        mallopt.restype = ctypes.c_int
        arenas = mallopt(M_ARENA_MAX, ARENA_MAX)
        threshold = mallopt(M_MMAP_THRESHOLD, MMAP_THRESHOLD)
    except (OSError, AttributeError):
        return False
    return bool(arenas and threshold)


def trim() -> None:
    """glibc: hand the free pages of the malloc arenas back to the system (``malloc_trim(0)``), after
    a scan and while the backend is idle (docs/memory.md §8). Elsewhere nothing happens."""
    libc = _glibc()
    if libc is None:
        return
    try:
        libc.malloc_trim.argtypes = (ctypes.c_size_t,)
        libc.malloc_trim(0)
    except (OSError, AttributeError):
        pass


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

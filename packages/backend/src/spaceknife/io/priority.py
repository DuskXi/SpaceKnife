"""Operating-system I/O priority (UI-W7-14: the idle level and background rescans read at low priority).

* Windows: ``NtSetInformationProcess`` / ``NtSetInformationThread`` with the I/O priority class
  (very low ↔ normal); lowering needs no privilege, also for our own child processes. The heavier
  background processing mode (``PROCESS_MODE_BACKGROUND_BEGIN`` via ``SetPriorityClass``, calling
  process only) is available with ``background_mode=True``, but it also drops the CPU priority to
  idle: on a busy machine (measured with tools/bench_io.py) the workers then starve for seconds,
  so the I/O-only class is the default.
* Linux: ``ioprio_set(IOPRIO_WHO_PROCESS, tid, IOPRIO_CLASS_IDLE)`` for every thread of the process
  (the I/O priority is per thread; threads started later inherit it). Only I/O schedulers that
  honour priorities (BFQ, recent mq-deadline) act on it; ``none`` on NVMe ignores it.
* macOS: ``setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, IOPOL_THROTTLE)``; it only applies
  to the calling process (or thread), so pool workers set it themselves.

For a process pool the simplest correct use is per batch inside the worker:
``set_thread_low_io_priority(governor.low_io_priority)`` (cheap, and it follows level changes).
All helpers return whether the change took effect and never raise.
"""

from __future__ import annotations

import ctypes
import os
import platform
import sys
from typing import Any

from loguru import logger

# Windows
_PROCESS_MODE_BACKGROUND_BEGIN = 0x00100000
_PROCESS_MODE_BACKGROUND_END = 0x00200000
_ERROR_PROCESS_MODE_ALREADY_BACKGROUND = 402
_ERROR_PROCESS_MODE_NOT_BACKGROUND = 403
_PROCESS_SET_INFORMATION = 0x0200
_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_PROCESS_IO_PRIORITY = 33  # PROCESSINFOCLASS.ProcessIoPriority
_THREAD_IO_PRIORITY = 22  # THREADINFOCLASS.ThreadIoPriority
IO_PRIORITY_VERY_LOW, IO_PRIORITY_LOW, IO_PRIORITY_NORMAL = 0, 1, 2

# Linux
_IOPRIO_WHO_PROCESS = 1
_IOPRIO_CLASS_SHIFT = 13
IOPRIO_CLASS_NONE, IOPRIO_CLASS_RT, IOPRIO_CLASS_BE, IOPRIO_CLASS_IDLE = 0, 1, 2, 3
_IOPRIO_SYSCALLS = {  # machine → (ioprio_set, ioprio_get)
    "x86_64": (251, 252),
    "amd64": (251, 252),
    "i386": (289, 290),
    "i686": (289, 290),
    "aarch64": (30, 31),
    "arm64": (30, 31),
    "riscv64": (30, 31),
    "loongarch64": (30, 31),
    "armv7l": (314, 315),
    "armv8l": (314, 315),
    "ppc64le": (273, 274),
    "ppc64": (273, 274),
    "s390x": (282, 283),
}

# macOS
_IOPOL_TYPE_DISK = 0
_IOPOL_SCOPE_PROCESS, _IOPOL_SCOPE_THREAD = 0, 1
_IOPOL_DEFAULT, _IOPOL_THROTTLE = 0, 3


# --------------------------------------------------------------------------- Windows


def _ntdll() -> Any:
    from ctypes import wintypes

    nt = ctypes.WinDLL("ntdll")  # type: ignore[attr-defined,unused-ignore]
    for name in ("NtSetInformationProcess", "NtSetInformationThread"):
        fn = getattr(nt, name)
        fn.restype = ctypes.c_long
        fn.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.ULONG]
    for name in ("NtQueryInformationProcess", "NtQueryInformationThread"):
        fn = getattr(nt, name)
        fn.restype = ctypes.c_long
        fn.argtypes = [
            wintypes.HANDLE,
            ctypes.c_int,
            ctypes.c_void_p,
            wintypes.ULONG,
            ctypes.POINTER(wintypes.ULONG),
        ]
    return nt


def _win_set_io(handle: Any, thread: bool, on: bool) -> bool:
    value = ctypes.c_ulong(IO_PRIORITY_VERY_LOW if on else IO_PRIORITY_NORMAL)
    nt = _ntdll()
    if thread:
        return bool(nt.NtSetInformationThread(handle, _THREAD_IO_PRIORITY, ctypes.byref(value), 4) == 0)
    return bool(nt.NtSetInformationProcess(handle, _PROCESS_IO_PRIORITY, ctypes.byref(value), 4) == 0)


def _win_get_io(handle: Any, thread: bool) -> int | None:
    from ctypes import wintypes

    value = ctypes.c_ulong(0)
    got = wintypes.ULONG(0)
    nt = _ntdll()
    query = nt.NtQueryInformationThread if thread else nt.NtQueryInformationProcess
    status = query(
        handle, _THREAD_IO_PRIORITY if thread else _PROCESS_IO_PRIORITY, ctypes.byref(value), 4, got
    )
    return int(value.value) if status == 0 else None


def _win_process(pid: int | None, on: bool, background_mode: bool) -> bool:
    from . import _win

    k32 = _win.kernel32()
    own = pid is None or pid == os.getpid()
    if own and background_mode:
        mode = _PROCESS_MODE_BACKGROUND_BEGIN if on else _PROCESS_MODE_BACKGROUND_END
        if k32.SetPriorityClass(k32.GetCurrentProcess(), mode):
            return True
        return _win.last_error() in (
            _ERROR_PROCESS_MODE_ALREADY_BACKGROUND,
            _ERROR_PROCESS_MODE_NOT_BACKGROUND,
        )
    if own:
        return _win_set_io(k32.GetCurrentProcess(), False, on)
    h = k32.OpenProcess(_PROCESS_SET_INFORMATION, False, pid)
    if not h:
        return False
    try:
        return _win_set_io(h, False, on)
    finally:
        k32.CloseHandle(h)


def _win_io_priority(pid: int | None, thread: bool) -> int | None:
    from . import _win

    k32 = _win.kernel32()
    if thread:
        return _win_get_io(k32.GetCurrentThread(), True)
    if pid is None or pid == os.getpid():
        return _win_get_io(k32.GetCurrentProcess(), False)
    h = k32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return None
    try:
        return _win_get_io(h, False)
    finally:
        k32.CloseHandle(h)


# --------------------------------------------------------------------------- Linux


def _linux_syscalls() -> tuple[int, int] | None:
    return _IOPRIO_SYSCALLS.get(platform.machine().lower())


def _linux_tids(pid: int | None) -> list[int]:
    base = f"/proc/{'self' if pid is None else pid}/task"
    try:
        return sorted(int(t) for t in os.listdir(base))
    except (OSError, ValueError):
        return [0 if pid is None else pid]


def _linux_set(tids: list[int], on: bool) -> bool:
    nrs = _linux_syscalls()
    if nrs is None:
        return False
    libc = ctypes.CDLL(None, use_errno=True)
    prio = (IOPRIO_CLASS_IDLE if on else IOPRIO_CLASS_NONE) << _IOPRIO_CLASS_SHIFT
    ok = False
    for tid in tids:
        if libc.syscall(nrs[0], _IOPRIO_WHO_PROCESS, tid, prio) == 0:
            ok = True
        else:
            logger.debug("ioprio_set({}) failed: errno {}", tid, ctypes.get_errno())
    return ok


def _linux_get(tid: int) -> int | None:
    nrs = _linux_syscalls()
    if nrs is None:
        return None
    value = int(ctypes.CDLL(None, use_errno=True).syscall(nrs[1], _IOPRIO_WHO_PROCESS, tid))
    return None if value < 0 else value >> _IOPRIO_CLASS_SHIFT


# --------------------------------------------------------------------------- macOS


def _darwin_set(scope: int, on: bool) -> bool:
    fn = getattr(ctypes.CDLL(None, use_errno=True), "setiopolicy_np", None)
    if fn is None:
        return False
    return bool(fn(_IOPOL_TYPE_DISK, scope, _IOPOL_THROTTLE if on else _IOPOL_DEFAULT) == 0)


def _darwin_get(scope: int) -> int | None:
    fn = getattr(ctypes.CDLL(None, use_errno=True), "getiopolicy_np", None)
    if fn is None:
        return None
    value = int(fn(_IOPOL_TYPE_DISK, scope))
    return None if value < 0 else value


# --------------------------------------------------------------------------- public


def set_low_io_priority(pid: int | None = None, on: bool = True, *, background_mode: bool = False) -> bool:
    """Lower (``on``) or restore the I/O priority of a process (``None``: the calling process).

    ``background_mode`` (Windows, calling process only): use ``PROCESS_MODE_BACKGROUND_BEGIN``,
    which also lowers CPU and memory priority. On macOS only the calling process can be changed.
    """
    try:
        if sys.platform == "win32":
            return _win_process(pid, on, background_mode)
        if sys.platform == "darwin":
            return pid in (None, os.getpid()) and _darwin_set(_IOPOL_SCOPE_PROCESS, on)
        return _linux_set(_linux_tids(None if pid == os.getpid() else pid), on)
    except Exception as exc:
        logger.debug("set_low_io_priority failed: {}", exc)
        return False


def set_thread_low_io_priority(on: bool = True) -> bool:
    """Lower or restore the I/O priority of the calling thread only (cheap: per batch in a worker)."""
    try:
        if sys.platform == "win32":
            from . import _win

            return _win_set_io(_win.kernel32().GetCurrentThread(), True, on)
        if sys.platform == "darwin":
            return _darwin_set(_IOPOL_SCOPE_THREAD, on)
        return _linux_set([0], on)
    except Exception as exc:
        logger.debug("set_thread_low_io_priority failed: {}", exc)
        return False


def io_priority_is_low(pid: int | None = None, *, thread: bool = False) -> bool | None:
    """Whether a process (or the calling thread) reads at low I/O priority (None: cannot tell)."""
    try:
        if sys.platform == "win32":
            value = _win_io_priority(pid, thread)
            return None if value is None else value < IO_PRIORITY_NORMAL
        if sys.platform == "darwin":
            if not thread and pid not in (None, os.getpid()):
                return None
            policy = _darwin_get(_IOPOL_SCOPE_THREAD if thread else _IOPOL_SCOPE_PROCESS)
            return None if policy is None else policy == _IOPOL_THROTTLE
        tid = 0 if thread else _linux_tids(None if pid == os.getpid() else pid)[0]
        cls = _linux_get(tid)
        return None if cls is None else cls == IOPRIO_CLASS_IDLE
    except Exception:
        return None


__all__ = ["io_priority_is_low", "set_low_io_priority", "set_thread_low_io_priority"]

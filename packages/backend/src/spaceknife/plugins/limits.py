"""Resource limits of plugin processes (docs/plugins.md, 「资源限制」).

* Windows: a job object per plugin — memory per process, kill on close (the backend's handle going
  away ends the plugin), at most a few processes, no clipboard / desktop / system settings; the
  process runs below normal priority.
* Linux: the process limits itself before it imports any plugin code — ``RLIMIT_DATA`` (private
  mappings count since 4.7; ``RLIMIT_AS`` would trip numpy's address reservations), no core dumps,
  ``PR_SET_PDEATHSIG`` (it dies with the backend), ``PR_SET_NO_NEW_PRIVS``, nice 10.
* macOS: no core dumps and nice 10; the host watches the resident size (heartbeats) and the child
  polls its parent.
"""

from __future__ import annotations

import contextlib
import os
import sys
from typing import Any

from loguru import logger

BELOW_NORMAL_PRIORITY_CLASS = 0x00004000
CREATE_NO_WINDOW = 0x08000000


def creation_flags() -> int:
    return BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW if sys.platform == "win32" else 0


if sys.platform == "win32":

    def limit_self(memory_mb: int, *, server_pid: int | None = None) -> list[str]:
        """The host's job object does it all on Windows."""
        return []

else:

    def limit_self(memory_mb: int, *, server_pid: int | None = None) -> list[str]:
        """In the plugin process, before the plugin's code: what could not be set (for the log)."""
        import resource

        missing: list[str] = []
        with contextlib.suppress(OSError, ValueError):
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        with contextlib.suppress(OSError):
            os.nice(10)
        if sys.platform.startswith("linux"):
            limit = memory_mb << 20
            try:
                _soft, hard = resource.getrlimit(resource.RLIMIT_DATA)
                new_hard = limit if hard == resource.RLIM_INFINITY or hard > limit else hard
                resource.setrlimit(resource.RLIMIT_DATA, (min(limit, new_hard), new_hard))
            except (OSError, ValueError) as exc:
                missing.append(f"RLIMIT_DATA: {exc}")
            try:
                import ctypes
                import signal

                libc = ctypes.CDLL(None, use_errno=True)
                pr_set_pdeathsig, pr_set_no_new_privs = 1, 38
                libc.prctl(pr_set_pdeathsig, int(signal.SIGKILL), 0, 0, 0)
                libc.prctl(pr_set_no_new_privs, 1, 0, 0, 0)
            except (OSError, AttributeError) as exc:
                missing.append(f"prctl: {exc}")
            if server_pid is not None and os.getppid() != server_pid:
                os._exit(4)  # the backend died before the death signal was armed
        return missing


# --------------------------------------------------------------------------- Windows job objects

if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes

    _k32 = ctypes.WinDLL("kernel32", use_last_error=True)

    JobObjectBasicUIRestrictions = 4
    JobObjectExtendedLimitInformation = 9
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 0x00000008
    JOB_OBJECT_LIMIT_PROCESS_MEMORY = 0x00000100
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION = 0x00000400
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000
    JOB_OBJECT_UILIMIT_HANDLES = 0x00000001
    JOB_OBJECT_UILIMIT_READCLIPBOARD = 0x00000002
    JOB_OBJECT_UILIMIT_WRITECLIPBOARD = 0x00000004
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS = 0x00000008
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS = 0x00000010
    JOB_OBJECT_UILIMIT_GLOBALATOMS = 0x00000020
    JOB_OBJECT_UILIMIT_DESKTOP = 0x00000040
    JOB_OBJECT_UILIMIT_EXITWINDOWS = 0x00000080
    PROCESS_SET_QUOTA = 0x0100
    PROCESS_TERMINATE = 0x0001
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000

    class IO_COUNTERS(ctypes.Structure):
        _fields_ = [
            (n, ctypes.c_ulonglong)
            for n in (
                "ReadOperationCount",
                "WriteOperationCount",
                "OtherOperationCount",
                "ReadTransferCount",
                "WriteTransferCount",
                "OtherTransferCount",
            )
        ]

    class JOBOBJECT_BASIC_LIMIT_INFORMATION(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", wintypes.LARGE_INTEGER),
            ("PerJobUserTimeLimit", wintypes.LARGE_INTEGER),
            ("LimitFlags", wintypes.DWORD),
            ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t),
            ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t),
            ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class JOBOBJECT_EXTENDED_LIMIT_INFORMATION(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", JOBOBJECT_BASIC_LIMIT_INFORMATION),
            ("IoInfo", IO_COUNTERS),
            ("ProcessMemoryLimit", ctypes.c_size_t),
            ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t),
            ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    class JOBOBJECT_BASIC_UI_RESTRICTIONS(ctypes.Structure):
        _fields_ = [("UIRestrictionsClass", wintypes.DWORD)]

    _k32.CreateJobObjectW.restype = wintypes.HANDLE
    _k32.CreateJobObjectW.argtypes = [wintypes.LPVOID, wintypes.LPCWSTR]
    _k32.SetInformationJobObject.restype = wintypes.BOOL
    _k32.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD]
    _k32.AssignProcessToJobObject.restype = wintypes.BOOL
    _k32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    _k32.OpenProcess.restype = wintypes.HANDLE
    _k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    _k32.CloseHandle.restype = wintypes.BOOL
    _k32.CloseHandle.argtypes = [wintypes.HANDLE]
    _k32.TerminateJobObject.restype = wintypes.BOOL
    _k32.TerminateJobObject.argtypes = [wintypes.HANDLE, wintypes.UINT]


if sys.platform == "win32":

    class Job:
        """A Windows job object for one plugin (no-op elsewhere)."""

        def __init__(self, memory_mb: int, processes: int) -> None:
            self.handle: Any = None
            h = _k32.CreateJobObjectW(None, None)
            if not h:
                logger.warning("CreateJobObjectW failed: {}", ctypes.get_last_error())
                return
            info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION()
            info.BasicLimitInformation.LimitFlags = (
                JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            )
            info.BasicLimitInformation.ActiveProcessLimit = processes
            info.ProcessMemoryLimit = memory_mb << 20
            if not _k32.SetInformationJobObject(
                h, JobObjectExtendedLimitInformation, ctypes.byref(info), ctypes.sizeof(info)
            ):
                logger.warning("SetInformationJobObject (limits) failed: {}", ctypes.get_last_error())
            ui = JOBOBJECT_BASIC_UI_RESTRICTIONS()
            ui.UIRestrictionsClass = (
                JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
            )
            if not _k32.SetInformationJobObject(
                h, JobObjectBasicUIRestrictions, ctypes.byref(ui), ctypes.sizeof(ui)
            ):
                logger.warning("SetInformationJobObject (UI) failed: {}", ctypes.get_last_error())
            self.handle = h

        def assign(self, pid: int) -> bool:
            if self.handle is None:
                return False
            proc = _k32.OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, False, pid)
            if not proc:
                return False
            try:
                ok = bool(_k32.AssignProcessToJobObject(self.handle, proc))
                if not ok:
                    logger.warning("AssignProcessToJobObject({}) failed: {}", pid, ctypes.get_last_error())
                return ok
            finally:
                _k32.CloseHandle(proc)

        def close(self) -> None:
            """Ends every process in the job (kill on close)."""
            if self.handle is not None:
                _k32.TerminateJobObject(self.handle, 1)
                _k32.CloseHandle(self.handle)
                self.handle = None

else:

    class Job:
        """No job objects here: the plugin process limits itself (:func:`limit_self`)."""

        def __init__(self, memory_mb: int, processes: int) -> None:
            self.handle: Any = None

        def assign(self, pid: int) -> bool:
            return False

        def close(self) -> None:
            return None


def rss_of(pid: int) -> int:
    """Resident bytes of a process (0 when unknown)."""
    try:
        if sys.platform.startswith("linux"):
            with open(f"/proc/{pid}/statm") as fh:
                return int(fh.read().split()[1]) * os.sysconf("SC_PAGE_SIZE")
    except (OSError, ValueError):
        return 0
    return 0


__all__ = ["Job", "creation_flags", "limit_self", "rss_of"]

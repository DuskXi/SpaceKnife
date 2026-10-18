"""Starting the helper process: a plain subprocess, or elevated through the UAC prompt."""

from __future__ import annotations

import asyncio
import subprocess
import sys
import threading
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Protocol

from ..i18n import _, tr
from .errors import HelperDenied, HelperUnavailable

if sys.platform == "win32":
    from . import _win32

ERROR_CANCELLED = 1223
SEE_MASK_NOCLOSEPROCESS = 0x00000040
SEE_MASK_NOASYNC = 0x00000100
SEE_MASK_FLAG_NO_UI = 0x00000400
SEE_MASK_NO_CONSOLE = 0x00008000
SW_HIDE = 0


class HelperProcess(Protocol):
    pid: int

    def poll(self) -> int | None: ...
    def wait(self, timeout: float) -> int | None: ...
    def terminate(self) -> None: ...
    def close(self) -> None: ...


class PopenProcess:
    def __init__(self, popen: subprocess.Popen[bytes]) -> None:
        self._popen = popen
        self.pid = popen.pid

    def poll(self) -> int | None:
        return self._popen.poll()

    def wait(self, timeout: float) -> int | None:
        try:
            return self._popen.wait(timeout)
        except subprocess.TimeoutExpired:
            return None

    def terminate(self) -> None:
        if self._popen.poll() is None:
            self._popen.kill()

    def close(self) -> None:
        self._popen.poll()  # reap if it has exited


def helper_command(elevated: bool) -> list[str]:
    """Interpreter + module. The elevated helper runs isolated (-I): no PYTHON* variables, no
    user site-packages, no current directory on sys.path, so nothing user-writable outside the
    installed environment can inject code into the administrator process. A frozen backend (the
    installed app, M8) has no ``-m``: it runs its own executable in helper mode, and the frozen
    interpreter only imports from its bundle."""
    if getattr(sys, "frozen", False):
        return [sys.executable, "--helper"]
    if elevated:
        return [sys.executable, "-I", "-m", "spaceknife.helper"]
    return [sys.executable, "-m", "spaceknife.helper"]


def start_subprocess(argv: Sequence[str]) -> HelperProcess:
    """Unelevated helper: stdout stays clean (the backend's ready line), stderr is shared."""
    flags = 0
    if sys.platform == "win32":
        flags = subprocess.CREATE_NO_WINDOW  # the backend may have no console to share
    try:
        popen = subprocess.Popen(
            list(argv),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            close_fds=True,
            creationflags=flags,
        )
    except OSError as exc:
        raise HelperUnavailable(tr("无法启动扫描助手：{reason}", reason=exc), "exited") from exc
    return PopenProcess(popen)


@dataclass(frozen=True, slots=True)
class ShellExecuteParams:
    verb: str
    file: str
    parameters: str
    directory: str | None
    show: int
    mask: int


def elevated_params(argv: Sequence[str]) -> ShellExecuteParams:
    """ShellExecuteExW arguments for ``argv`` behind the UAC prompt, hidden and without UI."""
    return ShellExecuteParams(
        verb="runas",
        file=argv[0],
        parameters=subprocess.list2cmdline(list(argv[1:])),
        directory=None,
        show=SW_HIDE,
        mask=SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
    )


async def start_elevated(argv: Sequence[str]) -> HelperProcess:
    """Show the UAC prompt and start ``argv`` elevated (Windows only).

    ShellExecuteExW blocks until the user answers, so it runs on its own COM-initialised daemon
    thread. If the caller gives up first (connect timeout), a late process handle is released;
    the helper then finds the pipe gone and exits by itself.
    """
    if sys.platform != "win32":
        raise HelperUnavailable(_("当前平台暂不支持提权助手"), "not_supported")
    params = elevated_params(argv)
    loop = asyncio.get_running_loop()
    future: asyncio.Future[HelperProcess] = loop.create_future()

    def deliver(proc: HelperProcess | None, exc: BaseException | None) -> None:
        if future.done():
            if proc is not None:
                proc.close()
        elif exc is not None:
            future.set_exception(exc)
        else:
            assert proc is not None
            future.set_result(proc)

    def worker() -> None:
        proc: HelperProcess | None = None
        exc: BaseException | None = None
        try:
            proc = _shell_execute(params)
        except PermissionError as err:
            exc = (
                HelperDenied()
                if err.errno == ERROR_CANCELLED
                else HelperUnavailable(tr("无法启动扫描助手：{reason}", reason=err))
            )
        except OSError as err:
            exc = HelperUnavailable(tr("无法启动扫描助手：{reason}", reason=err), "exited")
        try:
            loop.call_soon_threadsafe(deliver, proc, exc)
        except RuntimeError:  # loop closed meanwhile
            if proc is not None:
                proc.close()

    threading.Thread(target=worker, name="sk-helper-uac", daemon=True).start()
    return await future


if sys.platform == "win32":

    def _shell_execute(params: ShellExecuteParams) -> HelperProcess:
        return _win32.shell_execute(params)

else:

    def _shell_execute(params: ShellExecuteParams) -> HelperProcess:
        raise OSError(_("ShellExecuteExW 只在 Windows 上可用"))


__all__ = [
    "HelperProcess",
    "PopenProcess",
    "ShellExecuteParams",
    "elevated_params",
    "helper_command",
    "start_elevated",
    "start_subprocess",
]

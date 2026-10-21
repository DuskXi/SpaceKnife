"""Starting the helper process: a plain subprocess, or elevated through the UAC prompt (Windows)
or polkit's ``pkexec`` (Linux)."""

from __future__ import annotations

import asyncio
import contextlib
import os
import subprocess
import sys
import threading
from collections import deque
from collections.abc import Sequence
from dataclasses import dataclass
from typing import IO, Protocol

from loguru import logger

from ..i18n import _, tr
from .errors import HelperDenied, HelperError, HelperUnavailable

if sys.platform == "win32":
    from . import _win32

ERROR_CANCELLED = 1223
SEE_MASK_NOCLOSEPROCESS = 0x00000040
SEE_MASK_NOASYNC = 0x00000100
SEE_MASK_FLAG_NO_UI = 0x00000400
SEE_MASK_NO_CONSOLE = 0x00008000
SW_HIDE = 0
PKEXEC_PATHS = ("/usr/bin/pkexec", "/bin/pkexec")
PKEXEC_DISMISSED = 126  # the user closed the password dialog
PKEXEC_REFUSED = 127  # not authorized, no authentication agent, or the program can't run


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


class PkexecProcess(PopenProcess):
    """``pkexec helper …``: pkexec asks polkit, then replaces itself with the helper (the same
    pid, now root). Its stderr is read on a thread: the last lines say why pkexec gave up, and the
    helper's log goes to the backend's log."""

    def __init__(self, popen: subprocess.Popen[bytes]) -> None:
        super().__init__(popen)
        self.tail: deque[str] = deque(maxlen=20)
        self._reader: threading.Thread | None = None
        if popen.stderr is not None:
            self._reader = threading.Thread(
                target=self._read, args=(popen.stderr,), name="sk-helper-stderr", daemon=True
            )
            self._reader.start()

    def _read(self, stream: IO[bytes]) -> None:
        with contextlib.suppress(OSError, ValueError):
            for raw in stream:
                line = raw.decode("utf-8", "replace").rstrip()
                if line:
                    self.tail.append(line)
                    logger.debug("helper: {}", line)

    def terminate(self) -> None:
        with contextlib.suppress(PermissionError, ProcessLookupError):  # root by now: not ours to kill
            super().terminate()

    def exit_error(self, code: int) -> HelperError | None:
        """What pkexec's exit code means, or None (the helper itself exited)."""
        if self._reader is not None:
            self._reader.join(1.0)  # it has exited: its last words are on their way
        return pkexec_error(code, list(self.tail))


def pkexec_error(code: int, stderr: Sequence[str]) -> HelperError | None:
    if code == PKEXEC_DISMISSED:
        return HelperDenied()
    if code != PKEXEC_REFUSED:
        return None
    text = " ".join(stderr).strip()
    if "authentication agent" in text.lower():
        return HelperUnavailable(
            _("没有可以询问管理员密码的身份验证代理（比如在 SSH 会话里，或没有图形桌面）"), "no_agent"
        )
    return HelperUnavailable(tr("系统没有给予管理员授权：{reason}", reason=text or code), "not_authorized")


def pkexec_path() -> str | None:
    """pkexec at a fixed absolute path (never looked up in PATH)."""
    for path in PKEXEC_PATHS:
        if os.access(path, os.X_OK):
            return path
    return None


def in_appimage(executable: str | None = None) -> bool:
    """An AppImage runs from a FUSE mount only its user can read: root can't start the helper."""
    return bool(os.environ.get("APPIMAGE")) or "/.mount_" in (executable or sys.executable)


def start_pkexec(argv: Sequence[str]) -> HelperProcess:
    """``pkexec argv…`` (Linux): the desktop's polkit agent shows the password dialog while the
    helper is being started; the caller waits for it to connect. Its own session, so pkexec never
    falls back to asking on the backend's terminal."""
    pkexec = pkexec_path()
    if pkexec is None:
        raise HelperUnavailable(_("找不到 pkexec：请求管理员授权需要 polkit"), "not_supported")
    if in_appimage(argv[0]):
        raise HelperUnavailable(
            _("AppImage 版本不能以管理员身份运行助手；请改用 deb 或 rpm 安装包"), "not_supported"
        )
    if not os.path.isabs(argv[0]):
        raise HelperUnavailable(tr("助手的路径不是绝对路径：{path}", path=argv[0]), "not_supported")
    try:
        popen = subprocess.Popen(
            [pkexec, *argv],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            close_fds=True,
            start_new_session=True,
        )
    except OSError as exc:
        raise HelperUnavailable(tr("无法启动 pkexec：{reason}", reason=exc), "exited") from exc
    return PkexecProcess(popen)


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
    """Start ``argv`` elevated: the UAC prompt (Windows) or pkexec (Linux).

    ShellExecuteExW blocks until the user answers, so it runs on its own COM-initialised daemon
    thread. If the caller gives up first (connect timeout), a late process handle is released;
    the helper then finds the pipe gone and exits by itself. pkexec returns at once; its password
    dialog stays open while the caller waits for the helper to connect.
    """
    if sys.platform.startswith("linux"):
        return start_pkexec(argv)
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
    "PkexecProcess",
    "PopenProcess",
    "ShellExecuteParams",
    "elevated_params",
    "helper_command",
    "in_appimage",
    "pkexec_error",
    "pkexec_path",
    "start_elevated",
    "start_pkexec",
    "start_subprocess",
]

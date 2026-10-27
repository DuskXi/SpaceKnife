"""Process-level runner for ``spaceknife serve``.

Contract with the launching process (Electron main, native UI, tests):

1. With ``--token-stdin`` the first stdin line is the session token (never argv/env, which
   other local users can read). Without it a random token is generated and included in the
   ready line.
2. Once listening, exactly one JSON line is written to **stdout**::

       {"event": "ready", "url": "ws://127.0.0.1:PORT/v1", "port": PORT, "pid": ..., ...}

   All logging goes to stderr.
3. The backend exits gracefully when stdin reaches EOF (the parent closed the pipe or died),
   on ``app.shutdown``, on SIGTERM/SIGINT, or — on Linux — when the parent dies (PDEATHSIG).
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import signal
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import IO

from loguru import logger

from spaceknife_protocol import PROTOCOL_VERSION, SUBPROTOCOL

from ..logbus import LogHub
from .app import Server, ServerConfig


def write_ready_file(path: Path, ready: dict[str, object]) -> None:
    """Write connection info (including the token) readable by the current user only.

    Lets another local process — the CLI, a demo client — attach to a running backend, the same
    way Jupyter connection files work. Deleted again on shutdown.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_BINARY", 0)
    fd = os.open(tmp, flags, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(ready, fh)
    os.replace(tmp, path)


def remove_ready_file(path: Path) -> None:
    path.unlink(missing_ok=True)


def read_token_line(stream: IO[bytes]) -> str:
    """Read the token from the first line of a binary stream (stdin)."""
    line = stream.readline()
    token = line.decode("utf-8", "replace").strip()
    if len(token) < 16:
        raise SystemExit("expected a token of at least 16 characters on the first stdin line")
    return token


def _pipe_poller(stream: IO[bytes]) -> Callable[[], bool] | None:
    """Windows: a non-blocking "has the writer closed this pipe?" check, or None if not a pipe.

    A thread blocked in a synchronous ``ReadFile`` on stdin makes ``CreateProcess`` in other
    threads wait for that read (the handle's file object is locked), which deadlocks the scan
    process pool. ``PeekNamedPipe`` never leaves a read pending.
    """
    import ctypes
    import msvcrt
    from ctypes import wintypes

    try:
        fd = stream.fileno()
        handle = msvcrt.get_osfhandle(fd)  # type: ignore[attr-defined,unused-ignore]
    except (OSError, ValueError, AttributeError):
        return None
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    avail = wintypes.DWORD()

    def peek() -> bool | None:
        ok = kernel32.PeekNamedPipe(wintypes.HANDLE(handle), None, 0, None, ctypes.byref(avail), None)
        if not ok:
            err = ctypes.get_last_error()  # type: ignore[attr-defined,unused-ignore]
            return True if err in (109, 232) else None  # ERROR_BROKEN_PIPE, ERROR_NO_DATA
        if avail.value:
            os.read(fd, avail.value)  # discard: data is there, so this read cannot block
        return False

    if peek() is None:
        return None
    return lambda: bool(peek())


def _watch_stdin_eof(stream: IO[bytes], loop: asyncio.AbstractEventLoop, on_eof: Callable[[], None]) -> None:
    poll = _pipe_poller(stream) if sys.platform == "win32" else None

    def notify() -> None:
        logger.info("stdin closed by the parent process")
        with contextlib.suppress(RuntimeError):
            loop.call_soon_threadsafe(on_eof)

    def run_poll() -> None:
        assert poll is not None
        while not poll():
            time.sleep(0.25)
        notify()

    def run_read() -> None:
        try:
            while stream.readline():
                pass
        except (OSError, ValueError):
            pass
        notify()

    target = run_poll if poll is not None else run_read
    threading.Thread(target=target, name="stdin-eof-watch", daemon=True).start()


def _arm_parent_death_signal() -> None:
    """Linux: ask the kernel to SIGTERM us when the parent dies."""
    if not sys.platform.startswith("linux"):
        return
    try:
        import ctypes

        libc = ctypes.CDLL("libc.so.6", use_errno=True)
        pr_set_pdeathsig = 1
        libc.prctl(pr_set_pdeathsig, signal.SIGTERM)
    except OSError:
        logger.debug("prctl(PR_SET_PDEATHSIG) unavailable")


async def _watch_parent_posix(server: Server, interval: float = 1.0) -> None:
    """macOS/BSD fallback: notice re-parenting (parent died) by polling getppid()."""
    original = os.getppid()
    while not server.shutdown_requested.is_set():
        await asyncio.sleep(interval)
        if os.getppid() != original:
            logger.warning("parent process exited; shutting down")
            server.request_shutdown()
            return


async def run_server(
    config: ServerConfig,
    *,
    log_hub: LogHub,
    ready_out: IO[str],
    stdin: IO[bytes] | None,
    token_generated: bool,
    ready_file: Path | None = None,
) -> int:
    server = Server(config, log_hub=log_hub)
    await server.start()
    loop = asyncio.get_running_loop()

    ready = {
        "event": "ready",
        "url": server.url,
        "host": config.host,
        "port": server.port,
        "pid": os.getpid(),
        "protocol": PROTOCOL_VERSION,
        "subprotocol": SUBPROTOCOL,
        # where update files are downloaded: Electron main starts only files from there
        "updates_dir": str(server.updates.updates_dir),
    }
    if token_generated:
        ready["token"] = config.token
    if ready_file is not None:
        # Written before the ready line so a parent that sees "ready" can rely on the file.
        write_ready_file(ready_file, {**ready, "token": config.token})
    ready_out.write(json.dumps(ready) + "\n")
    ready_out.flush()

    watchers: list[asyncio.Task[None]] = []
    if stdin is not None:
        _watch_stdin_eof(stdin, loop, server.request_shutdown)
        _arm_parent_death_signal()
        if os.name == "posix" and not sys.platform.startswith("linux"):
            watchers.append(asyncio.create_task(_watch_parent_posix(server)))
    if os.name == "posix":
        for sig in (signal.SIGTERM, signal.SIGINT):
            with contextlib.suppress(NotImplementedError, RuntimeError):
                loop.add_signal_handler(sig, server.request_shutdown)

    try:
        await server.shutdown_requested.wait()
    finally:
        for task in watchers:
            task.cancel()
        await server.close()
        if ready_file is not None:
            remove_ready_file(ready_file)
    return 0

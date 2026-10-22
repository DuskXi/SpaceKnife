"""Local byte channels between the backend and the helper.

Windows: a named pipe ``\\\\.\\pipe\\spaceknife-helper-<hex>`` (single instance, local clients
only, DACL for this user); POSIX: a unix-domain socket in a private 0700 directory. The backend
(unprivileged) is always the server and owns the address.

* backend side: :class:`Listener` → :class:`AsyncChannel` (asyncio; pipe I/O runs on one
  dedicated thread per channel, sockets use the loop's ``sock_*`` methods);
* helper side: :func:`connect` → :class:`SyncChannel` (blocking, with timeouts).

Timeouts are *inactivity* timeouts: each underlying read or write must make progress in time,
so a large frame is never cut short just because it is large. Every channel knows the pid of
the process at the other end (from the OS, not from anything the peer says).
"""

from __future__ import annotations

import asyncio
import os
import re
import secrets
import select
import shutil
import socket
import struct
import sys
import tempfile
import time
from abc import ABC, abstractmethod
from collections.abc import Callable, Sequence
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from ..i18n import tr
from .messages import (
    HEADER,
    KIND_JSON,
    MAX_BINARY_BYTES,
    MAX_CONTROL_BYTES,
    FrameError,
    Message,
    check_header,
    json_frame,
)

HELPER_PREFIX = "spaceknife-helper-"
PLUGIN_PREFIX = "spaceknife-plugin-"  # plugin processes (spaceknife.plugins): the same channel
PREFIXES = (HELPER_PREFIX, PLUGIN_PREFIX)
PIPE_PREFIX = "\\\\.\\pipe\\" + HELPER_PREFIX
SOCKET_NAME = "helper.sock"
_SEND_CHUNK = 8 << 20
Buffer = bytes | bytearray | memoryview


def _pipe_re(prefix: str) -> re.Pattern[str]:
    return re.compile(r"\\\\\.\\pipe\\" + re.escape(prefix) + r"[0-9a-f]{32}")


def _socket_dir_re(prefix: str) -> re.Pattern[str]:
    return re.compile(re.escape(prefix) + r"[0-9a-z_]+")


def valid_address(address: str, prefix: str = HELPER_PREFIX) -> bool:
    """Whether ``address`` is one this module could have created with ``prefix`` (the helper and
    plugin processes refuse others)."""
    if prefix not in PREFIXES:
        return False
    if sys.platform == "win32":
        return _pipe_re(prefix).fullmatch(address) is not None
    path = Path(address)
    return (
        path.is_absolute()
        and path.name == SOCKET_NAME
        and bool(_socket_dir_re(prefix).fullmatch(path.parent.name))
    )


class AsyncChannel(ABC):
    """Backend end of a connected channel."""

    peer_pid: int

    @abstractmethod
    async def recv_into(self, view: memoryview, inactivity: float | None) -> None:
        """Fill ``view``. EOFError if the peer closed before the first byte, ConnectionError after."""

    @abstractmethod
    async def send(self, buffers: Sequence[Buffer], inactivity: float | None) -> None: ...

    @abstractmethod
    def close(self) -> None:
        """Abort pending I/O and release the channel (idempotent, callable from any thread)."""

    @property
    @abstractmethod
    def closed(self) -> bool: ...

    async def read_frame(
        self, inactivity: float | None, max_binary: int = MAX_BINARY_BYTES, max_json: int = MAX_CONTROL_BYTES
    ) -> tuple[int, bytearray]:
        head = bytearray(HEADER.size)
        await self.recv_into(memoryview(head), inactivity)
        kind, body_len = check_header(head, max_binary=max_binary, max_json=max_json)
        body = bytearray(body_len)
        if body_len:
            try:
                await self.recv_into(memoryview(body), inactivity)
            except EOFError:
                raise ConnectionResetError(tr("帧不完整：对端已关闭")) from None
        return kind, body

    async def send_message(self, message: Message, inactivity: float | None) -> None:
        await self.send(json_frame(message), inactivity)


class SyncChannel(ABC):
    """Helper end of a connected channel (blocking)."""

    peer_pid: int

    @abstractmethod
    def recv_into(self, view: memoryview, timeout: float | None) -> None:
        """Fill ``view``; EOFError before the first byte, ConnectionError after, TimeoutError."""

    @abstractmethod
    def send(self, buffers: Sequence[Buffer], timeout: float | None) -> None: ...

    @abstractmethod
    def close(self) -> None: ...

    def read_frame(
        self,
        first_timeout: float | None,
        timeout: float | None,
        max_binary: int,
        max_json: int = MAX_CONTROL_BYTES,
    ) -> tuple[int, bytearray]:
        """One frame; ``first_timeout`` bounds the wait for its first byte (the idle timeout)."""
        head = bytearray(HEADER.size)
        view = memoryview(head)
        self.recv_into(view[:1], first_timeout)
        try:
            self.recv_into(view[1:], timeout)
        except EOFError:
            raise ConnectionResetError(tr("帧不完整：对端已关闭")) from None
        kind, body_len = check_header(head, max_binary=max_binary, max_json=max_json)
        body = bytearray(body_len)
        if body_len:
            try:
                self.recv_into(memoryview(body), timeout)
            except EOFError:
                raise ConnectionResetError(tr("帧不完整：对端已关闭")) from None
        return kind, body

    def send_message(self, message: Message, timeout: float | None) -> None:
        self.send(json_frame(message), timeout)


if sys.platform == "win32":
    from . import _win32

    def _fill_pipe(pipe: _win32.Pipe, view: memoryview, timeout: float | None) -> None:
        got = 0
        while got < len(view):
            try:
                got += pipe.read_into(view[got:], timeout)
            except EOFError:
                if got:
                    raise ConnectionResetError(tr("帧不完整：管道已断开")) from None
                raise

    def _drain_pipe(pipe: _win32.Pipe, buffers: Sequence[Buffer], timeout: float | None) -> None:
        for buf in buffers:
            view = memoryview(buf).cast("B")
            sent = 0
            while sent < len(view):
                sent += pipe.write(view[sent:], timeout)

    class _PipeChannel(AsyncChannel):
        def __init__(self, pipe: _win32.Pipe) -> None:
            self._pipe = pipe
            self.peer_pid = pipe.client_pid()
            self._io = ThreadPoolExecutor(max_workers=1, thread_name_prefix="sk-helper-io")

        @property
        def closed(self) -> bool:
            return self._pipe.closed

        async def recv_into(self, view: memoryview, inactivity: float | None) -> None:
            await self._on_io_thread(_fill_pipe, view, inactivity)

        async def send(self, buffers: Sequence[Buffer], inactivity: float | None) -> None:
            await self._on_io_thread(_drain_pipe, buffers, inactivity)

        async def _on_io_thread(self, fn: Callable[..., None], *args: object) -> None:
            if self._pipe.closed:
                raise ConnectionAbortedError(tr("管道已关闭"))
            try:
                await asyncio.get_running_loop().run_in_executor(self._io, fn, self._pipe, *args)
            except RuntimeError as exc:  # the executor was shut down by close() meanwhile
                raise ConnectionAbortedError(tr("管道已关闭")) from exc

        def close(self) -> None:
            self._pipe.close()
            self._io.shutdown(wait=False)

    class _SyncPipe(SyncChannel):
        def __init__(self, pipe: _win32.Pipe) -> None:
            self._pipe = pipe
            self.peer_pid = pipe.server_pid()

        def recv_into(self, view: memoryview, timeout: float | None) -> None:
            _fill_pipe(self._pipe, view, timeout)

        def send(self, buffers: Sequence[Buffer], timeout: float | None) -> None:
            _drain_pipe(self._pipe, buffers, timeout)

        def close(self) -> None:
            self._pipe.close()

    class Listener:
        """The backend's end, created before the helper is launched; accepts exactly one connection."""

        def __init__(self, prefix: str = HELPER_PREFIX) -> None:
            self.address = "\\\\.\\pipe\\" + prefix + secrets.token_hex(16)
            self._pipe: _win32.Pipe | None = _win32.create_pipe_server(self.address)

        async def accept(self, wait: float) -> AsyncChannel:
            """Wait up to ``wait`` seconds for the helper to connect (TimeoutError otherwise; may retry)."""
            if self._pipe is None:
                raise ConnectionAbortedError(tr("监听已关闭"))
            await asyncio.get_running_loop().run_in_executor(None, self._pipe.accept, wait)
            pipe, self._pipe = self._pipe, None
            try:
                return _PipeChannel(pipe)
            except BaseException:
                pipe.close()
                raise

        def close(self) -> None:
            if self._pipe is not None:
                self._pipe.close()
                self._pipe = None

    def _connect(address: str, timeout: float) -> SyncChannel:
        pipe = _win32.connect_pipe(address, timeout)
        try:
            return _SyncPipe(pipe)
        except BaseException:
            pipe.close()
            raise

else:

    def _peer_cred(sock: socket.socket) -> tuple[int, int | None]:
        """(pid, uid) of the process at the other end of a connected unix socket."""
        if sys.platform == "darwin":
            (pid,) = struct.unpack("i", sock.getsockopt(0, 0x002, 4))  # SOL_LOCAL, LOCAL_PEERPID
            return int(pid), None  # the 0700 socket directory already limits who can connect
        pid, uid, _gid = struct.unpack("3i", sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        return int(pid), int(uid)

    class _SocketChannel(AsyncChannel):
        def __init__(self, sock: socket.socket) -> None:
            sock.setblocking(False)
            self._sock = sock
            self.peer_pid = _peer_cred(sock)[0]
            self._closed = False

        @property
        def closed(self) -> bool:
            return self._closed

        async def recv_into(self, view: memoryview, inactivity: float | None) -> None:
            if self._closed:
                raise ConnectionAbortedError(tr("套接字已关闭"))
            loop = asyncio.get_running_loop()
            got = 0
            while got < len(view):
                n = await asyncio.wait_for(loop.sock_recv_into(self._sock, view[got:]), inactivity)
                if n == 0:
                    if got:
                        raise ConnectionResetError(tr("帧不完整：对端已关闭"))
                    raise EOFError(tr("对端已关闭"))
                got += n

        async def send(self, buffers: Sequence[Buffer], inactivity: float | None) -> None:
            if self._closed:
                raise ConnectionAbortedError(tr("套接字已关闭"))
            loop = asyncio.get_running_loop()
            for buf in buffers:
                await asyncio.wait_for(loop.sock_sendall(self._sock, buf), inactivity)

        def close(self) -> None:
            if not self._closed:
                self._closed = True
                self._sock.close()

    class _SyncSocket(SyncChannel):
        """Non-blocking socket driven by select(), so sends and receives both honour timeouts."""

        def __init__(self, sock: socket.socket) -> None:
            self._sock = sock
            self.peer_pid = _peer_cred(sock)[0]
            sock.setblocking(False)

        def _wait(self, write: bool, timeout: float | None) -> None:
            deadline = None if timeout is None else time.monotonic() + timeout
            while True:
                remaining = None if deadline is None else max(0.0, deadline - time.monotonic())
                r, w, _ = select.select(
                    [] if write else [self._sock], [self._sock] if write else [], [], remaining
                )
                if r or w:
                    return
                if deadline is not None and time.monotonic() >= deadline:
                    raise TimeoutError(tr("套接字读写超时"))

        def recv_into(self, view: memoryview, timeout: float | None) -> None:
            got = 0
            while got < len(view):
                self._wait(False, timeout)
                try:
                    n = self._sock.recv_into(view[got:])
                except (BlockingIOError, InterruptedError):
                    continue
                except ConnectionResetError:
                    n = 0
                if n == 0:
                    if got:
                        raise ConnectionResetError(tr("帧不完整：对端已关闭"))
                    raise EOFError(tr("对端已关闭"))
                got += n

        def send(self, buffers: Sequence[Buffer], timeout: float | None) -> None:
            for buf in buffers:
                view = memoryview(buf).cast("B")
                sent = 0
                while sent < len(view):
                    self._wait(True, timeout)
                    try:
                        sent += self._sock.send(view[sent : sent + _SEND_CHUNK])
                    except (BlockingIOError, InterruptedError):
                        continue

        def close(self) -> None:
            self._sock.close()

    class Listener:
        """The backend's end, created before the helper is launched; accepts exactly one connection."""

        def __init__(self, prefix: str = HELPER_PREFIX) -> None:
            self._dir: str | None = tempfile.mkdtemp(prefix=prefix)  # 0700
            self.address = os.path.join(self._dir, SOCKET_NAME)
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                sock.bind(self.address)
                sock.listen(1)
                sock.setblocking(False)
            except BaseException:
                sock.close()
                self.close()
                raise
            self._sock: socket.socket | None = sock

        async def accept(self, wait: float) -> AsyncChannel:
            """Wait up to ``wait`` seconds for the helper to connect (TimeoutError otherwise; may retry)."""
            if self._sock is None:
                raise ConnectionAbortedError(tr("监听已关闭"))
            # Wait for readiness, then accept. wait_for(sock_accept()) drops a connection that arrives in
            # the same loop iteration as the timeout; a readiness wait consumes nothing, so a retry finds it.
            loop = asyncio.get_running_loop()
            ready = loop.create_future()
            fd = self._sock.fileno()

            def on_ready() -> None:
                if not ready.done():
                    ready.set_result(None)

            loop.add_reader(fd, on_ready)
            try:
                await asyncio.wait_for(ready, wait)
            finally:
                loop.remove_reader(fd)
            try:
                conn, _ = self._sock.accept()
            except (BlockingIOError, InterruptedError):
                raise TimeoutError from None  # a spurious wake-up: the caller retries
            try:
                _pid, uid = _peer_cred(conn)
                if uid is not None and uid not in (os.getuid(), 0):
                    raise PermissionError(tr("拒绝了用户 {uid} 的连接", uid=uid))
                self.close()  # one connection only: nobody else may connect after the helper
                return _SocketChannel(conn)
            except BaseException:
                conn.close()
                raise

        def close(self) -> None:
            sock, self._sock = getattr(self, "_sock", None), None
            if sock is not None:
                sock.close()
            if self._dir is not None:
                shutil.rmtree(self._dir, ignore_errors=True)
                self._dir = None

    def _connect(address: str, timeout: float) -> SyncChannel:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.settimeout(timeout)
            sock.connect(address)
            return _SyncSocket(sock)
        except BaseException:
            sock.close()
            raise


def connect(address: str, timeout: float, prefix: str = HELPER_PREFIX) -> SyncChannel:
    """Helper / plugin side: connect to the backend's listener (only to an address it could have made)."""
    if not valid_address(address, prefix):
        raise ValueError(tr("无效的助手地址：{address!r}", address=address))
    return _connect(address, timeout)


__all__ = [
    "HELPER_PREFIX",
    "KIND_JSON",
    "PLUGIN_PREFIX",
    "AsyncChannel",
    "FrameError",
    "Listener",
    "SyncChannel",
    "connect",
    "valid_address",
]

"""Backend side: launch the helper, authenticate it, and talk to it (docs/native.md 3).

>>> helper = await HelperClient.launch(elevate=True)    # UAC prompt unless already admin
>>> table = await helper.scan_mft("C:")                  # same dict as parse_mft_table
>>> await helper.close()

One request is in flight at a time (an asyncio lock serialises callers). Use
:class:`SharedHelper` to keep one helper per backend and reuse it across scans.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
from collections.abc import AsyncGenerator, Callable, Sequence
from pathlib import Path
from typing import Any

import numpy as np
from loguru import logger
from pydantic import ValidationError

from spaceknife_protocol.binary import BinaryFrameHeader, decode_frame
from spaceknife_protocol.errors import ProtocolError

from .auth import is_elevated, new_token, peer_is_launched, tokens_match, write_token_file
from .errors import HelperDenied, HelperError, HelperUnavailable
from .launch import HelperProcess, helper_command, start_elevated, start_subprocess
from .messages import (
    DEFAULT_BATCH_ROWS,
    KIND_BINARY,
    MFT_LAYOUT,
    MFT_STREAM,
    PROTOCOL_VERSION,
    REPLY,
    Begin,
    Call,
    Error,
    FrameError,
    Hello,
    Ping,
    Progress,
    Request,
    Response,
    Scalar,
    ScanMft,
    ScanMftImage,
    Shutdown,
    VolumeInfo,
)
from .service import IDLE_TIMEOUT
from .transport import AsyncChannel, Listener

CONNECT_TIMEOUT = 20.0
CONNECT_TIMEOUT_UAC = 60.0  # includes the time the UAC prompt is open
REQUEST_TIMEOUT = 60.0  # inactivity; a busy helper sends a heartbeat every second
HELLO_TIMEOUT = 10.0
CLOSE_TIMEOUT = 5.0
_ACCEPT_SLICE = 0.25
_U32_MAX = 2**32 - 1

# (phase "scan" | "send", seconds since the request, share of the $MFT read or None if not known)
ProgressCallback = Callable[[str, float, float | None], None]
Batch = dict[str, Any]
_Frame = tuple[BinaryFrameHeader, dict[str, np.ndarray]]


class HelperClient:
    """A connected, authenticated helper. Create with :meth:`launch`."""

    def __init__(
        self, channel: AsyncChannel, process: HelperProcess, hello: Hello, *, request_timeout: float
    ) -> None:
        self._channel = channel
        self._process = process
        self._hello = hello
        self._request_timeout = request_timeout
        self._lock = asyncio.Lock()
        self._next_id = 0
        self._broken: str | None = None
        self._closed = False
        # Result of the last finished scan: geometry (record_size, cluster_size, mft_size …), rows,
        # frames, names_bytes, wire_bytes and read/parse/send_seconds.
        self.last_scan: dict[str, Scalar] = {}

    # ------------------------------------------------------------------ launch

    @classmethod
    async def launch(
        cls,
        elevate: bool = True,
        *,
        connect_timeout: float | None = None,
        request_timeout: float = REQUEST_TIMEOUT,
        idle_timeout: float = IDLE_TIMEOUT,
        command: Sequence[str] | None = None,
    ) -> HelperClient:
        """Start a helper and wait until it has connected and authenticated.

        ``elevate=True`` shows the UAC prompt (Windows; skipped when this process already has
        administrator rights); ``elevate=False`` starts a plain subprocess. ``command`` replaces
        the interpreter + module prefix (tests). Raises HelperDenied when the user declines and
        HelperUnavailable for every other failure.
        """
        uac = elevate and not is_elevated()
        loop = asyncio.get_running_loop()
        timeout = (
            connect_timeout
            if connect_timeout is not None
            else CONNECT_TIMEOUT_UAC
            if uac
            else CONNECT_TIMEOUT
        )
        deadline = loop.time() + timeout
        token = new_token()
        try:
            listener = Listener()
        except OSError as exc:
            raise HelperUnavailable(f"无法创建助手通道：{exc}") from exc
        token_path: Path | None = None
        process: HelperProcess | None = None
        channel: AsyncChannel | None = None
        try:
            token_path = write_token_file(token)
            argv = [
                *(command or helper_command(uac)),
                "--pipe",
                listener.address,
                "--token-file",
                str(token_path),
                "--server-pid",
                str(os.getpid()),
                "--idle-timeout",
                f"{idle_timeout:g}",
            ]
            if uac:
                try:
                    process = await asyncio.wait_for(start_elevated(argv), timeout)
                except TimeoutError:
                    raise HelperUnavailable("等待管理员授权超时", "timeout") from None
            else:
                process = start_subprocess(argv)
            channel = await _accept(listener, process, deadline)
            hello = await _handshake(channel, process, token, deadline)
        except BaseException as exc:
            if channel is not None:
                channel.close()
            if process is not None:
                await _reap(process, 0)
            if isinstance(exc, OSError):
                raise HelperUnavailable(f"无法启动扫描助手：{exc}") from exc
            raise
        finally:
            listener.close()
            if token_path is not None:
                token_path.unlink(missing_ok=True)
        logger.info("扫描助手已就绪：pid {}，管理员权限 {}", hello.pid, hello.elevated)
        return cls(channel, process, hello, request_timeout=request_timeout)

    # ------------------------------------------------------------------ state

    @property
    def pid(self) -> int:
        """Pid of the helper interpreter (from its hello, verified against the OS)."""
        return self._hello.pid

    @property
    def elevated(self) -> bool:
        return self._hello.elevated

    @property
    def alive(self) -> bool:
        return (
            not self._closed
            and self._broken is None
            and not self._channel.closed
            and self._process.poll() is None
        )

    def _break(self, reason: str) -> None:
        if self._broken is None:
            self._broken = reason
            logger.warning("扫描助手连接中断：{}", reason)
        self._channel.close()

    def _check_usable(self) -> None:
        if self._closed:
            raise HelperUnavailable("扫描助手已关闭", "closed")
        if self._broken is not None:
            raise HelperUnavailable(f"扫描助手不可用：{self._broken}", "exited")

    # ------------------------------------------------------------------ exchange

    async def _read(self) -> tuple[int, bytearray]:
        try:
            return await self._channel.read_frame(self._request_timeout)
        except TimeoutError:
            self._break("响应超时")
            raise HelperUnavailable("扫描助手无响应", "timeout") from None
        except FrameError as exc:
            self._break(f"无效帧：{exc}")
            raise HelperUnavailable(f"扫描助手发送了无效帧：{exc}", "protocol") from None
        except (EOFError, OSError) as exc:
            self._break(f"连接断开：{exc}")
            raise HelperUnavailable("扫描助手已退出", "exited") from None
        except BaseException:
            self._break("读取被取消")  # a half-read frame leaves the stream unusable
            raise

    def _protocol_error(self, what: str) -> HelperUnavailable:
        self._break(what)
        return HelperUnavailable(f"扫描助手协议错误：{what}", "protocol")

    async def _exchange(
        self, call: Call, on_progress: ProgressCallback | None
    ) -> AsyncGenerator[_Frame | Begin | Response]:
        """Send one request; yield ``begin``, its bulk frames, then its response (an error raises).

        While the caller handles a bulk frame the next one is already being read (on Windows on
        the channel's I/O thread), so transfer and assembly overlap.
        """
        async with self._lock:
            self._check_usable()
            self._next_id += 1
            req_id = self._next_id
            try:
                await self._channel.send_message(Request(id=req_id, call=call), self._request_timeout)
            except (OSError, EOFError) as exc:
                self._break(f"发送失败：{exc}")
                raise HelperUnavailable("扫描助手已退出", "exited") from None
            except BaseException:
                self._break("发送被取消")
                raise
            done = False
            ahead: asyncio.Task[tuple[int, bytearray]] | None = None
            try:
                while True:
                    if ahead is not None:
                        kind, body = await ahead
                        ahead = None
                    else:
                        kind, body = await self._read()
                    if kind == KIND_BINARY:
                        frame = self._decode(body, req_id)
                        ahead = asyncio.ensure_future(self._read())
                        yield frame
                        continue
                    reply = self._control(body, req_id, on_progress)
                    if isinstance(reply, Begin):
                        yield reply
                    elif reply is not None:
                        done = True
                        if isinstance(reply, Error):
                            raise self._failure(reply)
                        yield reply
                        return
            finally:
                task = asyncio.current_task()
                if done or self._broken is not None or (task is not None and task.cancelling()):
                    if not done:
                        self._break("请求被中止")
                    _discard(ahead)
                else:
                    await self._drain(
                        req_id, ahead
                    )  # abandoned mid-stream: skip to the reply, keep the helper

    def _control(
        self, body: bytearray, req_id: int, on_progress: ProgressCallback | None
    ) -> Begin | Response | Error | None:
        """Handle one JSON frame: ``begin`` / the final reply are returned, progress is reported."""
        try:
            message = REPLY.validate_json(body)
        except ValidationError as exc:
            raise self._protocol_error(f"无法解析的消息（{exc.error_count()} 处错误）") from None
        if isinstance(message, Progress) and message.id == req_id:
            if on_progress is not None:
                read = min(1.0, message.done / message.total) if message.total else None
                on_progress(message.phase, message.elapsed, read)
            return None
        if isinstance(message, Begin | Response) and message.id == req_id:
            return message
        if isinstance(message, Error) and message.id in (req_id, None):
            return message
        raise self._protocol_error(f"意外的 {message.type} 消息")

    def _failure(self, error: Error) -> HelperError:
        if error.code == "bad_frame":  # the helper closes the connection after this
            self._break(error.message)
            return HelperUnavailable(error.message, "protocol")
        return HelperError(error.message, error.code)

    def _decode(self, body: bytearray, req_id: int) -> _Frame:
        try:
            header, columns = decode_frame(body)
        except ProtocolError as exc:
            raise self._protocol_error(f"无效的数据帧：{exc}") from None
        if header.stream != MFT_STREAM or header.layout != MFT_LAYOUT or header.request_id != req_id:
            raise self._protocol_error(f"意外的数据帧 {header.stream}/{header.layout}")
        return header, columns

    async def _drain(self, req_id: int, ahead: asyncio.Task[tuple[int, bytearray]] | None) -> None:
        with contextlib.suppress(HelperError):  # a broken channel has already been closed
            while True:
                if ahead is not None:
                    kind, body = await ahead
                    ahead = None
                else:
                    kind, body = await self._read()
                if kind != KIND_BINARY and isinstance(self._control(body, req_id, None), Response | Error):
                    return

    async def _simple(self, call: Call) -> dict[str, Scalar]:
        async with contextlib.aclosing(self._exchange(call, None)) as items:
            async for item in items:
                if isinstance(item, Response):
                    return dict(item.result)
                raise self._protocol_error("意外的数据帧")
        raise self._protocol_error("没有应答")

    # ------------------------------------------------------------------ operations

    async def ping(self) -> dict[str, Scalar]:
        """``{pid, elevated, version, platform}`` of the helper."""
        return await self._simple(Ping())

    async def volume_info(self, volume: str) -> dict[str, Scalar]:
        """Drive type, filesystem, label, serial, sector/cluster size of a local NTFS ``X:``;
        with administrator rights also record_size, mft_start_lcn, mft_valid_data_length …"""
        return await self._simple(_build(VolumeInfo, volume=volume))

    def stream_mft(
        self,
        volume: str,
        *,
        batch_rows: int = DEFAULT_BATCH_ROWS,
        duty: float = 1.0,
        on_progress: ProgressCallback | None = None,
    ) -> AsyncGenerator[Batch]:
        """Read and parse the $MFT of ``volume`` in the helper; yield batches of rows.

        Each batch is a dict of read-only numpy columns (docs/native.md 2.1) plus ``names``
        (uint8 array) with ``name_off`` relative to that batch. Use ``contextlib.aclosing``
        when stopping early: the rest of the stream is then skipped and the helper stays usable.
        """
        call = _build(ScanMft, volume=volume, batch_rows=batch_rows, duty=duty)
        return self._batches(call, on_progress)

    async def scan_mft(
        self,
        volume: str,
        *,
        batch_rows: int = DEFAULT_BATCH_ROWS,
        duty: float = 1.0,
        on_progress: ProgressCallback | None = None,
    ) -> dict[str, Any]:
        """The whole MftTable of ``volume``, the same dict ``parse_mft_table`` returns (columns as
        writable numpy arrays, ``names`` bytes, ``bad_records`` …). Geometry and timings are in
        :attr:`last_scan` afterwards. ``duty`` in (0, 1]: the $MFT reader is busy that share of
        the time (the scan's I/O limit)."""
        call = _build(ScanMft, volume=volume, batch_rows=batch_rows, duty=duty)
        return await self._table(call, on_progress)

    def stream_mft_image(
        self,
        path: str,
        *,
        batch_rows: int = DEFAULT_BATCH_ROWS,
        duty: float = 1.0,
        on_progress: ProgressCallback | None = None,
    ) -> AsyncGenerator[Batch]:
        """:meth:`stream_mft` on an NTFS volume image file (unelevated helpers only; tests)."""
        return self._batches(_build(ScanMftImage, path=path, batch_rows=batch_rows, duty=duty), on_progress)

    async def scan_mft_image(
        self,
        path: str,
        *,
        batch_rows: int = DEFAULT_BATCH_ROWS,
        duty: float = 1.0,
        on_progress: ProgressCallback | None = None,
    ) -> dict[str, Any]:
        """:meth:`scan_mft` on an NTFS volume image file (unelevated helpers only; tests)."""
        return await self._table(
            _build(ScanMftImage, path=path, batch_rows=batch_rows, duty=duty), on_progress
        )

    async def _batches(self, call: Call, on_progress: ProgressCallback | None) -> AsyncGenerator[Batch]:
        async with contextlib.aclosing(self._exchange(call, on_progress)) as items:
            async for item in items:
                if isinstance(item, Response):
                    self.last_scan = dict(item.result)
                elif not isinstance(item, Begin):
                    yield item[1]

    async def _table(self, call: Call, on_progress: ProgressCallback | None) -> dict[str, Any]:
        """Copy the batches straight into a table allocated from ``begin`` (no second copy)."""
        assembler: TableAssembler | None = None
        extras: dict[str, Scalar] = {}
        async with contextlib.aclosing(self._exchange(call, on_progress)) as items:
            async for item in items:
                if isinstance(item, Begin):
                    assembler = TableAssembler(item.rows)
                elif isinstance(item, Response):
                    self.last_scan = dict(item.result)
                    extras = dict(item.extras)
                elif assembler is None:
                    raise self._protocol_error("数据帧之前没有 begin")
                else:
                    try:
                        assembler.add(item[1])
                    except ValueError as exc:
                        raise self._protocol_error(str(exc)) from None
        if assembler is None:
            raise HelperUnavailable("扫描助手没有返回数据", "protocol")
        try:
            table = assembler.finish()
        except ValueError as exc:
            raise HelperUnavailable(f"扫描助手协议错误：{exc}", "protocol") from None
        table.update(extras)
        return table

    # ------------------------------------------------------------------ lifecycle

    async def close(self) -> None:
        """Ask the helper to exit, close the channel and reap the process (idempotent)."""
        if self._closed:
            return
        if self._broken is None and not self._channel.closed and not self._lock.locked():
            with contextlib.suppress(HelperError, OSError, TimeoutError):
                async with asyncio.timeout(2.0):
                    await self._simple(Shutdown())
        self._closed = True
        self._channel.close()
        await _reap(self._process, CLOSE_TIMEOUT)

    async def __aenter__(self) -> HelperClient:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.close()


class SharedHelper:
    """At most one helper per backend: launched on first use, reused while it lives.

    A helper exits after 10 idle minutes; the next :meth:`get` then launches (and, elevated,
    prompts) again.
    """

    def __init__(
        self,
        *,
        connect_timeout: float | None = None,
        request_timeout: float = REQUEST_TIMEOUT,
        idle_timeout: float = IDLE_TIMEOUT,
        command: Sequence[str] | None = None,
    ) -> None:
        self._options: dict[str, Any] = {
            "connect_timeout": connect_timeout,
            "request_timeout": request_timeout,
            "idle_timeout": idle_timeout,
            "command": command,
        }
        self._client: HelperClient | None = None
        self._lock = asyncio.Lock()

    @property
    def current(self) -> HelperClient | None:
        return self._client if self._client is not None and self._client.alive else None

    async def get(self, elevate: bool = True) -> HelperClient:
        """The live helper, or a newly launched one (HelperDenied / HelperUnavailable)."""
        async with self._lock:
            client = self._client
            if client is not None and client.alive and (client.elevated or not elevate):
                return client
            if client is not None:
                self._client = None
                await client.close()
            self._client = await HelperClient.launch(elevate, **self._options)
            return self._client

    async def close(self) -> None:
        async with self._lock:
            client, self._client = self._client, None
            if client is not None:
                await client.close()


# --------------------------------------------------------------------------- helpers


def _build(model: Any, **fields: Any) -> Any:
    try:
        return model(**fields)
    except ValidationError as exc:
        raise HelperError(f"无效的参数：{exc.errors()[0]['msg']}", "invalid_request") from None


async def _accept(listener: Listener, process: HelperProcess, deadline: float) -> AsyncChannel:
    """Wait for the helper to connect, giving up early if it exits."""
    loop = asyncio.get_running_loop()
    while True:
        remaining = deadline - loop.time()
        if remaining <= 0:
            raise HelperUnavailable("扫描助手没有按时连接", "timeout")
        try:
            return await listener.accept(min(_ACCEPT_SLICE, remaining))
        except TimeoutError:
            pass
        except PermissionError as exc:
            raise HelperUnavailable(f"扫描助手鉴权失败：{exc}", "auth") from None
        except OSError as exc:
            raise HelperUnavailable(f"等待扫描助手连接失败：{exc}", "exited") from None
        code = process.poll()
        if code is not None:
            raise HelperUnavailable(f"扫描助手启动后退出（退出码 {code}）", "exited")


async def _handshake(channel: AsyncChannel, process: HelperProcess, token: str, deadline: float) -> Hello:
    """First frame must be a hello with our token, from the process we launched."""
    timeout = max(0.5, min(HELLO_TIMEOUT, deadline - asyncio.get_running_loop().time()))
    try:
        kind, body = await channel.read_frame(timeout, max_binary=0)
    except TimeoutError:
        raise HelperUnavailable("扫描助手没有发送 hello", "timeout") from None
    except FrameError as exc:
        raise _auth_failed(f"首帧无效（{exc}）") from None
    except (EOFError, OSError) as exc:
        raise HelperUnavailable(f"扫描助手在握手时断开：{exc}", "exited") from None
    try:
        hello = REPLY.validate_json(body) if kind != KIND_BINARY else None
    except ValidationError:
        hello = None
    if not isinstance(hello, Hello):
        raise _auth_failed("首帧不是 hello")
    if hello.version != PROTOCOL_VERSION:
        raise _auth_failed(f"协议版本 {hello.version}，需要 {PROTOCOL_VERSION}")
    if not tokens_match(hello.token, token):
        raise _auth_failed("令牌不符")
    if hello.pid != channel.peer_pid:
        raise _auth_failed(f"声称的进程号 {hello.pid} 与实际连接方 {channel.peer_pid} 不符")
    if not peer_is_launched(channel.peer_pid, process.pid):
        raise _auth_failed(f"连接方 {channel.peer_pid} 不是启动的进程 {process.pid}")
    return hello


def _auth_failed(why: str) -> HelperUnavailable:
    logger.warning("扫描助手鉴权失败：{}", why)
    return HelperUnavailable(f"扫描助手鉴权失败：{why}", "auth")


async def _reap(process: HelperProcess, grace: float) -> None:
    """Wait up to ``grace`` seconds for the helper to exit, then kill it; release the handle."""
    if grace > 0 and process.poll() is None:
        await asyncio.to_thread(process.wait, grace)
    if process.poll() is None:
        process.terminate()
        await asyncio.to_thread(process.wait, 2.0)
    process.close()


def _discard(task: asyncio.Task[Any] | None) -> None:
    """Cancel a read-ahead nobody will await, without 'exception was never retrieved' noise."""
    if task is not None:
        task.add_done_callback(lambda t: t.cancelled() or t.exception())
        task.cancel()


class TableAssembler:
    """Builds one MftTable from batches: columns are allocated once (``rows`` from ``begin``)
    and each batch is copied into place; ``name_off`` is rebased onto the joined name blob."""

    def __init__(self, rows: int) -> None:
        self.rows = rows
        self._pos = 0
        self._columns: dict[str, np.ndarray] = {}
        self._names: list[np.ndarray] = []
        self._names_len = 0

    def add(self, batch: Batch) -> None:
        names = np.asarray(batch["names"], dtype=np.uint8)
        n = len(batch["record"])
        start, end = self._pos, self._pos + n
        keys = batch.keys() - {"names"}
        if end > self.rows:
            raise ValueError(f"数据行数超过声明的 {self.rows}")
        if self._columns and keys != self._columns.keys():
            raise ValueError("各批数据的列不一致")
        if self._names_len + len(names) > _U32_MAX:
            raise ValueError("名字总长超过 4 GiB")
        if n and int((batch["name_off"].astype(np.int64) + batch["name_len"]).max()) > len(names):
            raise ValueError("名字偏移越界")
        for key in keys:
            col = batch[key]
            out = self._columns.get(key)
            if out is None:
                out = self._columns[key] = np.empty(self.rows, dtype=col.dtype)
            if len(col) != n or out.dtype != col.dtype:
                raise ValueError(f"列 {key!r} 的长度或类型不一致")
            if key == "name_off":
                np.add(col, np.uint32(self._names_len), out=out[start:end])
            else:
                out[start:end] = col
        self._names.append(names)
        self._names_len += len(names)
        self._pos = end

    def finish(self) -> dict[str, Any]:
        if self._pos != self.rows or not self._columns:
            raise ValueError(f"声明 {self.rows} 行，收到 {self._pos} 行")
        table: dict[str, Any] = dict(self._columns)
        table["names"] = b"".join(memoryview(part) for part in self._names)
        return table


def concat_batches(batches: Sequence[Batch]) -> dict[str, Any]:
    """Join MFT batches (as :meth:`HelperClient.stream_mft` yields them) into one table."""
    assembler = TableAssembler(sum(len(batch["record"]) for batch in batches))
    for batch in batches:
        assembler.add(batch)
    return assembler.finish()


__all__ = [
    "HelperClient",
    "HelperDenied",
    "HelperError",
    "HelperUnavailable",
    "SharedHelper",
    "TableAssembler",
    "concat_batches",
]

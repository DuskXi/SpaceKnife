"""The helper's side: a whitelist of read-only operations behind the authenticated channel.

Requests are handled one at a time on the main thread. While a long operation runs, a small
thread sends ``progress`` heartbeats (frames are written under one lock, so they interleave
cleanly with the bulk frames). Nothing here opens anything for writing.
"""

from __future__ import annotations

import importlib
import json
import os
import sys
import threading
import time
from collections.abc import Iterator, Mapping
from concurrent.futures import Future, ThreadPoolExecutor
from contextlib import closing, contextmanager
from types import ModuleType
from typing import Any, Literal

import numpy as np
from loguru import logger
from pydantic import ValidationError

from spaceknife_protocol.binary import ALIGN, ALLOWED_DTYPES, BinaryFrameHeader, BlobSpec, ColumnSpec

from ..i18n import _, tr
from . import usn
from .messages import (
    DEFAULT_BATCH_ROWS,
    KIND_BINARY,
    KIND_JSON,
    MAX_CONTROL_BYTES,
    MAX_ID,
    MFT_LAYOUT,
    MFT_STREAM,
    PROTOCOL_VERSION,
    USN_LAYOUT,
    USN_STREAM,
    Begin,
    DiskHealth,
    Error,
    ErrorCode,
    FrameError,
    Message,
    Ping,
    Progress,
    QueryUsn,
    ReadUsn,
    Request,
    Response,
    Scalar,
    ScanMft,
    ScanMftImage,
    Shutdown,
    VolumeInfo,
    frame_header,
    json_frame,
)
from .transport import Buffer, SyncChannel

if sys.platform == "win32":
    from . import _win32

IDLE_TIMEOUT = 600.0
IO_TIMEOUT = 120.0
HEARTBEAT_SECONDS = 1.0
NAMES_BUDGET = 256 << 20  # name bytes per bulk frame, so frames stay far below the size limit
_LOCAL_DRIVES = (2, 3, 6)  # removable, fixed, RAM disk
MFT_COLUMNS = ("record", "name_off", "name_len")  # required in an MftTable (split_table)
# what journal operations fail with (anything else is a bug: logged and reported as internal)
_USN_FAILURES = (usn.UsnError, OSError, ValueError, NotImplementedError)


class OpError(Exception):
    def __init__(self, code: ErrorCode, message: str) -> None:
        super().__init__(message)
        self.code: ErrorCode = code
        self.message = message


class ChannelClosed(Exception):
    """Writing to the backend failed: the helper stops."""


class Service:
    def __init__(self, channel: SyncChannel, *, elevated: bool, io_timeout: float = IO_TIMEOUT) -> None:
        self._chan = channel
        self._elevated = elevated
        self._io_timeout = io_timeout
        self._write_lock = threading.Lock()

    # ------------------------------------------------------------------ sending

    def _send_frame(self, buffers: list[Buffer]) -> None:
        with self._write_lock:
            try:
                self._chan.send(buffers, self._io_timeout)
            except (OSError, EOFError) as exc:
                raise ChannelClosed(str(exc)) from exc

    def send(self, message: Message) -> None:
        self._send_frame([*json_frame(message)])

    def _error(self, req_id: int | None, code: ErrorCode, message: str) -> None:
        self.send(Error(id=req_id, code=code, message=message[:4096]))

    # ------------------------------------------------------------------ loop

    def serve(self, idle_timeout: float = IDLE_TIMEOUT) -> None:
        """Handle requests until shutdown, EOF, idle timeout or a broken stream."""
        try:
            self._serve(idle_timeout)
        except ChannelClosed as exc:
            logger.debug("connection closed: {}", exc)

    def _serve(self, idle_timeout: float) -> None:
        while True:
            try:
                kind, body = self._chan.read_frame(idle_timeout, self._io_timeout, MAX_CONTROL_BYTES)
            except TimeoutError:
                logger.info("idle for {:.0f} s, exiting", idle_timeout)
                return
            except EOFError:
                logger.debug("the backend disconnected")
                return
            except FrameError as exc:
                logger.warning(tr("收到无效帧：{reason}", reason=exc))
                self._error(None, "bad_frame", str(exc))
                return
            except OSError as exc:
                logger.debug("reading a request failed: {}", exc)
                return
            if kind != KIND_JSON:
                self._error(None, "invalid_request", _("助手只接受 JSON 请求帧"))
                continue
            try:
                request = Request.model_validate_json(body)
            except ValidationError as exc:
                self._error(_guess_id(body), "invalid_request", _summarize(exc))
                continue
            if not self._dispatch(request):
                return

    def _dispatch(self, request: Request) -> bool:
        call = request.call
        logger.debug("request {} {}", request.id, call.op)
        extras: dict[str, Scalar] = {}
        try:
            if isinstance(call, Shutdown):
                self.send(Response(id=request.id))
                return False
            if isinstance(call, Ping):
                result: dict[str, Scalar] = {
                    "pid": os.getpid(),
                    "elevated": self._elevated,
                    "version": PROTOCOL_VERSION,
                    "platform": sys.platform,
                }
            elif isinstance(call, VolumeInfo):
                result = self._volume_info(call.volume)
            elif isinstance(call, QueryUsn):
                result = self._query_usn(call)
            elif isinstance(call, ReadUsn):
                result, extras = self._read_usn(request.id, call)
            elif isinstance(call, ScanMft):
                result, extras = self._scan_mft(request.id, call)
            elif isinstance(call, DiskHealth):
                result = dict(self._disk_health(call))
            else:
                result, extras = self._scan_mft_image(request.id, call)
        except OpError as exc:
            self._error(request.id, exc.code, exc.message)
            return True
        except ChannelClosed:
            raise
        except Exception as exc:
            logger.exception(tr("处理 {op} 失败", op=call.op))
            self._error(request.id, "internal", f"{type(exc).__name__}: {exc}")
            return True
        self.send(Response(id=request.id, result=result, extras=extras))
        return True

    # ------------------------------------------------------------------ operations

    def _volume_info(self, volume: str) -> dict[str, Scalar]:
        details = _check_ntfs(volume)
        result: dict[str, Scalar] = {"volume": volume.upper(), **details, "elevated": self._elevated}
        if self._elevated and sys.platform == "win32":
            try:
                result.update(_win32.ntfs_volume_data(volume))
            except OSError as exc:
                raise OpError("io_error", tr("读取 NTFS 卷参数失败：{reason}", reason=exc)) from exc
        return result

    def _scan_mft(self, req_id: int, call: ScanMft) -> tuple[dict[str, Scalar], dict[str, Scalar]]:
        if not self._elevated:
            raise OpError("needs_admin", _("需要管理员权限"))
        if sys.platform != "win32":
            raise OpError("not_supported", _("MFT 快速扫描只支持 Windows NTFS 卷"))
        _check_ntfs(call.volume)
        return self._scan(req_id, "scan_volume", call.volume.upper(), call.batch_rows, call.duty)

    def _scan_mft_image(self, req_id: int, call: ScanMftImage) -> tuple[dict[str, Scalar], dict[str, Scalar]]:
        if self._elevated:
            raise OpError("forbidden", _("scan_mft_image 只在非提权助手中可用"))
        return self._scan(req_id, "scan_volume_image", call.path, call.batch_rows, call.duty)

    def _disk_health(self, call: DiskHealth) -> dict[str, str]:
        """The raw health pages of a Linux disk (``disks.passthrough``): NVMe log 02h, or ATA
        IDENTIFY and the Device Statistics pages, as hex."""
        if not sys.platform.startswith("linux"):
            raise OpError("not_supported", _("助手只在 Linux 上读取磁盘自己的健康记录"))
        if not self._elevated:
            raise OpError("needs_admin", _("需要管理员权限"))
        from ..disks import passthrough

        try:
            return passthrough.read_record(call.disk)
        except passthrough.PassthroughError as exc:
            raise OpError("not_supported", exc.message) from exc
        except (OSError, ValueError) as exc:
            raise OpError("io_error", tr("读取 {disk} 失败：{reason}", disk=call.disk, reason=exc)) from exc

    def _usn_volume(self, volume: str) -> str:
        """``X:`` of a local NTFS volume whose journal the (elevated) helper may read."""
        if not self._elevated:
            raise OpError("needs_admin", _("需要管理员权限"))
        _check_ntfs(volume)
        return volume.upper()

    def _query_usn(self, call: QueryUsn) -> dict[str, Scalar]:
        volume = self._usn_volume(call.volume)
        try:
            with closing(usn.open_volume(volume)) as device:
                journal = device.query()
        except _USN_FAILURES as exc:
            raise usn_failure(exc) from exc
        return {"volume": volume, **journal.scalars()}

    def _read_usn(self, req_id: int, call: ReadUsn) -> tuple[dict[str, Scalar], dict[str, Scalar]]:
        """Read the journal from the checkpoint (at most ``max_rows`` records), then stream the
        rows. Returns (statistics, extras: journal_id, next_usn, rows, truncated)."""
        volume = self._usn_volume(call.volume)
        with self._heartbeat(req_id, "scan") as beat:
            t0 = time.perf_counter()
            try:
                with closing(usn.open_volume(volume)) as device:
                    table, stats = usn.read_journal(
                        device,
                        call.start_usn,
                        call.journal_id,
                        max_rows=call.max_rows,
                        duty=call.duty,
                        progress=beat.reading,
                    )
            except _USN_FAILURES as exc:
                raise usn_failure(exc) from exc
            t1 = time.perf_counter()
            beat.phase = "send"
            sent, extras = self._send_table(
                req_id,
                table,
                DEFAULT_BATCH_ROWS,
                stream=USN_STREAM,
                layout=USN_LAYOUT,
                required=tuple(usn.COLUMNS),
            )
            del table
            timing = {"read_seconds": t1 - t0, "send_seconds": time.perf_counter() - t1}
        return {"volume": volume, **stats, **sent, **timing}, extras

    def _scan(
        self,
        req_id: int,
        scanner: Literal["scan_volume", "scan_volume_image"],
        source: str,
        batch_rows: int,
        duty: float,
    ) -> tuple[dict[str, Scalar], dict[str, Scalar]]:
        """Read and parse the $MFT (streaming: never all of it in memory), then stream the columns.
        Returns (statistics, extras)."""
        mft = _mft_module()
        with self._heartbeat(req_id, "scan") as beat:
            t0 = time.perf_counter()
            try:
                table, geometry = _unpack_scan(getattr(mft, scanner)(source, duty, beat.reading))
            except PermissionError as exc:
                raise OpError("needs_admin", tr("需要管理员权限：{reason}", reason=exc)) from exc
            except NotImplementedError as exc:
                raise OpError("not_supported", tr("MFT 读取不可用：{reason}", reason=exc)) from exc
            except (OSError, ValueError) as exc:
                raise OpError("io_error", tr("读取 $MFT 失败：{reason}", reason=exc)) from exc
            t1 = time.perf_counter()
            beat.phase = "send"
            stats, extras = self._send_table(req_id, table, batch_rows)
            del table
            timing = {"scan_seconds": t1 - t0, "send_seconds": time.perf_counter() - t1}
        return {**geometry, **stats, **timing}, extras

    def _send_table(
        self,
        req_id: int,
        table: Mapping[str, Any],
        batch_rows: int,
        *,
        stream: str = MFT_STREAM,
        layout: str = MFT_LAYOUT,
        required: tuple[str, ...] = MFT_COLUMNS,
    ) -> tuple[dict[str, Scalar], dict[str, Scalar]]:
        """Bulk frames of ``batch_rows`` rows; frame k+1 is encoded while frame k is written."""
        columns, names, extras = split_table(table, required)
        rows = len(columns["name_len"])
        bounds = batch_bounds(columns["name_len"], batch_rows, NAMES_BUDGET)
        self.send(Begin(id=req_id, rows=rows, frames=len(bounds)))
        wire = 0
        pending: Future[None] | None = None
        with ThreadPoolExecutor(max_workers=1, thread_name_prefix="sk-helper-send") as writer:
            try:
                for seq, (a, b) in enumerate(bounds):
                    batch = {key: col[a:b] for key, col in columns.items()}
                    blob, batch["name_off"] = rebase_names(
                        names, columns["name_off"][a:b], columns["name_len"][a:b]
                    )
                    parts, size = frame_parts(
                        stream=stream,
                        layout=layout,
                        columns=batch,
                        seq=seq,
                        final=seq == len(bounds) - 1,
                        request_id=req_id,
                        blobs={"names": blob},
                    )
                    del batch, blob
                    if pending is not None:
                        pending.result()  # at most one frame in flight
                    pending = writer.submit(self._send_frame, [frame_header(KIND_BINARY, size), *parts])
                    wire += size
                    del parts
                if pending is not None:
                    pending.result()
            finally:
                if pending is not None and not pending.done():
                    pending.cancel()
        stats: dict[str, Scalar] = {
            "rows": rows,
            "frames": len(bounds),
            "names_bytes": len(names),
            "wire_bytes": wire,
        }
        return stats, extras

    @contextmanager
    def _heartbeat(self, req_id: int, phase: Phase) -> Iterator[_Beat]:
        beat = _Beat(phase)
        stop = threading.Event()
        start = time.perf_counter()

        def run() -> None:
            while not stop.wait(HEARTBEAT_SECONDS):
                try:
                    self.send(
                        Progress(
                            id=req_id,
                            phase=beat.phase,
                            elapsed=time.perf_counter() - start,
                            done=beat.done,
                            total=beat.total,
                        )
                    )
                except ChannelClosed:
                    return  # the main thread notices on its next write

        thread = threading.Thread(target=run, name="sk-helper-beat", daemon=True)
        thread.start()
        try:
            yield beat
        finally:
            stop.set()
            thread.join()


Phase = Literal["scan", "send"]


class _Beat:
    def __init__(self, phase: Phase) -> None:
        self.phase: Phase = phase
        self.done = 0  # bytes of $MFT parsed / of journal read, of total (sent with every heartbeat)
        self.total = 0

    def reading(self, done: int, total: int) -> None:
        """The scanner's progress callback (from the parsing thread: two plain int stores)."""
        self.done, self.total = int(done), int(total)


# --------------------------------------------------------------------------- helpers


def usn_failure(exc: BaseException) -> OpError:
    """The error a failed journal operation is reported with."""
    if isinstance(exc, OpError):
        return exc
    if isinstance(exc, usn.UsnError):
        return OpError(exc.code, exc.message)
    if isinstance(exc, PermissionError):
        return OpError("needs_admin", tr("需要管理员权限：{reason}", reason=exc))
    if isinstance(exc, NotImplementedError):
        return OpError("not_supported", tr("USN 日志不可用：{reason}", reason=exc))
    if isinstance(exc, OSError):
        return OpError("io_error", tr("读取 USN 日志失败：{reason}", reason=exc))
    if isinstance(exc, ValueError):
        return OpError("io_error", tr("USN 日志记录无法解析：{reason}", reason=exc))
    return OpError("internal", f"{type(exc).__name__}: {exc}")


def _mft_module() -> ModuleType:
    try:
        return importlib.import_module("spaceknife.scan.mft")
    except ImportError as exc:
        raise OpError("unavailable", tr("MFT 解析模块不可用：{reason}", reason=exc)) from exc


def _unpack_scan(raw: object) -> tuple[Mapping[str, Any], dict[str, int]]:
    """``scan_volume`` / ``scan_volume_image`` → (MftTable, geometry of ``mft_volume_info``)."""
    try:
        if not isinstance(raw, tuple) or len(raw) != 2:
            raise TypeError(tr("需要 (table, info)，得到 {type}", type=type(raw).__name__))
        data, info = raw
        if not isinstance(data, Mapping):
            raise TypeError(tr("表应为 dict，得到 {type}", type=type(data).__name__))
        geometry = {str(key): int(value) for key, value in dict(info).items()}
        missing = {"record_size", "cluster_size"} - geometry.keys()
        if missing:
            raise KeyError(sorted(missing))
    except (TypeError, ValueError, KeyError) as exc:
        raise OpError("internal", tr("MFT 读取结果无法识别：{reason!r}", reason=exc)) from exc
    return data, geometry


def _check_ntfs(volume: str) -> dict[str, Scalar]:
    """Facts about a local NTFS volume ``X:``; OpError for anything else."""
    if sys.platform == "win32":
        details = _win32.volume_details(volume)
        if details["drive_type"] not in _LOCAL_DRIVES:
            raise OpError("not_ntfs", tr("{volume} 不是本地卷", volume=volume.upper()))
        if str(details["device"]).startswith("\\??\\"):
            raise OpError("not_ntfs", tr("{volume} 是 SUBST 映射的文件夹，不是卷", volume=volume.upper()))
        if details["fs_type"] != "NTFS":
            fs = details["fs_type"] or _("无法识别")
            raise OpError("not_ntfs", tr("{volume} 不是 NTFS 卷（{fs}）", volume=volume.upper(), fs=fs))
        return dict(details)
    raise OpError("not_supported", _("卷信息只支持 Windows"))


def frame_parts(
    *,
    stream: str,
    layout: str,
    columns: Mapping[str, np.ndarray],
    seq: int,
    final: bool,
    request_id: int,
    blobs: Mapping[str, bytes],
) -> tuple[list[Buffer], int]:
    """The exact bytes of :func:`spaceknife_protocol.binary.encode_frame`, as a list of buffers.

    ``encode_frame`` copies every column twice (``tobytes`` and the final join); here contiguous
    little-endian columns are handed to the channel as views and written straight from the
    table's memory. Tests assert byte equality with ``encode_frame``. Returns (parts, size).
    """
    count: int | None = None
    specs: dict[str, ColumnSpec] = {}
    parts: list[Buffer] = []
    offset = 0
    for name, array in columns.items():
        arr = np.asarray(array)
        dtype = arr.dtype.newbyteorder("<") if arr.dtype.byteorder == ">" else arr.dtype
        code = np.dtype(dtype).str.replace("|", "<")
        if arr.ndim != 1 or code not in ALLOWED_DTYPES:
            raise OpError(
                "internal",
                tr(
                    "数据列 {name!r} 无法编码（{dtype}，{ndim} 维）",
                    name=name,
                    dtype=arr.dtype,
                    ndim=arr.ndim,
                ),
            )
        if count is None:
            count = arr.shape[0]
        elif arr.shape[0] != count:
            raise OpError("internal", tr("数据列 {name!r} 的行数不一致", name=name))
        view = memoryview(np.ascontiguousarray(arr, dtype=np.dtype(code))).cast("B")
        specs[name] = ColumnSpec(offset=offset, dtype=code)
        offset = _append_padded(parts, view, offset)
    blob_specs: dict[str, BlobSpec] = {}
    for name, blob in blobs.items():
        blob_specs[name] = BlobSpec(offset=offset, length=len(blob))
        offset = _append_padded(parts, memoryview(blob).cast("B"), offset)
    header = BinaryFrameHeader(
        stream=stream,
        request_id=request_id,
        seq=seq,
        final=final,
        layout=layout,
        count=count or 0,
        columns=specs,
        blobs=blob_specs,
    )
    header_bytes = header.model_dump_json(exclude_none=True).encode("utf-8")
    prefix = len(header_bytes).to_bytes(4, "little") + header_bytes
    prefix += b"\0" * (-len(prefix) % ALIGN)
    return [prefix, *parts], len(prefix) + offset


def _append_padded(parts: list[Buffer], view: memoryview, offset: int) -> int:
    if len(view):
        parts.append(view)
    pad = -len(view) % ALIGN
    if pad:
        parts.append(b"\0" * pad)
    return offset + len(view) + pad


def split_table(
    table: Mapping[str, Any], required: tuple[str, ...] = MFT_COLUMNS
) -> tuple[dict[str, np.ndarray], np.ndarray, dict[str, Scalar]]:
    """MftTable (or USN rows) → (equal-length 1-D columns, names as uint8, scalar extras such as
    bad_records). ``required``: the columns the table must have besides name_off / name_len."""
    columns: dict[str, np.ndarray] = {}
    extras: dict[str, Scalar] = {}
    names: np.ndarray | None = None
    for key, value in table.items():
        if key == "names":
            names = (
                np.frombuffer(value, dtype=np.uint8)
                if not isinstance(value, np.ndarray)
                else value.view(np.uint8)
            )
        elif isinstance(value, np.ndarray):
            if value.ndim != 1:
                raise OpError("internal", tr("MFT 表的列 {key!r} 不是一维数组", key=key))
            columns[key] = value
        elif isinstance(value, np.generic):
            extras[key] = value.item()
        elif value is None or isinstance(value, bool | int | float | str):
            extras[key] = value
        else:
            raise OpError(
                "internal", tr("MFT 表的 {key!r}（{type}）无法传输", key=key, type=type(value).__name__)
            )
    missing = {"name_off", "name_len", *required} - columns.keys()
    if names is None or missing:
        raise OpError("internal", tr("表缺少 {columns}", columns=sorted(missing) or ["names"]))
    rows = len(columns["name_len"])
    uneven = [key for key, col in columns.items() if len(col) != rows]
    if uneven:
        raise OpError("internal", tr("MFT 表的列长度不一致：{columns}", columns=uneven))
    return columns, names, extras


def batch_bounds(name_len: np.ndarray, batch_rows: int, names_budget: int) -> list[tuple[int, int]]:
    """Row ranges of at most ``batch_rows`` rows and (unless one name is larger) ``names_budget`` bytes."""
    n = len(name_len)
    if n == 0:
        return [(0, 0)]  # one empty frame still carries the column dtypes
    cum = np.cumsum(name_len, dtype=np.int64)
    out: list[tuple[int, int]] = []
    a = 0
    while a < n:
        base = int(cum[a - 1]) if a else 0
        b = min(n, a + batch_rows, int(np.searchsorted(cum, base + names_budget, side="right")))
        b = max(b, a + 1)
        out.append((a, b))
        a = b
    return out


def rebase_names(names: np.ndarray, off: np.ndarray, length: np.ndarray) -> tuple[bytes, np.ndarray]:
    """The names of one batch as their own blob, plus offsets into it.

    A contiguous (or nearly so) range is sliced; scattered names are gathered in row order.
    """
    if len(off) == 0:
        return b"", np.zeros(0, dtype=np.uint32)
    start = off.astype(np.int64)
    end = start + length
    lo, hi = int(start.min()), int(end.max())
    if lo < 0 or hi > len(names):
        raise OpError("internal", _("MFT 表的名字偏移越界"))
    total = int(length.sum(dtype=np.int64))
    if hi - lo <= 2 * total + 4096:
        return names[lo:hi].tobytes(), (start - lo).astype(np.uint32)
    packed = np.zeros(len(off), dtype=np.int64)
    np.cumsum(length[:-1], out=packed[1:])
    index = np.repeat(start - packed, length) + np.arange(total, dtype=np.int64)
    return names[index].tobytes(), packed.astype(np.uint32)


def _guess_id(body: bytes | bytearray) -> int | None:
    try:
        value = json.loads(body).get("id")
    except (ValueError, AttributeError):
        return None
    return value if isinstance(value, int) and not isinstance(value, bool) and 1 <= value <= MAX_ID else None


def _summarize(exc: ValidationError) -> str:
    parts = [f"{'.'.join(str(p) for p in err['loc']) or _('请求')}: {err['msg']}" for err in exc.errors()[:5]]
    return tr("无效请求：{details}", details=_("；").join(parts))


__all__ = [
    "IDLE_TIMEOUT",
    "IO_TIMEOUT",
    "ChannelClosed",
    "OpError",
    "Service",
    "batch_bounds",
    "frame_parts",
    "rebase_names",
    "split_table",
    "usn_failure",
]

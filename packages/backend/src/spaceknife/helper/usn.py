"""NTFS change journal (USN) for the helper's ``query_usn`` / ``read_usn`` (docs/monitor.md 3).

The backend keeps a checkpoint ``(journal_id, next_usn)`` per scanned volume. When a watcher
loses events it asks the helper for the records written since, and lists again the folders
they name (``parent_frn``). Everything here is read-only: the volume is opened with
``GENERIC_READ`` and only ``FSCTL_QUERY_USN_JOURNAL`` / ``FSCTL_READ_USN_JOURNAL`` are issued.

:func:`parse_read_buffer` is a pure numpy parser of what ``FSCTL_READ_USN_JOURNAL`` returns
(``USN_RECORD_V2`` and ``V3``), testable on every platform. :func:`read_journal` drives any
:class:`UsnDevice` (the ctypes one from :func:`open_volume`, or a test double) and returns the
rows as columns. File references stay 64-bit: V3 records carry 128-bit ids, of which the low
64 bits are the NTFS file reference (record number in the low 48 bits, sequence above); the
helper only serves NTFS volumes, where the high half is zero.

Every failure is a :class:`UsnError` whose ``code`` is the helper's error code.
"""

from __future__ import annotations

import struct
import sys
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from typing import Any, Protocol

import numpy as np

from ..i18n import _, tr
from .messages import ErrorCode

# Win32 errors the journal FSCTLs report (winerror.h)
ERROR_INVALID_FUNCTION = 1  # not a filesystem with a change journal (FAT, exFAT …)
ERROR_ACCESS_DENIED = 5
ERROR_NOT_SUPPORTED = 50
ERROR_INVALID_PARAMETER = 87
ERROR_JOURNAL_DELETE_IN_PROGRESS = 1178
ERROR_JOURNAL_NOT_ACTIVE = 1179
ERROR_JOURNAL_ENTRY_DELETED = 1181  # start USN below the first record still kept
ERROR_PRIVILEGE_NOT_HELD = 1314

FSCTL_READ_USN_JOURNAL = 0x000900BB
FSCTL_QUERY_USN_JOURNAL = 0x000900F4
ALL_REASONS = 0xFFFFFFFF

MIN_BUFFER = 4 << 10  # holds the largest record (76 + 255 UTF-16 units) several times over
MAX_BUFFER = 1 << 20
_BYTES_PER_ROW = 128  # buffer sizing for small reads: a typical record is 80–120 bytes
_U32_MAX = 2**32 - 1

# Output columns of read_usn, layout usn.v1 (plus the names blob)
COLUMNS: dict[str, type[np.generic]] = {
    "frn": np.uint64,
    "parent_frn": np.uint64,
    "usn": np.int64,
    "reason": np.uint32,
    "attrs": np.uint32,
    "name_off": np.uint32,
    "name_len": np.uint16,
}

_HEAD = struct.Struct("<IH")  # RecordLength, MajorVersion
_NEXT_USN = struct.Struct("<q")
# USN_RECORD_V2 / V3 up to the file name (packed, little-endian; FileName follows at name_off)
_V2 = np.dtype(
    [
        ("length", "<u4"),
        ("major", "<u2"),
        ("minor", "<u2"),
        ("frn", "<u8"),
        ("parent", "<u8"),
        ("usn", "<i8"),
        ("time", "<i8"),
        ("reason", "<u4"),
        ("source", "<u4"),
        ("security", "<u4"),
        ("attrs", "<u4"),
        ("name_len", "<u2"),
        ("name_off", "<u2"),
    ]
)
_V3 = np.dtype(
    [
        ("length", "<u4"),
        ("major", "<u2"),
        ("minor", "<u2"),
        ("frn", "<u8"),  # FILE_ID_128: low half
        ("frn_hi", "<u8"),
        ("parent", "<u8"),
        ("parent_hi", "<u8"),
        ("usn", "<i8"),
        ("time", "<i8"),
        ("reason", "<u4"),
        ("source", "<u4"),
        ("security", "<u4"),
        ("attrs", "<u4"),
        ("name_len", "<u2"),
        ("name_off", "<u2"),
    ]
)
_LAYOUTS = {2: _V2, 3: _V3}
_USN_AT = {2: 24, 3: 40}  # offset of the Usn field
assert _V2.itemsize == 60
assert _V3.itemsize == 76


class UsnError(Exception):
    """A journal operation failed; ``code`` is the helper's error code (``usn_gone`` …)."""

    def __init__(self, code: ErrorCode, message: str) -> None:
        super().__init__(message)
        self.code: ErrorCode = code
        self.message = message


def error_code(winerror: int) -> ErrorCode:
    """The helper's error code for a Win32 error of the journal FSCTLs."""
    if winerror in (ERROR_JOURNAL_NOT_ACTIVE, ERROR_JOURNAL_DELETE_IN_PROGRESS):
        return "usn_inactive"
    if winerror == ERROR_JOURNAL_ENTRY_DELETED:
        return "usn_gone"
    if winerror in (ERROR_ACCESS_DENIED, ERROR_PRIVILEGE_NOT_HELD):
        return "needs_admin"
    if winerror in (ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED):
        return "not_supported"
    return "io_error"


@dataclass(frozen=True, slots=True)
class Journal:
    """``USN_JOURNAL_DATA`` of a volume."""

    journal_id: int  # u64; changes when the journal is deleted and created again
    first_usn: int  # oldest record still kept
    next_usn: int  # where the next record will be written
    lowest_valid_usn: int  # records below were written by an earlier journal instance
    max_usn: int
    max_size: int  # bytes the journal may grow to before old records are dropped
    allocation_delta: int
    max_major: int = 2  # newest record version the volume writes (V1 data; 2 when not reported)

    def scalars(self) -> dict[str, int]:
        return {
            "journal_id": self.journal_id,
            "first_usn": self.first_usn,
            "next_usn": self.next_usn,
            "lowest_valid_usn": self.lowest_valid_usn,
            "max_usn": self.max_usn,
            "max_size": self.max_size,
            "allocation_delta": self.allocation_delta,
        }


class UsnDevice(Protocol):
    """An open volume's change journal (see :func:`open_volume`)."""

    def query(self) -> Journal: ...

    def read(self, start_usn: int, journal_id: int, size: int) -> bytes:
        """One ``FSCTL_READ_USN_JOURNAL`` of at most ``size`` bytes from ``start_usn``: the next
        USN (8 bytes) followed by whole records; returns at once when there are none."""
        ...

    def close(self) -> None: ...


# --------------------------------------------------------------------------- parsing


@dataclass(frozen=True, slots=True)
class UsnChunk:
    """The records of one read buffer, as columns (:data:`COLUMNS`) and a UTF-8 names blob."""

    next_usn: int  # where to continue: the buffer's next USN, or the first record not returned
    columns: dict[str, np.ndarray]
    names: bytes
    truncated: bool  # ``limit`` records returned and more were in the buffer
    skipped: int  # records of other versions (V4 range records …), not returned

    @property
    def rows(self) -> int:
        return len(self.columns["usn"])


def parse_read_buffer(buf: bytes | bytearray | memoryview, limit: int | None = None) -> UsnChunk:
    """Parse the output of ``FSCTL_READ_USN_JOURNAL``: a next USN, then V2/V3 records.

    At most ``limit`` records are returned; ``next_usn`` is then the USN of the first record left
    out, so reading again from it continues without a gap. Names are UTF-16 in the records and
    UTF-8 (surrogatepass: lone surrogates survive) in the blob. Malformed buffers raise ValueError.
    """
    view = memoryview(buf).cast("B")
    if len(view) < _NEXT_USN.size:
        raise ValueError(tr("USN 读取结果只有 {size} 字节", size=len(view)))
    (next_usn,) = _NEXT_USN.unpack_from(view, 0)
    offsets, majors, skipped, stop = _walk(view, _NEXT_USN.size, limit)
    u8 = np.frombuffer(view, dtype=np.uint8)
    n = len(offsets)
    off = np.asarray(offsets, dtype=np.int64)
    major = np.asarray(majors, dtype=np.uint16)
    out = {key: np.zeros(n, dtype=dtype) for key, dtype in COLUMNS.items()}
    name_start = np.zeros(n, dtype=np.int64)
    name_bytes = np.zeros(n, dtype=np.int64)
    for version, layout in _LAYOUTS.items():
        sel = np.flatnonzero(major == version)
        if not len(sel):
            continue
        rec = u8[off[sel, None] + np.arange(layout.itemsize)].view(layout).reshape(len(sel))
        length = rec["length"].astype(np.int64)
        start = rec["name_off"].astype(np.int64)
        size = rec["name_len"].astype(np.int64)
        bad = (start < layout.itemsize) | (start + size > length) | (size % 2 != 0)
        if bad.any():
            first = int(sel[np.argmax(bad)])
            raise ValueError(tr("第 {index} 条 USN 记录的文件名越界或长度为奇数", index=first + 1))
        out["frn"][sel] = rec["frn"]
        out["parent_frn"][sel] = rec["parent"]
        out["usn"][sel] = rec["usn"]
        out["reason"][sel] = rec["reason"]
        out["attrs"][sel] = rec["attrs"]
        name_start[sel] = off[sel] + start
        name_bytes[sel] = size
    names, lens = _utf8_names(u8, name_start, name_bytes // 2)
    if n and int(lens.max()) > 0xFFFF:
        raise ValueError(_("USN 记录的文件名过长"))
    out["name_len"][:] = lens
    if n:
        np.cumsum(lens[:-1], out=out["name_off"][1:], dtype=np.uint32)
    if stop is not None:  # limit reached: continue at the first record not returned
        at = stop[0] + _USN_AT[stop[1]]
        next_usn = int(u8[at : at + 8].view("<i8")[0])
    return UsnChunk(next_usn, out, names, stop is not None, skipped)


def _walk(
    view: memoryview, pos: int, limit: int | None
) -> tuple[list[int], list[int], int, tuple[int, int] | None]:
    """Follow the RecordLength chain. Returns (offsets, versions) of up to ``limit`` V2/V3
    records, the number of other records skipped, and (offset, version) of the first V2/V3
    record beyond ``limit`` (None when the buffer ended first)."""
    offsets: list[int] = []
    majors: list[int] = []
    skipped = 0
    end = len(view)
    unpack = _HEAD.unpack_from
    while pos < end:
        if end - pos < 8:
            raise ValueError(tr("USN 读取结果末尾有 {size} 字节不完整的记录", size=end - pos))
        length, major = unpack(view, pos)
        layout = _LAYOUTS.get(major)
        if length < (layout.itemsize if layout is not None else 8) or length > end - pos:
            raise ValueError(
                tr(
                    "USN 记录长度 {length} 无效（偏移 {offset}，版本 {version}）",
                    length=length,
                    offset=pos,
                    version=major,
                )
            )
        if layout is None:
            skipped += 1
        elif limit is not None and len(offsets) >= limit:
            return offsets, majors, skipped, (pos, major)
        else:
            offsets.append(pos)
            majors.append(major)
        pos += length
    return offsets, majors, skipped, None


def _ranges(starts: np.ndarray, lens: np.ndarray) -> np.ndarray:
    """Indices ``starts[i] .. starts[i] + lens[i]`` of every range, concatenated."""
    total = int(lens.sum())
    if not total:
        return np.zeros(0, dtype=np.int64)
    before = np.zeros(len(lens), dtype=np.int64)
    np.cumsum(lens[:-1], out=before[1:])
    return np.repeat(starts - before, lens) + np.arange(total, dtype=np.int64)


def _utf8_names(u8: np.ndarray, start: np.ndarray, units: np.ndarray) -> tuple[bytes, np.ndarray]:
    """UTF-16 names at byte offsets ``start`` → one UTF-8 blob (surrogatepass) and the lengths.

    One decode for all names: each is followed by a NUL unit, so surrogates never pair across
    names, and the UTF-8 width of every unit (pairs count 4) gives the boundaries.
    """
    if not len(units):
        return b"", np.zeros(0, dtype=np.int64)
    raw = u8[_ranges(start, units * 2)]  # gathered: 2-byte aligned whatever the buffer was
    stride = units + 1
    dest = np.cumsum(stride) - stride
    stream = np.zeros(int(stride.sum()), dtype=np.uint16)
    stream[_ranges(dest, units)] = raw.view("<u2")
    enc = np.frombuffer(
        stream.tobytes().decode("utf-16-le", "surrogatepass").encode("utf-8", "surrogatepass"), np.uint8
    )
    hi = (stream >= 0xD800) & (stream < 0xDC00)
    lo = (stream >= 0xDC00) & (stream < 0xE000)
    pair = np.zeros(len(stream), dtype=bool)
    pair[:-1] = hi[:-1] & lo[1:]
    width = np.where(stream < 0x80, 1, np.where(stream < 0x800, 2, 3)).astype(np.int64)
    width[pair] = 4
    width[1:][pair[:-1]] = 0
    cs = np.concatenate(([0], np.cumsum(width)))
    if int(cs[-1]) != len(enc):
        raise ValueError(_("USN 记录的文件名无法转换为 UTF-8"))
    first = cs[dest]
    lens = cs[dest + units] - first
    return enc[_ranges(first, lens)].tobytes(), lens


def join_chunks(chunks: Sequence[UsnChunk]) -> tuple[dict[str, np.ndarray], bytes]:
    """The chunks of one read as one set of columns and one names blob."""
    if not chunks:
        return {key: np.zeros(0, dtype=dtype) for key, dtype in COLUMNS.items()}, b""
    if len(chunks) == 1:
        return dict(chunks[0].columns), chunks[0].names
    if sum(len(chunk.names) for chunk in chunks) > _U32_MAX:
        raise ValueError(_("USN 记录的文件名总长超过 4 GiB"))
    columns = {key: np.concatenate([chunk.columns[key] for chunk in chunks]) for key in COLUMNS}
    base = np.repeat(
        np.cumsum([0] + [len(chunk.names) for chunk in chunks[:-1]]).astype(np.uint32),
        [chunk.rows for chunk in chunks],
    )
    columns["name_off"] += base
    return columns, b"".join(chunk.names for chunk in chunks)


# --------------------------------------------------------------------------- reading

Progress = Callable[[int, int], None]  # (journal bytes read, bytes to read)
_clock = time.perf_counter
_sleep = time.sleep


def buffer_size(max_rows: int) -> int:
    """Read buffer for at most ``max_rows`` records: small reads stay small."""
    return min(MAX_BUFFER, max(MIN_BUFFER, max_rows * _BYTES_PER_ROW))


def check_checkpoint(journal: Journal, start_usn: int, journal_id: int) -> None:
    """UsnError ``usn_gone`` unless the records from ``start_usn`` on are all still there."""
    if journal.journal_id != journal_id:
        raise UsnError(
            "usn_gone",
            tr(
                "USN 日志已重建（当前编号 {current:#x}，检查点属于 {checkpoint:#x}），检查点失效",
                current=journal.journal_id,
                checkpoint=journal_id,
            ),
        )
    if start_usn < max(journal.first_usn, journal.lowest_valid_usn):
        raise UsnError(
            "usn_gone",
            tr(
                "检查点 {start} 之后的记录已被覆盖（日志现从 {first} 开始）",
                start=start_usn,
                first=journal.first_usn,
            ),
        )
    if start_usn > journal.next_usn:
        raise UsnError(
            "usn_gone", tr("检查点 {start} 超出日志末尾 {end}", start=start_usn, end=journal.next_usn)
        )


def read_journal(
    device: UsnDevice,
    start_usn: int,
    journal_id: int,
    *,
    max_rows: int,
    duty: float = 1.0,
    progress: Progress | None = None,
) -> tuple[dict[str, Any], dict[str, int]]:
    """Every record from ``start_usn`` up to the journal's end as it was when the read began
    (records appended meanwhile come along with the last buffer), at most ``max_rows``.

    Returns (table, statistics). The table holds the :data:`COLUMNS`, ``names`` (bytes) and the
    scalars ``journal_id``, ``next_usn`` (the checkpoint to read from next time), ``rows`` and
    ``truncated`` (``max_rows`` was reached; read again from ``next_usn`` for the rest).
    ``duty`` in (0, 1]: the reader is busy that share of the time (I/O limit).
    """
    journal = device.query()
    check_checkpoint(journal, start_usn, journal_id)
    target = journal.next_usn
    size = buffer_size(max_rows)
    chunks: list[UsnChunk] = []
    rows = reads = bytes_read = skipped = 0
    pos = start_usn
    truncated = False
    while pos < target:
        if rows >= max_rows:
            truncated = True  # records between pos and target are left for the next read
            break
        t0 = _clock()
        try:
            buf = device.read(pos, journal_id, size)
        except UsnError as exc:
            raise _recheck(device, journal_id, exc) from exc
        busy = _clock() - t0
        reads += 1
        bytes_read += len(buf)
        chunk = parse_read_buffer(buf, max_rows - rows)
        if chunk.rows:
            chunks.append(chunk)
            rows += chunk.rows
        skipped += chunk.skipped
        if chunk.truncated:
            truncated = True
            pos = chunk.next_usn
            break
        if chunk.next_usn <= pos:
            break  # no progress: nothing more to read
        pos = chunk.next_usn
        if progress is not None:
            progress(min(pos, target) - start_usn, target - start_usn)
        if duty < 1.0 and pos < target:
            _sleep(busy * (1.0 - duty) / duty)
    columns, names = join_chunks(chunks)
    table: dict[str, Any] = {
        **columns,
        "names": names,
        "journal_id": journal_id,
        "next_usn": pos,
        "rows": rows,
        "truncated": truncated,
    }
    stats = {
        "first_usn": journal.first_usn,
        "start_usn": start_usn,
        "target_usn": target,
        "reads": reads,
        "bytes_read": bytes_read,
        "skipped": skipped,
    }
    return table, stats


def _recheck(device: UsnDevice, journal_id: int, exc: UsnError) -> UsnError:
    """A read failed: if the journal was recreated meanwhile, say so (``usn_gone``)."""
    if exc.code != "io_error":
        return exc
    try:
        current = device.query().journal_id
    except UsnError:
        return exc
    if current != journal_id:
        return UsnError("usn_gone", tr("读取期间 USN 日志被重建（{reason}）", reason=exc.message))
    return exc


def open_volume(volume: str) -> UsnDevice:
    """The change journal of the local volume ``X:`` (read-only; needs administrator rights)."""
    if sys.platform == "win32":  # (the class exists on Windows only: mypy checks this branch there)
        return _VolumeJournal(volume)
    raise UsnError("not_supported", _("USN 日志只支持 Windows NTFS 卷"))


# --------------------------------------------------------------------------- Win32

if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes

    _k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _k32.CreateFileW.restype = wintypes.HANDLE
    _k32.CreateFileW.argtypes = (
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    )
    _k32.DeviceIoControl.restype = wintypes.BOOL
    _k32.DeviceIoControl.argtypes = (
        wintypes.HANDLE,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    )
    _k32.CloseHandle.restype = wintypes.BOOL
    _k32.CloseHandle.argtypes = (wintypes.HANDLE,)

    _GENERIC_READ = 0x80000000
    _SHARE_READ_WRITE = 0x1 | 0x2
    _OPEN_EXISTING = 3
    _INVALID_HANDLE = ctypes.c_void_p(-1).value
    # USN_JOURNAL_DATA_V0 (56 bytes), V1 adds Min/MaxSupportedMajorVersion; V2 is 80 bytes
    _JOURNAL_DATA = struct.Struct("<QqqqqQQ")
    _JOURNAL_VERSIONS = struct.Struct("<HH")
    _JOURNAL_BUFFER = 80
    # READ_USN_JOURNAL_DATA_V1: StartUsn, ReasonMask, ReturnOnlyOnClose, Timeout, BytesToWaitFor,
    # UsnJournalID, MinMajorVersion, MaxMajorVersion (sizeof 48)
    _READ_DATA = struct.Struct("<qIIQQQHH4x")

    def _fail(what: str) -> UsnError:
        err = ctypes.get_last_error()
        reason = ctypes.FormatError(err).strip()
        return UsnError(
            error_code(err), tr("{what}失败：[WinError {code}] {reason}", what=what, code=err, reason=reason)
        )

    class _VolumeJournal:
        """``\\\\.\\X:`` opened for reading; the journal FSCTLs on it."""

        def __init__(self, volume: str) -> None:
            self._volume = volume.upper()
            handle = _k32.CreateFileW(
                "\\\\.\\" + self._volume, _GENERIC_READ, _SHARE_READ_WRITE, None, _OPEN_EXISTING, 0, None
            )
            if handle is None or handle == _INVALID_HANDLE:
                raise _fail(tr("打开卷 {volume}", volume=self._volume))
            self._handle: int | None = int(handle)
            self._max_major = 2
            self._out = ctypes.create_string_buffer(0)

        def _ioctl(self, code: int, data: bytes | None, out: ctypes.Array[ctypes.c_char], what: str) -> int:
            if self._handle is None:
                raise UsnError("io_error", tr("卷 {volume} 已关闭", volume=self._volume))
            got = wintypes.DWORD(0)
            ok = _k32.DeviceIoControl(
                self._handle, code, data, len(data or b""), out, len(out), ctypes.byref(got), None
            )
            if not ok:
                raise _fail(what)
            return int(got.value)

        def query(self) -> Journal:
            out = ctypes.create_string_buffer(_JOURNAL_BUFFER)
            what = tr("查询 {volume} 的 USN 日志", volume=self._volume)
            got = self._ioctl(FSCTL_QUERY_USN_JOURNAL, None, out, what)
            if got < _JOURNAL_DATA.size:
                raise UsnError("io_error", tr("USN 日志信息只有 {size} 字节", size=got))
            jid, first, nxt, lowest, max_usn, max_size, delta = _JOURNAL_DATA.unpack_from(out, 0)
            if got >= _JOURNAL_DATA.size + _JOURNAL_VERSIONS.size:
                _min_major, max_major = _JOURNAL_VERSIONS.unpack_from(out, _JOURNAL_DATA.size)
                self._max_major = max(2, min(3, int(max_major)))
            return Journal(jid, first, nxt, lowest, max_usn, max_size, delta, self._max_major)

        def read(self, start_usn: int, journal_id: int, size: int) -> bytes:
            if len(self._out) != size:
                self._out = ctypes.create_string_buffer(size)
            data = _READ_DATA.pack(start_usn, ALL_REASONS, 0, 0, 0, journal_id, 2, self._max_major)
            what = tr("读取 {volume} 的 USN 日志", volume=self._volume)
            got = self._ioctl(FSCTL_READ_USN_JOURNAL, data, self._out, what)
            return ctypes.string_at(self._out, got)

        def close(self) -> None:
            if self._handle is not None:
                _k32.CloseHandle(self._handle)
                self._handle = None


__all__ = [
    "COLUMNS",
    "Journal",
    "UsnChunk",
    "UsnDevice",
    "UsnError",
    "buffer_size",
    "check_checkpoint",
    "error_code",
    "join_chunks",
    "open_volume",
    "parse_read_buffer",
    "read_journal",
]

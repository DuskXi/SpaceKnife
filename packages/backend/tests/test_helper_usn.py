"""USN change journal through the helper (spaceknife.helper.usn; ``query_usn`` / ``read_usn``).

The parser runs on synthetic ``USN_RECORD_V2`` / ``V3`` buffers. The service operations run in
a real helper subprocess whose volume access is replaced by an in-memory journal (``FAKE_USN``,
patched in before the helper starts): no volume is opened, the helper runs without
administrator rights (Safer token when this process is elevated) and only *claims* elevation.
Nothing here shows a UAC prompt. One test reads the real journal of the system volume; it needs
SPACEKNIFE_TEST_RAW_VOLUME=1 and administrator rights, and reads a few KiB at most.
"""

from __future__ import annotations

import os
import struct
import sys
import time
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

import numpy as np
import pytest
from pydantic import ValidationError

import spaceknife.helper.client as client_mod
from spaceknife.helper import HelperClient, HelperError, HelperUnavailable, is_elevated, usn
from spaceknife.helper.messages import REPLY, ReadUsn, Request, Response
from spaceknife.helper.service import OpError, usn_failure

IS_WINDOWS = sys.platform == "win32"
RAW_VOLUME = IS_WINDOWS and os.environ.get("SPACEKNIFE_TEST_RAW_VOLUME") == "1"

# --------------------------------------------------------------------------- synthetic journal

FAKE_USN = r"""
import struct
import time

from spaceknife.helper.usn import Journal, UsnError

TS = 133_700_000_000_000_000  # a FILETIME; the parser ignores it
FRN_HI = 0x5EED  # V3 ids: high half (ignored), must not leak into frn


def record(frn, parent, usn, name, *, version=2, reason=0x100, attrs=0x20, pad=0):
    data = name.encode("utf-16-le", "surrogatepass")
    if version == 4:  # a range-tracking record: no name; the parser skips it
        return struct.pack("<IHHQQQQq", 64, 4, 0, frn, 0, parent, 0, usn) + bytes(16)
    head = 60 if version == 2 else 76
    name_off = head + pad
    length = (name_off + len(data) + 7) & ~7
    tail = (TS, reason, 0, 0, attrs, len(data), name_off)  # TimeStamp … FileNameOffset
    if version == 2:
        fixed = struct.pack("<IHHQQqqIIIIHH", length, 2, 0, frn, parent, usn, *tail)
    else:
        fixed = struct.pack("<IHHQQQQqqIIIIHH", length, 3, 1, frn, FRN_HI, parent, FRN_HI, usn, *tail)
    body = fixed + b"\xee" * pad + data
    return body + b"\0" * (length - len(body))


def name_of(i):
    name = f"文件-{i}.txt"
    if i % 7 == 3:
        name = "\ud83d\ude00" + name  # a surrogate pair: one astral character
    if i % 11 == 5:
        name += "\udc80"  # lone low surrogate
    if i % 13 == 6:
        name = "\ud800" + name  # lone high surrogate
    return name


def spec_of(i):
    # (frn, parent, name, version): sequence numbers in the top 16 bits
    frn = 0x0001_0000_0000_0000 * (1 + i % 3) + 1000 + i
    parent = 0x0005_0000_0000_0000 + 5 + i % 17
    return frn, parent, name_of(i), 3 if i % 4 == 1 else 2


class FakeJournal:
    # An in-memory journal: USNs are byte offsets, as on NTFS. read() mimics FSCTL_READ_USN_JOURNAL.

    def __init__(self, count, *, journal_id=0xD1CE_1234_5678, first_usn=1 << 20, per_read=None, delay=0.0, v4=False):
        self.journal_id = journal_id
        self.first_usn = first_usn
        self.next_usn = first_usn
        self.records = []
        self.per_read = per_read
        self.delay = delay
        self.reads = []
        self.closed = False
        self.grow = 0
        self.fail_read = None
        self.recreate_on_read = False
        self.garbage = False
        for i in range(count):
            self.append(*spec_of(i))
            if v4 and i % 50 == 25:
                self.append(9, 9, "", 4)

    def append(self, frn, parent, name, version=2):
        rec = record(frn, parent, self.next_usn, name, version=version)
        self.records.append((self.next_usn, rec))
        self.next_usn += len(rec)

    def query(self):
        return Journal(self.journal_id, self.first_usn, self.next_usn, self.first_usn, 1 << 60, 32 << 20, 8 << 20, 3)

    def read(self, start_usn, journal_id, size):
        self.reads.append((start_usn, size))
        if self.delay:
            time.sleep(self.delay)
        if self.fail_read is not None:
            raise self.fail_read
        if self.recreate_on_read:
            self.journal_id += 1
            raise UsnError("io_error", "读取失败：[WinError 87] 参数错误。")
        if journal_id != self.journal_id:
            raise UsnError("io_error", "读取失败：[WinError 87] 参数错误。")
        if start_usn < self.first_usn:
            raise UsnError("usn_gone", "读取失败：[WinError 1181] 日志项已删除。")
        if self.garbage:
            return struct.pack("<q", self.next_usn) + struct.pack("<IHH", 4, 2, 0)
        out = []
        used = 8
        nxt = self.next_usn
        for pos, rec in self.records:
            if pos < start_usn:
                continue
            if used + len(rec) > size or (self.per_read is not None and len(out) >= self.per_read):
                nxt = pos
                break
            out.append(rec)
            used += len(rec)
        if not out and nxt != self.next_usn:
            raise UsnError("io_error", "读取失败：[WinError 122] 缓冲区太小。")
        for i in range(self.grow):  # the volume keeps changing while we read
            self.append(77, 5, f"new-{len(self.records)}-{i}")
        return struct.pack("<q", nxt) + b"".join(out)

    def close(self):
        self.closed = True


def journal_for(volume):
    if volume == "C:":
        return FakeJournal(3000, v4=True)
    if volume == "S:":  # slow: several reads with heartbeats in between
        return FakeJournal(1000, per_read=200, delay=0.3)
    if volume == "D:":
        raise UsnError("usn_inactive", "D: 没有启用 USN 日志")
    if volume == "E:":
        raise PermissionError(5, "拒绝访问。")
    journal = FakeJournal(100)
    if volume == "F:":
        journal.fail_read = OSError(23, "数据错误（循环冗余检查）。")
    elif volume == "G:":
        journal.garbage = True
    elif volume == "H:":
        journal.recreate_on_read = True
    return journal


def open_fake_volume(volume):
    return journal_for(volume)
"""

USN_BOOT = (
    FAKE_USN
    + r"""
import sys
import spaceknife.helper.__main__ as helper_main
import spaceknife.helper.service as service
import spaceknife.helper.usn as usn


def fake_check_ntfs(volume):
    if volume.upper() == "N:":
        raise service.OpError("not_ntfs", "N: 不是 NTFS 卷（FAT32）")
    return {"drive_type": 3, "fs_type": "NTFS"}


usn.open_volume = open_fake_volume
service._check_ntfs = fake_check_ntfs
helper_main.is_elevated = lambda: True  # claimed only: the process itself is unprivileged
sys.exit(helper_main.main(sys.argv[1:]))
"""
)
USN_COMMAND = [sys.executable, "-c", USN_BOOT]
_FAKE: dict[str, Any] = {}
exec(FAKE_USN, _FAKE)


def record(frn: int, parent: int, usn_: int, name: str, **kwargs: Any) -> bytes:
    data: bytes = _FAKE["record"](frn, parent, usn_, name, **kwargs)
    return data


def fake_journal(count: int, **kwargs: Any) -> Any:
    return _FAKE["FakeJournal"](count, **kwargs)


def canonical(name: str) -> str:
    """What a name reads back as: UTF-16 pairs become one astral character, lone surrogates stay."""
    return name.encode("utf-16-le", "surrogatepass").decode("utf-16-le", "surrogatepass")


def names_of(table: dict[str, Any] | usn.UsnChunk) -> list[str]:
    if isinstance(table, usn.UsnChunk):
        blob, off, length = table.names, table.columns["name_off"], table.columns["name_len"]
    else:
        blob, off, length = table["names"], table["name_off"], table["name_len"]
    raw = bytes(blob)
    return [
        raw[o : o + n].decode("utf-8", "surrogatepass")
        for o, n in zip(off.tolist(), length.tolist(), strict=True)
    ]


def buffer(next_usn: int, *records: bytes) -> bytes:
    return struct.pack("<q", next_usn) + b"".join(records)


# --------------------------------------------------------------------------- fixtures

if IS_WINDOWS:
    from spaceknife.helper import _win32

    from .test_helper import spawn_unprivileged


@pytest.fixture(autouse=True)
def no_uac(monkeypatch: pytest.MonkeyPatch) -> None:
    """A test that reaches the real ShellExecuteEx would show a UAC prompt: fail instead."""
    if IS_WINDOWS:

        def forbidden(params: object) -> object:
            raise AssertionError("tests must never call the real ShellExecuteExW")

        monkeypatch.setattr(_win32, "shell_execute", forbidden)


@pytest.fixture
def unprivileged(monkeypatch: pytest.MonkeyPatch) -> None:
    """Helpers started by HelperClient.launch(elevate=False) run without administrator rights."""
    if not is_elevated():
        return
    if not IS_WINDOWS:
        pytest.skip("running as root: cannot start an unprivileged helper")
    monkeypatch.setattr(client_mod, "start_subprocess", spawn_unprivileged)


@pytest.fixture
async def helper(unprivileged: None) -> AsyncIterator[HelperClient]:
    """A helper serving the in-memory journals of ``FAKE_USN`` (it claims to be elevated)."""
    client = await HelperClient.launch(elevate=False, command=USN_COMMAND)
    try:
        yield client
    finally:
        await client.close()


# --------------------------------------------------------------------------- parser


def test_parse_v2_records() -> None:
    names = ["a.txt", "目录", "x" * 255, "with space .tmp"]
    recs = []
    pos = 4096
    for i, name in enumerate(names):
        recs.append(record(100 + i, 5, pos, name, reason=0x80000100 + i, attrs=0x10 * i))
        pos += len(recs[-1])
    chunk = usn.parse_read_buffer(buffer(pos, *recs))
    assert chunk.rows == 4 and not chunk.truncated and chunk.skipped == 0
    assert chunk.next_usn == pos
    assert chunk.columns.keys() == usn.COLUMNS.keys()
    for key, dtype in usn.COLUMNS.items():
        assert chunk.columns[key].dtype == dtype, key
    assert chunk.columns["frn"].tolist() == [100, 101, 102, 103]
    assert chunk.columns["parent_frn"].tolist() == [5] * 4
    assert chunk.columns["usn"].tolist() == [4096 + sum(len(r) for r in recs[:i]) for i in range(4)]
    assert chunk.columns["reason"].tolist() == [0x80000100 + i for i in range(4)]
    assert chunk.columns["attrs"].tolist() == [0, 0x10, 0x20, 0x30]
    assert names_of(chunk) == names
    assert chunk.columns["name_off"][0] == 0 and len(chunk.names) == sum(len(n.encode()) for n in names)
    assert all(len(r) % 8 == 0 for r in recs)  # records are 8-byte aligned


def test_parse_v3_takes_the_low_64_bits() -> None:
    big = 0xFFFF_0000_0000_0010  # sequence in the top 16 bits, record number below
    recs = [
        record(big, 0x0003_0000_0000_0005, 64, "v3.bin", version=3),
        record(7, 5, 200, "v2.bin", version=2),
        record(9, 9, 300, "", version=4),  # range record: skipped
        record(8, 5, 400, "again", version=3),
    ]
    chunk = usn.parse_read_buffer(buffer(999, *recs))
    assert chunk.rows == 3 and chunk.skipped == 1
    assert chunk.columns["frn"].tolist() == [big, 7, 8]  # FRN_HI never shows up
    assert chunk.columns["parent_frn"].tolist() == [0x0003_0000_0000_0005, 5, 5]
    assert chunk.columns["usn"].tolist() == [64, 200, 400]
    assert names_of(chunk) == ["v3.bin", "v2.bin", "again"]


def test_parse_names_with_surrogates() -> None:
    names = [
        "\ud83d\ude00smile",  # pair → one character, 4 UTF-8 bytes
        "lone-high\ud800",  # ends with a high surrogate ...
        "\udc00lone-low",  # ... the next starts with a low one: they must not pair across names
        "\udfff",
        "中文名字",
        "ascii",
        "",
    ]
    recs = [record(i, 5, 8 * (i + 1), name, version=2 + i % 2) for i, name in enumerate(names)]
    chunk = usn.parse_read_buffer(buffer(0, *recs))
    assert names_of(chunk) == [canonical(n) for n in names]
    assert chunk.columns["name_len"].tolist() == [
        len(canonical(n).encode("utf-8", "surrogatepass")) for n in names
    ]
    assert chunk.columns["name_len"][0] == 4 + 5 and chunk.columns["name_len"][1] == 9 + 3


def test_parse_alignment_and_name_offsets() -> None:
    recs = [record(i, 5, 100 + i, f"n{i}" * (i + 1), pad=8 * (i % 3), version=2 + i % 2) for i in range(20)]
    data = buffer(12345, *recs)
    want = usn.parse_read_buffer(data)
    assert names_of(want) == [f"n{i}" * (i + 1) for i in range(20)]  # FileNameOffset is honoured
    for lead in (1, 3, 5):  # the buffer itself at an odd address
        raw = bytearray(lead) + data
        got = usn.parse_read_buffer(memoryview(raw)[lead:])
        for key in usn.COLUMNS:
            np.testing.assert_array_equal(got.columns[key], want.columns[key], err_msg=key)
        assert got.names == want.names and got.next_usn == 12345
        cut = usn.parse_read_buffer(memoryview(raw)[lead:], 5)  # next USN read at an odd address
        assert cut.truncated and cut.next_usn == int(want.columns["usn"][5]) == 105


def test_parse_limit_and_continuation() -> None:
    recs = [record(i, 5, 1000 + 128 * i, f"f{i}") for i in range(10)]
    data = buffer(5000, *recs)
    full = usn.parse_read_buffer(data)
    first = usn.parse_read_buffer(data, 4)
    assert first.rows == 4 and first.truncated
    assert first.next_usn == 1000 + 128 * 4  # the first record not returned
    assert names_of(first) == names_of(full)[:4]
    nothing = usn.parse_read_buffer(data, 0)
    assert nothing.rows == 0 and nothing.truncated and nothing.next_usn == 1000
    exact = usn.parse_read_buffer(data, 10)
    assert exact.rows == 10 and not exact.truncated and exact.next_usn == 5000
    empty = usn.parse_read_buffer(buffer(777))
    assert empty.rows == 0 and not empty.truncated and empty.next_usn == 777 and empty.names == b""
    assert all(
        col.dtype == dtype for col, dtype in zip(empty.columns.values(), usn.COLUMNS.values(), strict=True)
    )


def test_parse_rejects_malformed_buffers() -> None:
    good = record(1, 5, 64, "name.txt")
    zero = struct.pack("<IHH", 0, 2, 0) + bytes(56)
    bad_name = bytearray(good)
    struct.pack_into("<H", bad_name, 56, 200)  # FileNameLength past the record
    odd_name = bytearray(good)
    struct.pack_into("<H", odd_name, 56, 3)
    early_name = bytearray(good)
    struct.pack_into("<H", early_name, 58, 40)  # FileNameOffset inside the fixed part
    cases = [
        b"\0" * 7,  # shorter than the next USN
        buffer(0, zero),  # RecordLength 0 (would loop forever)
        buffer(0, good[:-8]),  # runs past the end of the buffer
        buffer(0, good, b"\0" * 4),  # a partial header at the end
        buffer(0, struct.pack("<IHH", 40, 2, 0) + bytes(32)),  # shorter than a V2 header
        buffer(0, bytes(bad_name)),
        buffer(0, bytes(odd_name)),
        buffer(0, bytes(early_name)),
    ]
    for data in cases:
        with pytest.raises(ValueError, match="USN"):
            usn.parse_read_buffer(data)


def test_parse_many_records_matches_a_reference() -> None:
    rng = np.random.default_rng(5)
    alphabet = [*"abcxyz.-_ 文件夹ü", "\ud83d\ude00", "\ud800", "\udc01"]
    names = ["".join(rng.choice(alphabet, size=int(rng.integers(1, 40)))) for _ in range(20_000)]
    recs = [record(i, i // 7, 64 * i, n, version=2 + i % 2) for i, n in enumerate(names)]
    data = buffer(1 << 40, *recs)
    t0 = time.perf_counter()
    chunk = usn.parse_read_buffer(data)
    elapsed = time.perf_counter() - t0
    print(f"\nUSN parse: {chunk.rows:,} records, {len(data) / 1e6:.1f} MB in {elapsed * 1e3:.0f} ms")
    assert names_of(chunk) == [canonical(n) for n in names]
    assert chunk.columns["parent_frn"].tolist() == [i // 7 for i in range(20_000)]


def test_join_chunks_rebases_names() -> None:
    a = usn.parse_read_buffer(buffer(0, *(record(i, 5, 8 * i, f"a{i}") for i in range(3))))
    b = usn.parse_read_buffer(buffer(0, *(record(i, 5, 8 * i, f"bb{i}") for i in range(4))))
    columns, names = usn.join_chunks([a, b])
    joined = {**columns, "names": names}
    assert names_of(joined) == ["a0", "a1", "a2", "bb0", "bb1", "bb2", "bb3"]
    assert columns["name_off"].dtype == np.uint32 and columns["frn"].tolist() == [0, 1, 2, 0, 1, 2, 3]
    columns, names = usn.join_chunks([])
    assert names == b"" and all(len(col) == 0 for col in columns.values())


# --------------------------------------------------------------------------- read loop (in-process)


def test_read_journal_whole_and_in_pieces() -> None:
    journal = fake_journal(500, v4=True)
    table, stats = usn.read_journal(journal, journal.first_usn, journal.journal_id, max_rows=10_000)
    assert table["rows"] == 500 and not table["truncated"]
    assert table["next_usn"] == journal.next_usn and table["journal_id"] == journal.journal_id
    assert names_of(table) == [canonical(_FAKE["name_of"](i)) for i in range(500)]
    assert stats["skipped"] == 10 and stats["reads"] == 1 and stats["bytes_read"] > 500 * 60
    # in pieces of 64: every piece continues exactly where the last one stopped
    seen: list[str] = []
    pos = journal.first_usn
    while True:
        part, _ = usn.read_journal(journal, pos, journal.journal_id, max_rows=64)
        seen += names_of(part)
        assert part["rows"] <= 64
        pos = part["next_usn"]
        if not part["truncated"]:
            break
    assert seen == names_of(table)
    assert pos == journal.next_usn
    sizes = {size for _, size in journal.reads[1:]}
    assert sizes == {usn.buffer_size(64)} and usn.buffer_size(64) <= 8 << 10  # small reads stay small


def test_read_journal_checkpoint_checks() -> None:
    journal = fake_journal(20)
    jid, first, end = journal.journal_id, journal.first_usn, journal.next_usn
    for start, ident in [(first, jid + 1), (first - 8, jid), (end + 8, jid)]:
        with pytest.raises(usn.UsnError) as err:
            usn.read_journal(journal, start, ident, max_rows=10)
        assert err.value.code == "usn_gone"
    assert journal.reads == []  # refused before reading anything
    table, stats = usn.read_journal(journal, end, jid, max_rows=10)  # up to date: nothing to read
    assert table["rows"] == 0 and table["next_usn"] == end and not table["truncated"]
    assert stats["reads"] == 0 and journal.reads == []


def test_read_journal_stops_at_the_end_seen_at_start() -> None:
    journal = fake_journal(300, per_read=40)
    end = journal.next_usn
    journal.grow = 5  # every read appends records: the loop must not chase them forever
    table, _ = usn.read_journal(journal, journal.first_usn, journal.journal_id, max_rows=100_000)
    assert table["rows"] >= 300 and table["next_usn"] >= end
    assert len(journal.reads) == 8  # 300 / 40 → 8 reads, then the end seen at the start is reached


def test_read_journal_duty_and_progress(monkeypatch: pytest.MonkeyPatch) -> None:
    now = [0.0]
    slept: list[float] = []
    journal = fake_journal(100, per_read=25)
    real_read = journal.read

    def timed_read(start: int, ident: int, size: int) -> bytes:
        now[0] += 0.01  # every read keeps the disk busy for 10 ms
        data: bytes = real_read(start, ident, size)
        return data

    journal.read = timed_read
    monkeypatch.setattr(usn, "_clock", lambda: now[0])
    monkeypatch.setattr(usn, "_sleep", slept.append)
    progress: list[tuple[int, int]] = []
    usn.read_journal(
        journal,
        journal.first_usn,
        journal.journal_id,
        max_rows=1000,
        duty=0.25,
        progress=lambda done, total: progress.append((done, total)),
    )
    assert slept == pytest.approx([0.03] * 3)  # busy 10 ms at 25 % → idle 30 ms; none after the last read
    total = journal.next_usn - journal.first_usn
    assert [t for _, t in progress] == [total] * 4
    assert [d for d, _ in progress] == sorted(d for d, _ in progress) and progress[-1][0] == total


def test_read_journal_errors() -> None:
    journal = fake_journal(10)
    journal.recreate_on_read = True  # the journal was recreated between query and read
    with pytest.raises(usn.UsnError) as err:
        usn.read_journal(journal, journal.first_usn, journal.journal_id, max_rows=10)
    assert err.value.code == "usn_gone" and "重建" in err.value.message
    journal = fake_journal(10)
    journal.fail_read = usn.UsnError("io_error", "设备未就绪")
    with pytest.raises(usn.UsnError) as err:
        usn.read_journal(journal, journal.first_usn, journal.journal_id, max_rows=10)
    assert err.value.code == "io_error"  # same journal: the read error stays an I/O error
    journal = fake_journal(10)
    journal.garbage = True
    with pytest.raises(ValueError, match="USN 记录长度"):
        usn.read_journal(journal, journal.first_usn, journal.journal_id, max_rows=10)


def test_error_code_mapping() -> None:
    assert usn.error_code(usn.ERROR_JOURNAL_NOT_ACTIVE) == "usn_inactive"
    assert usn.error_code(usn.ERROR_JOURNAL_DELETE_IN_PROGRESS) == "usn_inactive"
    assert usn.error_code(usn.ERROR_JOURNAL_ENTRY_DELETED) == "usn_gone"
    assert usn.error_code(usn.ERROR_ACCESS_DENIED) == "needs_admin"
    assert usn.error_code(usn.ERROR_PRIVILEGE_NOT_HELD) == "needs_admin"
    assert usn.error_code(usn.ERROR_INVALID_FUNCTION) == "not_supported"
    assert usn.error_code(usn.ERROR_NOT_SUPPORTED) == "not_supported"
    assert usn.error_code(usn.ERROR_INVALID_PARAMETER) == "io_error"
    assert usn.error_code(23) == "io_error"  # ERROR_CRC
    cases: list[tuple[BaseException, str]] = [
        (usn.UsnError("usn_gone", "gone"), "usn_gone"),
        (usn.UsnError("usn_inactive", "off"), "usn_inactive"),
        (PermissionError(13, "denied"), "needs_admin"),
        (NotImplementedError("no"), "not_supported"),
        (OSError(5, "io"), "io_error"),
        (ValueError("bad record"), "io_error"),
        (RuntimeError("bug"), "internal"),
        (OpError("not_ntfs", "fat"), "not_ntfs"),
    ]
    for exc, code in cases:
        assert usn_failure(exc).code == code, exc
    assert usn_failure(usn.UsnError("usn_gone", "检查点失效")).message == "检查点失效"
    if not IS_WINDOWS:
        with pytest.raises(usn.UsnError) as err:
            usn.open_volume("C:")
        assert err.value.code == "not_supported"


def test_usn_requests_are_strictly_validated() -> None:
    top = 2**64 - 1
    ok = Request.model_validate_json(
        f'{{"type":"request","id":1,"call":{{"op":"read_usn","volume":"c:","start_usn":0,"journal_id":{top}}}}}'
    )
    assert isinstance(ok.call, ReadUsn) and ok.call.journal_id == top and ok.call.max_rows == 1_000_000
    assert Request.model_validate_json(ok.model_dump_json()) == ok  # 64-bit ids survive JSON
    bad = [
        '"op":"query_usn","volume":"C:\\\\"',
        '"op":"query_usn","volume":"\\\\\\\\.\\\\C:"',
        '"op":"read_usn","volume":"C:","start_usn":-1,"journal_id":1',
        '"op":"read_usn","volume":"C:","start_usn":"5","journal_id":1',
        f'"op":"read_usn","volume":"C:","start_usn":0,"journal_id":{top + 1}',
        '"op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"max_rows":0',
        '"op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"max_rows":4000001',
        '"op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"duty":0',
        '"op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"duty":1.5',
        '"op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"path":"C:\\\\x"',
        '"op":"read_usn","volume":"C:","start_usn":0',
    ]
    for call in bad:
        with pytest.raises(ValidationError):
            Request.model_validate_json(f'{{"type":"request","id":1,"call":{{{call}}}}}')
    reply = Response(id=3, extras={"journal_id": top, "next_usn": 2**62, "rows": 0, "truncated": True})
    assert REPLY.validate_json(reply.model_dump_json()) == reply


# --------------------------------------------------------------------------- live helper (fake journal)


async def test_query_and_read_through_the_helper(helper: HelperClient) -> None:
    assert helper.elevated  # claimed by the patched helper
    want = fake_journal(3000, v4=True)
    info = await helper.query_usn("c:")
    assert info == {"volume": "C:", **want.query().scalars()}
    got = await helper.read_usn("C:", int(info["first_usn"] or 0), int(info["journal_id"] or 0))
    assert got["rows"] == 3000 and not got["truncated"]
    assert got["journal_id"] == want.journal_id and got["next_usn"] == want.next_usn
    for key, dtype in usn.COLUMNS.items():
        assert got[key].dtype == dtype and len(got[key]) == 3000, key
        assert got[key].flags.writeable
    assert isinstance(got["names"], bytes)
    expect = [_FAKE["spec_of"](i) for i in range(3000)]
    assert got["frn"].tolist() == [spec[0] for spec in expect]
    assert got["parent_frn"].tolist() == [spec[1] for spec in expect]
    assert names_of(got) == [canonical(spec[2]) for spec in expect]
    assert (np.diff(got["usn"]) > 0).all() and int(got["usn"][0]) == want.first_usn
    stats = helper.last_usn
    assert stats["rows"] == 3000 and stats["frames"] == 1 and stats["skipped"] == 60
    assert stats["reads"] == 1 and stats["volume"] == "C:"
    assert helper.last_scan == {}  # journal reads leave the scan statistics alone


async def test_read_in_pieces_through_the_helper(helper: HelperClient) -> None:
    info = await helper.query_usn("C:")
    pos, names = int(info["first_usn"] or 0), []
    pieces = 0
    while True:
        part = await helper.read_usn("C:", pos, int(info["journal_id"] or 0), max_rows=700, duty=0.9)
        pieces += 1
        assert part["rows"] == len(part["usn"]) <= 700
        names += names_of(part)
        pos = int(part["next_usn"])
        if not part["truncated"]:
            break
    assert pieces == 5 and len(names) == 3000 and pos == info["next_usn"]
    up_to_date = await helper.read_usn("C:", pos, int(info["journal_id"] or 0))
    assert up_to_date["rows"] == 0 and up_to_date["next_usn"] == pos and len(up_to_date["frn"]) == 0
    assert up_to_date["names"] == b"" and up_to_date["frn"].dtype == np.uint64


async def test_heartbeats_while_reading(helper: HelperClient) -> None:
    seen: list[tuple[str, float | None]] = []

    def on_progress(phase: str, _elapsed: float, fraction: float | None) -> None:
        seen.append((phase, fraction))

    info = await helper.query_usn("S:")
    got = await helper.read_usn(
        "S:", int(info["first_usn"] or 0), int(info["journal_id"] or 0), on_progress=on_progress
    )
    assert got["rows"] == 1000 and helper.last_usn["reads"] == 5
    fractions = [f for phase, f in seen if phase == "scan"]
    assert fractions and all(f is not None and 0 < f <= 1 for f in fractions)
    assert any(f is not None and f < 1 for f in fractions)  # a heartbeat in the middle of the read


async def test_errors_through_the_helper(helper: HelperClient) -> None:
    info = await helper.query_usn("C:")
    jid, first = int(info["journal_id"] or 0), int(info["first_usn"] or 0)
    cases: list[tuple[str, int, int, str]] = [
        ("C:", first, jid ^ 0xFF, "usn_gone"),  # journal recreated since the checkpoint
        ("C:", first - 64, jid, "usn_gone"),  # checkpoint's records overwritten
        ("D:", 0, 0, "usn_inactive"),
        ("E:", 0, 0, "needs_admin"),
        ("N:", 0, 0, "not_ntfs"),
    ]
    journal = fake_journal(100)
    for volume, start, ident, code in cases:
        with pytest.raises(HelperError) as err:
            await helper.read_usn(volume, start, ident)
        assert err.value.code == code, (volume, err.value.message)
        assert not isinstance(err.value, HelperUnavailable)
    for volume, code in [("F:", "io_error"), ("G:", "io_error"), ("H:", "usn_gone")]:
        with pytest.raises(HelperError) as err:
            await helper.read_usn(volume, journal.first_usn, journal.journal_id)
        assert err.value.code == code, (volume, err.value.message)
    for volume, code in [("D:", "usn_inactive"), ("E:", "needs_admin"), ("N:", "not_ntfs")]:
        with pytest.raises(HelperError) as err:
            await helper.query_usn(volume)
        assert err.value.code == code
    with pytest.raises(HelperError) as bad:
        await helper.read_usn("C:\\Windows", 0, 0)
    assert bad.value.code == "invalid_request"
    with pytest.raises(HelperError) as bad:
        await helper.read_usn("C:", -1, 0)
    assert bad.value.code == "invalid_request"
    with pytest.raises(HelperError) as bad:
        await helper.read_usn("C:", 0, 0, max_rows=0)
    assert bad.value.code == "invalid_request"
    assert helper.alive and (await helper.ping())["pid"] == helper.pid  # still usable


async def test_unelevated_helper_needs_admin(unprivileged: None) -> None:
    async with await HelperClient.launch(elevate=False) as client:  # the real helper, no patches
        assert not client.elevated
        for call in (client.query_usn("C:"), client.read_usn("C:", 0, 1)):
            with pytest.raises(HelperError) as err:
                await call
            assert err.value.code == "needs_admin"
        assert (await client.ping())["pid"] == client.pid


# --------------------------------------------------------------------------- real journal (opt-in)


@pytest.mark.skipif(
    not (RAW_VOLUME and is_elevated()),
    reason="reads a few KiB of the system volume's USN journal (set SPACEKNIFE_TEST_RAW_VOLUME=1, elevated)",
)
async def test_real_journal_of_system_volume(tmp_path: Path) -> None:
    volume = os.environ.get("SYSTEMDRIVE", "C:").upper()
    # this process is elevated, so a plain subprocess helper is too: no UAC prompt
    async with await HelperClient.launch(elevate=False) as client:
        assert client.elevated
        info = await client.query_usn(volume)
        jid, first, end = (
            int(info["journal_id"] or 0),
            int(info["first_usn"] or 0),
            int(info["next_usn"] or 0),
        )
        assert jid and 0 <= first <= end and int(info["max_size"] or 0) > 0
        probe = tmp_path / f"usn-probe-{os.getpid()}.txt"
        probe.write_text("x")
        ino, parent_ino = probe.stat().st_ino, tmp_path.stat().st_ino
        probe.unlink()
        got = await client.read_usn(volume, end, jid, max_rows=32, duty=0.5)
        stats = client.last_usn
        print(
            f"\nUSN {volume}: {got['rows']} records, {stats['reads']} reads, {stats['bytes_read']} bytes, {stats}"
        )
        assert got["rows"] == len(got["usn"]) <= 32 and got["journal_id"] == jid
        assert int(got["next_usn"]) >= end
        assert int(stats["bytes_read"] or 0) <= 64 << 10  # only a tiny amount was read
        assert (got["usn"] >= end).all() and (np.diff(got["usn"]) > 0).all()
        mine = [i for i, name in enumerate(names_of(got)) if name == probe.name]
        if not got["truncated"] and Path(tmp_path).drive.upper() == volume:
            assert mine, names_of(got)
        for i in mine:  # FRNs are the 64-bit file references the OS reports as st_ino
            assert int(got["frn"][i]) == ino and int(got["parent_frn"][i]) == parent_ino
        again = await client.read_usn(volume, int(got["next_usn"]), jid, max_rows=8)
        assert again["rows"] <= 8 and int(again["next_usn"]) >= int(got["next_usn"])
        with pytest.raises(HelperError) as err:
            await client.read_usn(volume, end, jid ^ 1)
        assert err.value.code == "usn_gone"
        if first >= 8:  # refused before reading: the records are gone
            with pytest.raises(HelperError) as err:
                await client.read_usn(volume, first - 8, jid)
            assert err.value.code == "usn_gone"

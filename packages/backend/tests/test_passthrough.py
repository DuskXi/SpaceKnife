"""A disk's own record read as root on Linux (spaceknife.disks.passthrough): the NVMe admin command
and the ATA PASS-THROUGH commands are built here and run against a fake kernel, which reads the
structures the way the kernel does and writes its answer into the caller's buffers."""

from __future__ import annotations

import ctypes
import os
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from spaceknife.disks import health
from spaceknife.disks import passthrough as pt

from .test_disks import nvme_log, qword

# ---------------------------------------------------------------- structures


def test_command_layouts() -> None:
    assert pt.NVME_CMD.size == 72  # struct nvme_passthru_cmd
    assert (3 << 30) | (72 << 16) | (ord("N") << 8) | 0x41 == pt.NVME_IOCTL_ADMIN_CMD  # _IOWR('N', 0x41, …)
    if ctypes.sizeof(ctypes.c_void_p) == 8:
        assert ctypes.sizeof(pt.SgIoHdr) == 88  # struct sg_io_hdr on 64-bit Linux
        assert pt.SgIoHdr.dxferp.offset == 16 and pt.SgIoHdr.usr_ptr.offset == 56
        assert pt.SgIoHdr.status.offset == 64 and pt.SgIoHdr.resid.offset == 72
    cmd = pt.NVME_CMD.unpack(bytes(pt.nvme_get_log(0x1234)))
    opcode, nsid, addr, data_len, cdw10 = cmd[0], cmd[3], cmd[7], cmd[9], cmd[10]
    assert (opcode, nsid, addr, data_len) == (0x02, 0xFFFFFFFF, 0x1234, 512)
    assert cdw10 == 0x007F0002  # 128 dwords (0-based), log 02h


def test_ata_commands() -> None:
    identify = pt.identify_device()
    assert identify == bytes([0x85, 0x08, 0x0E, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0xEC, 0])
    page1 = pt.read_log_ext(0x04, 0x01)  # READ LOG EXT, 48-bit: log address in LBA 7:0, page in 15:8
    assert page1 == bytes([0x85, 0x09, 0x0E, 0, 0, 0, 1, 0, 0x04, 0, 0x01, 0, 0, 0, 0x2F, 0])
    assert pt.read_log_ext(0x04, 0x05)[10] == 0x05


def test_disk_names() -> None:
    for good in ("nvme0n1", "nvme12n3", "sda", "sdab"):
        assert pt.DISK_RE.fullmatch(good)
    for bad in ("sda1", "nvme0", "nvme0n1p2", "../sda", "sda/../../etc/shadow", "loop0", "dm-0", ""):
        assert not pt.DISK_RE.fullmatch(bad)
        with pytest.raises(ValueError, match="不是一块磁盘"):
            pt.open_disk(bad)


# ---------------------------------------------------------------- a fake kernel


def nvme_kernel(log: bytes, status: int = 0) -> Callable[[int, int, Any], int]:
    def ioctl(fd: int, request: int, arg: Any) -> int:
        assert request == pt.NVME_IOCTL_ADMIN_CMD
        fields = pt.NVME_CMD.unpack(bytes(arg))
        addr, length = fields[7], fields[9]
        assert length == 512
        ctypes.memmove(addr, log, length)
        return status

    return ioctl


def stats_page(number: int, entries: dict[int, bytes]) -> bytes:
    page = bytearray(512)
    page[0:2] = (1).to_bytes(2, "little")  # revision
    page[2] = number
    for offset, value in entries.items():
        page[offset : offset + 8] = value
    return bytes(page)


def identify_words(sector_words: int | None = None) -> bytes:
    words = bytearray(512)
    words[0] = 0x40
    if sector_words:
        words[212:214] = (0x4000 | (1 << 12)).to_bytes(2, "little")
        words[234:238] = sector_words.to_bytes(4, "little")
    words[510] = 0xA5
    words[511] = (-sum(words)) % 256  # the checksum makes all 512 bytes sum to 0
    return bytes(words)


Reply = bytes | tuple[int, bytes]  # data, or (SCSI status, sense)


def sat_kernel(replies: dict[tuple[int, int], Reply]) -> Callable[[int, int, Any], int]:
    """Answers ATA PASS-THROUGH by (command, page)."""

    def ioctl(fd: int, request: int, hdr: Any) -> int:
        assert request == pt.SG_IO and isinstance(hdr, pt.SgIoHdr)
        assert hdr.interface_id == ord("S") and hdr.dxfer_direction == pt.SG_DXFER_FROM_DEV
        cdb = ctypes.string_at(hdr.cmdp, hdr.cmd_len)
        reply = replies.get((cdb[14], cdb[10]), (0x02, bytes([0x72, 0x05, 0x20, 0x00, 0, 0, 0, 0])))
        if isinstance(reply, bytes):
            ctypes.memmove(hdr.dxferp, reply, len(reply))
            hdr.status = 0
        else:
            hdr.status, sense = reply
            ctypes.memmove(hdr.sbp, sense, len(sense))
            hdr.sb_len_wr = len(sense)
            hdr.driver_status = 0x08
        return 0

    return ioctl


@pytest.fixture
def opener(tmp_path: Path) -> Callable[[str], int]:
    """A real descriptor for the code to close (a plain file standing in for /dev/<disk>)."""
    stand_in = tmp_path / "disk"
    stand_in.write_bytes(b"")
    return lambda name: os.open(stand_in, os.O_RDONLY)


def ata_error_sense(status: int = 0x51) -> bytes:
    """Descriptor-format sense: ABORTED COMMAND, ATA PASS THROUGH INFORMATION AVAILABLE, the ATA
    Status Return descriptor with ERR set."""
    descriptor = bytes([0x09, 0x0C, 0x01, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, status])
    return bytes([0x72, 0x0B, 0x00, 0x1D, 0, 0, 0, len(descriptor)]) + descriptor


# ---------------------------------------------------------------- reading


def test_nvme_record(opener: Callable[[str], int]) -> None:
    log = nvme_log(units_read=206_874_470, units_written=493_120_475, cycles=686, hours=8070)
    pages = pt.read_record("nvme0n1", nvme_kernel(log), opener)
    assert pages == {"kind": "nvme", "log": log.hex()}
    record = health.from_pages(pages)
    assert record is not None and record.written_bytes == 493_120_475 * 512_000
    with pytest.raises(pt.PassthroughError, match="0x2"):
        pt.read_record("nvme0n1", nvme_kernel(log, status=0x2), opener)
    with pytest.raises(pt.PassthroughError):
        pt.read_record("nvme0n1", nvme_kernel(bytes(512)), opener)


def test_ata_record(opener: Callable[[str], int]) -> None:
    general = stats_page(1, {0x08: qword(2859), 0x10: qword(30322), 0x18: qword(25_974_947_733)})
    temps = stats_page(5, {0x08: qword(31)})
    kernel = sat_kernel({(0xEC, 0): identify_words(2048), (0x2F, 1): general, (0x2F, 5): temps})
    pages = pt.read_record("sda", kernel, opener)
    assert pages == {
        "kind": "ata",
        "identify": identify_words(2048).hex(),
        "general": general.hex(),
        "temperature": temps.hex(),
    }
    record = health.from_pages(pages)
    assert record is not None and record.source == "ata"
    assert (record.power_cycles, record.power_on_hours, record.temperature_c) == (2859, 30322, 31)
    assert record.written_bytes == 25_974_947_733 * 4096  # 4 KiB logical sectors

    # no temperature page: the rest still comes back
    no_temp = sat_kernel(
        {(0xEC, 0): identify_words(), (0x2F, 1): general, (0x2F, 5): (0x02, ata_error_sense())}
    )
    pages = pt.read_record("sdb", no_temp, opener)
    assert pages["temperature"] == ""
    assert health.from_pages(pages).written_bytes == 25_974_947_733 * 512  # type: ignore[union-attr]


def test_ata_refusals(opener: Callable[[str], int]) -> None:
    general = stats_page(1, {0x10: qword(10)})
    # a bridge that doesn't take ATA PASS-THROUGH: ILLEGAL REQUEST, INVALID FIELD IN CDB
    with pytest.raises(pt.PassthroughError, match="ATA"):
        pt.read_record("sdc", sat_kernel({}), opener)
    # a disk without the Device Statistics log: READ LOG EXT aborted with ERR
    no_log = sat_kernel({(0xEC, 0): identify_words(), (0x2F, 1): (0x02, ata_error_sense())})
    with pytest.raises(pt.PassthroughError, match="拒绝"):
        pt.read_record("sdd", no_log, opener)
    # a page that isn't the one asked for
    wrong = sat_kernel({(0xEC, 0): identify_words(), (0x2F, 1): stats_page(3, {})})
    with pytest.raises(pt.PassthroughError):
        pt.read_record("sde", wrong, opener)
    # identify data that fails its checksum
    broken = bytearray(identify_words())
    broken[0] ^= 1
    with pytest.raises(pt.PassthroughError):
        pt.read_record("sdf", sat_kernel({(0xEC, 0): bytes(broken), (0x2F, 1): general}), opener)


def test_sense_decoding() -> None:
    hdr = pt.SgIoHdr()
    assert pt.sg_failure(hdr, b"") is None
    # ATA PASS THROUGH INFORMATION AVAILABLE without an error: done (some translators always say so)
    ok = bytes([0x72, 0x01, 0x00, 0x1D, 0, 0, 0, 14]) + bytes(
        [0x09, 0x0C, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x50]
    )
    hdr.status = 0x02
    assert pt.sg_failure(hdr, ok) is None
    assert pt.sg_failure(hdr, ata_error_sense()) is not None
    fixed = bytes([0x70, 0, 0x05, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0x24, 0x00])  # fixed format, ILLEGAL REQUEST
    assert "ATA" in (pt.sg_failure(hdr, fixed) or "")
    hdr.status, hdr.host_status = 0, 0x07
    assert "0x7" in (pt.sg_failure(hdr, b"") or "")
    hdr.host_status, hdr.driver_status = 0, 0x06  # DRIVER_TIMEOUT
    assert pt.sg_failure(hdr, b"") is not None


def test_short_transfer(opener: Callable[[str], int]) -> None:
    def short(fd: int, request: int, hdr: Any) -> int:
        hdr.resid = 256
        return 0

    with pytest.raises(pt.PassthroughError, match="256"):
        pt.read_record("sda", short, opener)


def test_from_pages_rejects_junk() -> None:
    assert health.from_pages({}) is None
    assert health.from_pages({"kind": "nvme", "log": "zz"}) is None
    assert health.from_pages({"kind": "ata", "general": ""}) is None
    assert health.from_pages({"kind": "scsi", "log": "00"}) is None

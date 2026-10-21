"""A disk's own health record on Linux, read as root (the elevated helper only, ``disk_health``).

- NVMe: the admin command Get Log Page, log 02h (SMART / Health Information, 512 bytes, for the
  whole controller), through ``NVME_IOCTL_ADMIN_CMD``.
- SATA, and SATA behind a USB bridge that translates ATA commands: IDENTIFY DEVICE (for the
  logical sector size) and READ LOG EXT of the Device Statistics log (04h), pages 01h (general:
  power-on hours, power cycles, sectors written and read) and 05h (temperature), each an ATA
  PASS-THROUGH (16) command through ``SG_IO``.

Only these fixed read-only commands are ever sent, on a block device opened read-only; the
caller names the disk, never the command. The structures are built here and ``ioctl`` is passed
in, so the tests run them against a fake kernel. The raw pages go back to the backend, which
parses them (:mod:`.health`).
"""

from __future__ import annotations

import ctypes
import os
import re
import stat
import struct
import sys
from collections.abc import Callable
from typing import Any

from ..i18n import _, tr

# _IOWR('N', 0x41, struct nvme_admin_cmd): the 72-byte command below
NVME_IOCTL_ADMIN_CMD = 0xC0484E41
NVME_ADMIN_GET_LOG_PAGE = 0x02
NVME_LOG_SMART = 0x02
NVME_ALL_NAMESPACES = 0xFFFFFFFF
# opcode, flags, rsvd1, nsid, cdw2, cdw3, metadata, addr, metadata_len, data_len, cdw10-15,
# timeout_ms, result (linux/nvme_ioctl.h struct nvme_passthru_cmd)
NVME_CMD = struct.Struct("<BBHIIIQQII6III")

SG_IO = 0x2285
SG_DXFER_FROM_DEV = -3
SG_INTERFACE_ID = ord("S")
ATA_PASS_THROUGH_16 = 0x85
ATA_PIO_DATA_IN = 4
ATA_IDENTIFY_DEVICE = 0xEC
ATA_READ_LOG_EXT = 0x2F
ATA_LOG_DEVICE_STATISTICS = 0x04
STATS_GENERAL = 0x01
STATS_TEMPERATURE = 0x05
SECTOR = 512
TIMEOUT_MS = 5000

# what the helper may open: a whole disk, by its kernel name
DISK_RE = re.compile(r"nvme\d{1,3}n\d{1,3}|sd[a-z]{1,4}")

Ioctl = Callable[[int, int, Any], int]


class PassthroughError(Exception):
    """The device did not answer the command (no pass-through, or the disk refused it)."""

    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.message = message


# --------------------------------------------------------------------------- NVMe


def nvme_get_log(addr: int, length: int = SECTOR, log_id: int = NVME_LOG_SMART) -> bytearray:
    """``struct nvme_admin_cmd`` for Get Log Page into the buffer at ``addr``."""
    numd = length // 4 - 1  # dwords, 0-based (NUMDL in cdw10 bits 31:16)
    cdw10 = (numd & 0xFFFF) << 16 | log_id
    return bytearray(
        NVME_CMD.pack(
            NVME_ADMIN_GET_LOG_PAGE, 0, 0, NVME_ALL_NAMESPACES, 0, 0, 0, addr, 0, length,
            cdw10, 0, 0, 0, 0, 0, TIMEOUT_MS, 0,
        )
    )  # fmt: skip


def read_nvme_log(fd: int, ioctl: Ioctl) -> bytes:
    buf = ctypes.create_string_buffer(SECTOR)
    cmd = nvme_get_log(ctypes.addressof(buf))
    status = ioctl(fd, NVME_IOCTL_ADMIN_CMD, cmd)
    if status:
        raise PassthroughError(tr("NVMe 控制器拒绝了读取健康日志（状态 0x{status:x}）", status=status))
    data = buf.raw
    if not any(data):
        raise PassthroughError(_("NVMe 控制器返回了空的健康日志"))
    return data


# --------------------------------------------------------------------------- ATA through SG_IO


class SgIoHdr(ctypes.Structure):
    """``struct sg_io_hdr`` (scsi/sg.h)."""

    _fields_ = [
        ("interface_id", ctypes.c_int),
        ("dxfer_direction", ctypes.c_int),
        ("cmd_len", ctypes.c_ubyte),
        ("mx_sb_len", ctypes.c_ubyte),
        ("iovec_count", ctypes.c_ushort),
        ("dxfer_len", ctypes.c_uint),
        ("dxferp", ctypes.c_void_p),
        ("cmdp", ctypes.c_void_p),
        ("sbp", ctypes.c_void_p),
        ("timeout", ctypes.c_uint),
        ("flags", ctypes.c_uint),
        ("pack_id", ctypes.c_int),
        ("usr_ptr", ctypes.c_void_p),
        ("status", ctypes.c_ubyte),
        ("masked_status", ctypes.c_ubyte),
        ("msg_status", ctypes.c_ubyte),
        ("sb_len_wr", ctypes.c_ubyte),
        ("host_status", ctypes.c_ushort),
        ("driver_status", ctypes.c_ushort),
        ("resid", ctypes.c_int),
        ("duration", ctypes.c_uint),
        ("info", ctypes.c_uint),
    ]


def ata_pass_through(command: int, *, lba_low: int = 0, lba_mid: int = 0, extend: bool = False) -> bytes:
    """ATA PASS-THROUGH (16), PIO data-in of one 512-byte block (SAT-3 12.2.2)."""
    cdb = bytearray(16)
    cdb[0] = ATA_PASS_THROUGH_16
    cdb[1] = ATA_PIO_DATA_IN << 1 | int(extend)
    cdb[2] = 0x0E  # T_DIR from the device, BYTE_BLOCK blocks, T_LENGTH in the count field
    cdb[6] = 1  # count (7:0)
    cdb[8] = lba_low  # LBA (7:0): the log address
    cdb[10] = lba_mid  # LBA (15:8): the page
    cdb[14] = command
    return bytes(cdb)


def identify_device() -> bytes:
    return ata_pass_through(ATA_IDENTIFY_DEVICE)


def read_log_ext(log: int, page: int) -> bytes:
    return ata_pass_through(ATA_READ_LOG_EXT, lba_low=log, lba_mid=page, extend=True)


def _ata_status(sense: bytes) -> int | None:
    """The ATA status register from the sense data, when the translator put it there."""
    if len(sense) < 8:
        return None
    if sense[0] & 0x7F in (0x72, 0x73):  # descriptor format: the ATA Status Return descriptor
        at, end = 8, min(len(sense), 8 + sense[7])
        while at + 1 < end:
            if sense[at] == 0x09 and at + 13 < len(sense):
                return sense[at + 13]
            at += 2 + sense[at + 1]
        return None
    if len(sense) >= 5:  # fixed format: INFORMATION holds error, status, device, count
        return sense[4]
    return None


def sg_failure(hdr: SgIoHdr, sense: bytes) -> str | None:
    """None when the command went through; else why it did not."""
    if hdr.host_status:
        return tr("主机适配器报告错误 0x{code:x}", code=hdr.host_status)
    if hdr.driver_status & 0x07:  # anything but DRIVER_SENSE
        return tr("驱动报告错误 0x{code:x}", code=hdr.driver_status)
    if hdr.status == 0:
        return None
    if hdr.status != 0x02 or len(sense) < 4:  # not CHECK CONDITION
        return tr("设备状态 0x{code:x}", code=hdr.status)
    descriptor = sense[0] & 0x7F in (0x72, 0x73)
    key = (sense[1] if descriptor else sense[2]) & 0x0F
    asc, ascq = (sense[2], sense[3]) if descriptor else (sense[12], sense[13]) if len(sense) > 13 else (0, 0)
    if key == 0x05:  # ILLEGAL REQUEST
        return _("设备不接受 ATA 直通命令（USB 桥接或控制器没有转发）")
    if (asc, ascq) == (0x00, 0x1D):  # ATA PASS THROUGH INFORMATION AVAILABLE
        ata = _ata_status(sense)
        if ata is not None and ata & 0x21:  # ERR or DF
            return _("磁盘拒绝了这条命令（没有这份日志）")
        if key in (0x00, 0x01):  # NO SENSE / RECOVERED ERROR: done
            return None
    return tr("设备报告 {key:x}/{asc:02x}/{ascq:02x}", key=key, asc=asc, ascq=ascq)


def ata_read(fd: int, ioctl: Ioctl, cdb: bytes) -> bytes:
    """One 512-byte PIO data-in command through SG_IO."""
    data = ctypes.create_string_buffer(SECTOR)
    cmd = ctypes.create_string_buffer(cdb, len(cdb))
    sense = ctypes.create_string_buffer(32)
    hdr = SgIoHdr(
        interface_id=SG_INTERFACE_ID,
        dxfer_direction=SG_DXFER_FROM_DEV,
        cmd_len=len(cdb),
        mx_sb_len=len(sense),
        dxfer_len=SECTOR,
        dxferp=ctypes.addressof(data),
        cmdp=ctypes.addressof(cmd),
        sbp=ctypes.addressof(sense),
        timeout=TIMEOUT_MS,
    )
    ioctl(fd, SG_IO, hdr)
    why = sg_failure(hdr, sense.raw[: hdr.sb_len_wr])
    if why is not None:
        raise PassthroughError(why)
    if hdr.resid > 0:
        raise PassthroughError(tr("只读回 {n} 字节", n=SECTOR - hdr.resid))
    return data.raw


def _identify_ok(words: bytes) -> bool:
    """Word 255: when its low byte is the signature A5h, all 512 bytes sum to 0 (mod 256)."""
    return words[510] != 0xA5 or sum(words) % 256 == 0


def read_ata(fd: int, ioctl: Ioctl) -> tuple[bytes, bytes, bytes | None]:
    """(IDENTIFY DEVICE, Device Statistics page 01h, page 05h or None)."""
    identify = ata_read(fd, ioctl, identify_device())
    if not any(identify) or not _identify_ok(identify):
        raise PassthroughError(_("磁盘返回的识别信息无效"))
    general = ata_read(fd, ioctl, read_log_ext(ATA_LOG_DEVICE_STATISTICS, STATS_GENERAL))
    if general[2] != STATS_GENERAL:  # the page header names its page
        raise PassthroughError(_("磁盘没有设备统计日志"))
    try:
        temperature: bytes | None = ata_read(
            fd, ioctl, read_log_ext(ATA_LOG_DEVICE_STATISTICS, STATS_TEMPERATURE)
        )
    except PassthroughError:
        temperature = None
    if temperature is not None and temperature[2] != STATS_TEMPERATURE:
        temperature = None
    return identify, general, temperature


# --------------------------------------------------------------------------- the disk


def open_disk(name: str) -> int:
    """``/dev/<name>``, read-only, refusing anything that is not a whole disk's block device."""
    if DISK_RE.fullmatch(name) is None or not os.path.isdir(f"/sys/block/{name}"):
        raise ValueError(tr("不是一块磁盘：{name}", name=name))
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_CLOEXEC", 0)
    fd = os.open(f"/dev/{name}", flags | getattr(os, "O_NOFOLLOW", 0))
    if not stat.S_ISBLK(os.fstat(fd).st_mode):
        os.close(fd)
        raise ValueError(tr("不是块设备：/dev/{name}", name=name))
    return fd


if sys.platform != "win32":
    import fcntl

    def system_ioctl(fd: int, request: int, arg: Any) -> int:
        return int(fcntl.ioctl(fd, request, arg))  # with a mutable buffer: the call's result code

else:

    def system_ioctl(fd: int, request: int, arg: Any) -> int:
        raise OSError(_("这里没有 ioctl"))


def read_record(
    name: str, ioctl: Ioctl = system_ioctl, opener: Callable[[str], int] = open_disk
) -> dict[str, str]:
    """The raw pages of disk ``name`` (hex): ``{kind: nvme, log}`` or ``{kind: ata, identify,
    general, temperature}`` (temperature "" when the disk has no such page).

    Raises PassthroughError when the device does not answer, ValueError for a bad name and
    OSError when it cannot be opened."""
    fd = opener(name)
    try:
        if name.startswith("nvme"):
            return {"kind": "nvme", "log": read_nvme_log(fd, ioctl).hex()}
        identify, general, temperature = read_ata(fd, ioctl)
        return {
            "kind": "ata",
            "identify": identify.hex(),
            "general": general.hex(),
            "temperature": temperature.hex() if temperature else "",
        }
    finally:
        os.close(fd)


__all__ = [
    "DISK_RE",
    "NVME_IOCTL_ADMIN_CMD",
    "SG_IO",
    "PassthroughError",
    "SgIoHdr",
    "ata_pass_through",
    "identify_device",
    "nvme_get_log",
    "open_disk",
    "read_ata",
    "read_log_ext",
    "read_nvme_log",
    "read_record",
    "sg_failure",
    "system_ioctl",
]

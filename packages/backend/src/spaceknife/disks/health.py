"""What a disk records about itself, and the system's read / write counters (pure parsers, tested).

- NVMe SMART / Health Information log (log page 02h, 512 bytes, NVMe base specification): critical
  warning, composite temperature (K), available spare and its threshold, percentage used, data
  units read / written (each 1000 × 512 bytes), power cycles, power-on hours, unsafe shutdowns,
  media and data integrity errors.
- ATA Device Statistics log (log address 04h, ACS-3): page 01h General Statistics (power-on
  resets, power-on hours, logical sectors written / read) and page 05h Temperature Statistics
  (current temperature). Each statistic is a QWORD: bit 63 supported, bit 62 valid, value in the low
  bits.
- Linux ``/sys/block/<disk>/stat``: sectors read (field 3) and written (field 7), 512 bytes each.
- The raw pages the elevated helper reads on Linux (:mod:`.passthrough`), as hex.
"""

from __future__ import annotations

import sys
import time
from collections.abc import Mapping
from datetime import UTC, datetime

from spaceknife_protocol.machine import DiskHealth

NVME_DATA_UNIT = 1000 * 512


def _le(b: bytes, at: int, n: int) -> int:
    return int.from_bytes(b[at : at + n], "little")


def nvme_health(log: bytes) -> DiskHealth | None:
    """The NVMe SMART / Health log page; None for a short or empty answer."""
    if len(log) < 192 or not any(log[:192]):
        return None
    kelvin = _le(log, 1, 2)
    return DiskHealth(
        source="nvme",
        critical_warning=log[0],
        temperature_c=kelvin - 273 if 200 < kelvin < 500 else None,
        available_spare=log[3],
        spare_threshold=log[4],
        percentage_used=log[5],
        read_bytes=_le(log, 32, 16) * NVME_DATA_UNIT,
        written_bytes=_le(log, 48, 16) * NVME_DATA_UNIT,
        power_cycles=_le(log, 112, 16),
        power_on_hours=_le(log, 128, 16),
        unsafe_shutdowns=_le(log, 144, 16),
        media_errors=_le(log, 160, 16),
    )


def _stat(page: bytes, offset: int, bits: int = 48) -> int | None:
    """A Device Statistics QWORD's value, when supported and valid."""
    if len(page) < offset + 8:
        return None
    q = _le(page, offset, 8)
    if not (q >> 63) & 1 or not (q >> 62) & 1:
        return None
    return q & ((1 << bits) - 1)


def ata_sector_size(identify: bytes) -> int:
    """The logical sector size from IDENTIFY DEVICE (words 106 and 117-118); 512 when not given."""
    if len(identify) < 512:
        return 512
    w106 = _le(identify, 212, 2)
    if (w106 & 0xC000) == 0x4000 and w106 & (1 << 12):
        words = _le(identify, 234, 2) | (_le(identify, 236, 2) << 16)
        if words >= 256:
            return words * 2
    return 512


def ata_statistics(general: bytes, temperature: bytes | None, sector: int = 512) -> DiskHealth | None:
    """Device Statistics pages 01h and 05h; None when the disk reports nothing of use."""
    hours, cycles = _stat(general, 0x10, 32), _stat(general, 0x08, 32)
    written, read = _stat(general, 0x18), _stat(general, 0x28)
    temp = _stat(temperature, 0x08, 8) if temperature else None
    if temp is not None and temp > 127:
        temp -= 256  # a signed byte
    if all(v is None for v in (hours, cycles, written, read, temp)):
        return None
    return DiskHealth(
        source="ata",
        power_on_hours=hours,
        power_cycles=cycles,
        written_bytes=written * sector if written is not None else None,
        read_bytes=read * sector if read is not None else None,
        temperature_c=temp,
    )


def _hex(pages: Mapping[str, object], key: str) -> bytes | None:
    value = pages.get(key)
    if not isinstance(value, str) or not value:
        return None
    try:
        return bytes.fromhex(value)
    except ValueError:
        return None


def from_pages(pages: Mapping[str, object]) -> DiskHealth | None:
    """The record from the helper's ``disk_health`` answer: ``{kind: nvme, log}`` or ``{kind: ata,
    identify, general, temperature}`` (hex). None when it holds nothing usable."""
    if pages.get("kind") == "nvme":
        log = _hex(pages, "log")
        return nvme_health(log) if log else None
    if pages.get("kind") == "ata":
        general = _hex(pages, "general")
        if not general:
            return None
        sector = ata_sector_size(_hex(pages, "identify") or b"")
        return ata_statistics(general, _hex(pages, "temperature"), sector)
    return None


def linux_stat(text: str) -> tuple[int, int] | None:
    """(bytes read, bytes written) from ``/sys/block/<disk>/stat``."""
    fields = text.split()
    if len(fields) < 7:
        return None
    try:
        return int(fields[2]) * 512, int(fields[6]) * 512
    except ValueError:
        return None


def boot_time() -> datetime | None:
    """When the system started: the counters of the system's I/O statistics start there."""
    try:
        if sys.platform == "win32":
            import ctypes

            k32 = ctypes.WinDLL("kernel32")  # type: ignore[attr-defined,unused-ignore]
            k32.GetTickCount64.restype = ctypes.c_uint64
            up = k32.GetTickCount64() / 1000
        elif sys.platform == "darwin":
            import subprocess

            cmd = ["sysctl", "-n", "kern.boottime"]
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=5).stdout
            sec = int(out.split("sec =")[1].split(",")[0])
            return datetime.fromtimestamp(sec, UTC)
        else:
            with open("/proc/uptime", encoding="ascii") as fh:
                up = float(fh.read().split()[0])
    except (OSError, ValueError, IndexError, AttributeError):
        return None
    return datetime.fromtimestamp(time.time() - up, UTC)


__all__ = ["ata_sector_size", "ata_statistics", "boot_time", "from_pages", "linux_stat", "nvme_health"]

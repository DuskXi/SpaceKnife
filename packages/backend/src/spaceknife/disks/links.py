"""Links of disks and their controllers, from what each system reports (pure functions, tested).

- SATA: ATA IDENTIFY DEVICE word 76 (the speeds the device supports, bits 1-3 = 1.5 / 3 / 6 Gb/s)
  and word 77 (bits 3:1, the negotiated speed; ACS-3); Linux's ``sata_spd`` ("6.0 Gbps").
- PCIe: Windows' DEVPKEY_PciDevice_*LinkSpeed codes (1 = 2.5 GT/s … 6 = 64 GT/s, that is the
  generation), Linux's ``current_link_speed`` ("16.0 GT/s PCIe") and macOS' "8.0 GT/s" / "x4".
- USB: Windows' connection speed code and the SuperSpeed flags of the hub's connection query,
  Linux's ``speed`` in Mb/s ("480", "5000") and ``version`` ("3.20").
"""

from __future__ import annotations

import re

from spaceknife_protocol.machine import DiskLink

SATA_MBPS = {1: 1500, 2: 3000, 3: 6000}
_GTS_GEN = {2.5: 1, 5.0: 2, 8.0: 3, 16.0: 4, 32.0: 5, 64.0: 6}


def sata_identify(word76: int, word77: int) -> DiskLink | None:
    """The SATA link from IDENTIFY DEVICE words 76 and 77 (None for a device that reports none)."""
    if word76 in (0, 0xFFFF):
        return None
    supported = [gen for gen in (1, 2, 3) if word76 & (1 << gen)]
    current = (word77 >> 1) & 0x7 if word77 not in (0, 0xFFFF) else 0
    rate, top = SATA_MBPS.get(current), SATA_MBPS.get(max(supported)) if supported else None
    if rate is None and top is None:
        return None
    return DiskLink(kind="sata", rate_mbps=rate, max_rate_mbps=top)


def gbps_text(text: str) -> int | None:
    """Mb/s from "6.0 Gbps" (Linux sata_spd), "3 Gigabit" (macOS) or "1.5 Gb/s"; None otherwise."""
    m = re.search(r"(\d+(?:\.\d+)?)\s*(?:Gbps|Gb/s|Gigabit|G)", text, re.IGNORECASE)
    return round(float(m.group(1)) * 1000) if m else None


def pcie_gen_code(code: int | None) -> int | None:
    """Windows' link speed code is the generation (1 = 2.5 GT/s … 6 = 64 GT/s)."""
    return code if code is not None and 1 <= code <= 6 else None


def pcie_gen_text(text: str) -> int | None:
    """The generation from "16.0 GT/s PCIe" (Linux) or "8.0 GT/s" (macOS); None for "Unknown"."""
    m = re.search(r"(\d+(?:\.\d+)?)\s*GT/s", text)
    return _GTS_GEN.get(float(m.group(1))) if m else None


def pcie_width_text(text: str) -> int | None:
    """Lanes from "4" (Linux) or "x4" (macOS)."""
    m = re.fullmatch(r"\s*[xX]?(\d{1,2})\s*", text)
    width = int(m.group(1)) if m else 0
    return width if width in (1, 2, 4, 8, 12, 16, 32) else None


def pcie(gen: int | None, width: int | None, max_gen: int | None, max_width: int | None) -> DiskLink | None:
    if gen is None and width is None:
        return None
    return DiskLink(
        kind="pcie", pcie_gen=gen, pcie_width=width, max_pcie_gen=max_gen, max_pcie_width=max_width
    )


# USB_CONNECTION_INFORMATION_EX_V2 flags (usbioctl.h)
OPERATING_SUPERSPEED, SUPERSPEED_CAPABLE = 0x1, 0x2
OPERATING_SUPERSPEED_PLUS, SUPERSPEED_PLUS_CAPABLE = 0x4, 0x8
_USB_SPEED_CODE = {0: 1, 1: 12, 2: 480, 3: 5000}  # UsbLowSpeed (1.5 Mb/s, shown as 1) … UsbSuperSpeed


def usb_windows(speed: int | None, flags: int | None) -> DiskLink | None:
    """The USB link from the hub's connection query: the speed code, and the V2 flags when known.

    SuperSpeedPlus is reported as 10 Gb/s (Windows doesn't tell 10 from 20 Gb/s here)."""
    f = flags or 0
    if f & OPERATING_SUPERSPEED_PLUS:
        rate: int | None = 10000
    elif f & OPERATING_SUPERSPEED:
        rate = 5000
    else:
        rate = _USB_SPEED_CODE.get(speed) if speed is not None else None
    top = 10000 if f & SUPERSPEED_PLUS_CAPABLE else 5000 if f & SUPERSPEED_CAPABLE else None
    if rate is None and top is None:
        return None
    return DiskLink(kind="usb", rate_mbps=rate, max_rate_mbps=top)


def usb_linux(speed: str, version: str) -> DiskLink | None:
    """The USB link from sysfs: ``speed`` in Mb/s; ``version`` says a USB 3 device that runs slower
    supports at least 5 Gb/s (the exact most is not in sysfs)."""
    try:
        rate = max(1, round(float(speed.strip())))
    except ValueError:
        return None
    try:
        spec = float(version.strip())
    except ValueError:
        spec = 0.0
    top = 5000 if spec >= 3.0 and rate < 5000 else None
    return DiskLink(kind="usb", rate_mbps=rate, max_rate_mbps=top)


_MAC_USB = {
    "low_speed": 1,
    "full_speed": 12,
    "high_speed": 480,
    "super_speed": 5000,
    "super_speed_plus": 10000,
    "super_speed_plus_by2": 20000,
}


def usb_mac(text: str) -> DiskLink | None:
    """macOS' "device_speed" ("high_speed", "super_speed") or link speed text ("Up to 5 Gb/s")."""
    key = text.strip().lower()
    rate = _MAC_USB.get(key)
    if rate is None:
        m = re.search(r"(\d+(?:\.\d+)?)\s*(Gb|Mb)/s", text)
        if m:
            rate = round(float(m.group(1)) * (1000 if m.group(2) == "Gb" else 1))
    return DiskLink(kind="usb", rate_mbps=rate) if rate else None


__all__ = [
    "gbps_text",
    "pcie",
    "pcie_gen_code",
    "pcie_gen_text",
    "pcie_width_text",
    "sata_identify",
    "usb_linux",
    "usb_mac",
    "usb_windows",
]

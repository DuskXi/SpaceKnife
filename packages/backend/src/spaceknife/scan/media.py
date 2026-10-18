"""Storage media of a volume: NVMe / SATA SSD, spinning disk, USB, network (UI-W1-16, UI-W2-37).

Read-only probes that need no elevation:

* Windows: ``IOCTL_STORAGE_QUERY_PROPERTY`` on ``\\\\.\\C:`` (bus type + seek penalty).
* Linux: ``/sys/class/block/<dev>`` (rotational flag, the device path tells NVMe / USB / MMC).
* macOS: ``diskutil info -plist`` (SolidState, BusProtocol).

Anything that cannot be determined is ``"unknown"``; callers never fail because of it.
"""

from __future__ import annotations

import os
import plistlib
import subprocess
import sys
from functools import lru_cache
from typing import Literal

Media = Literal[
    "nvme_ssd",
    "sata_ssd",
    "ssd",
    "hdd",
    "usb_hdd",
    "usb_ssd",
    "usb_flash",
    "sd_card",
    "network",
    "virtual",
    "unknown",
]

# STORAGE_BUS_TYPE (winioctl.h)
_BUS_USB, _BUS_SATA, _BUS_SD, _BUS_MMC, _BUS_VIRTUAL, _BUS_FILE_VIRTUAL, _BUS_NVME = 7, 11, 12, 13, 14, 15, 17


def classify(bus: str, rotational: bool | None, removable: bool = False) -> Media:
    """Combine a bus name ('nvme', 'sata', 'usb', 'sd', 'virtual', …) and the rotational flag."""
    if bus == "nvme":
        return "nvme_ssd"
    if bus == "usb":
        if rotational:
            return "usb_hdd"
        return "usb_flash" if removable else "usb_ssd"
    if bus == "sd":
        return "sd_card"
    if bus == "virtual":
        return "virtual"
    if rotational is True:
        return "hdd"
    if rotational is False:
        return "sata_ssd" if bus == "sata" else "ssd"
    return "unknown"


# --------------------------------------------------------------------------- Windows


def _win_media(root: str) -> Media:
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    k32.CreateFileW.restype = wintypes.HANDLE
    k32.CreateFileW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    ]
    k32.DeviceIoControl.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    ]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    drive = root.rstrip("\\")
    if len(drive) != 2 or drive[1] != ":":
        return "unknown"
    # access 0: the storage property queries work without read rights (no elevation)
    h = k32.CreateFileW(f"\\\\.\\{drive}", 0, 3, None, 3, 0, None)  # share read|write, OPEN_EXISTING
    if not h or h == wintypes.HANDLE(-1).value:
        return "unknown"
    try:

        def query(prop: int, size: int) -> bytes | None:
            q = (ctypes.c_uint32 * 3)(prop, 0, 0)  # STORAGE_PROPERTY_QUERY: id, PropertyStandardQuery
            out = ctypes.create_string_buffer(size)
            got = wintypes.DWORD(0)
            ok = k32.DeviceIoControl(h, 0x2D1400, q, 12, out, size, ctypes.byref(got), None)
            return out.raw[: got.value] if ok else None

        device = query(0, 1024)  # StorageDeviceProperty -> STORAGE_DEVICE_DESCRIPTOR
        seek = query(7, 12)  # StorageDeviceSeekPenaltyProperty -> DEVICE_SEEK_PENALTY_DESCRIPTOR
        bus_type = int.from_bytes(device[28:32], "little") if device and len(device) >= 32 else 0
        removable = bool(device[10]) if device and len(device) > 10 else False
        rotational = bool(seek[8]) if seek and len(seek) > 8 else None
    finally:
        k32.CloseHandle(h)
    bus = {
        _BUS_NVME: "nvme",
        _BUS_USB: "usb",
        _BUS_SATA: "sata",
        _BUS_SD: "sd",
        _BUS_MMC: "sd",
        _BUS_VIRTUAL: "virtual",
        _BUS_FILE_VIRTUAL: "virtual",
    }.get(bus_type, "other")
    return classify(bus, rotational, removable)


# --------------------------------------------------------------------------- Linux


def _linux_disk(dev: str) -> str | None:
    """sysfs directory of the whole disk behind a block device (partitions, dm/LVM resolved)."""
    name = os.path.basename(os.path.realpath(dev))
    path = os.path.realpath(f"/sys/class/block/{name}")
    if not os.path.isdir(path):
        return None
    slaves = os.path.join(path, "slaves")
    if os.path.isdir(slaves) and (inner := sorted(os.listdir(slaves))):
        return _linux_disk(f"/dev/{inner[0]}")  # device mapper / LVM / LUKS: the first backing device
    if os.path.exists(os.path.join(path, "partition")):
        path = os.path.dirname(path)
    return path


def _read(path: str) -> str:
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read().strip()
    except OSError:
        return ""


def _linux_media(device: str) -> Media:
    if not device.startswith("/dev/"):
        return "unknown"
    disk = _linux_disk(device)
    if disk is None:
        return "unknown"
    name = os.path.basename(disk)
    rot = _read(os.path.join(disk, "queue", "rotational"))
    rotational = None if rot == "" else rot == "1"
    removable = _read(os.path.join(disk, "removable")) == "1"
    if "/usb" in disk:
        bus = "usb"
    elif name.startswith("nvme"):
        bus = "nvme"
    elif name.startswith("mmcblk"):
        bus = "sd"
    elif name.startswith(("loop", "zram", "vd", "xvd")) or "/virtual/" in disk:
        bus = "virtual"
    elif name.startswith("sd") and os.path.isdir(os.path.join(disk, "device", "scsi_disk")):
        bus = "sata"
    else:
        bus = "other"
    return classify(bus, rotational, removable)


# --------------------------------------------------------------------------- macOS


def _darwin_media(mount_point: str) -> Media:
    try:
        raw = subprocess.run(
            ["diskutil", "info", "-plist", mount_point], capture_output=True, timeout=5, check=True
        ).stdout
        info = plistlib.loads(raw)
    except (OSError, subprocess.SubprocessError, plistlib.InvalidFileException, ValueError):
        return "unknown"
    protocol = str(info.get("BusProtocol", "")).lower()
    solid = info.get("SolidState")
    rotational = None if solid is None else not bool(solid)
    bus = (
        "usb"
        if "usb" in protocol
        else "nvme"
        if ("pci" in protocol or "fabric" in protocol) and solid
        else "sata"
        if "sata" in protocol
        else "other"
    )
    if info.get("VirtualOrPhysical") == "Virtual" and bus == "other":
        return "virtual" if rotational is None else classify("other", rotational)
    return classify(bus, rotational, bool(info.get("Ejectable", False)) and bus == "usb")


# --------------------------------------------------------------------------- public


@lru_cache(maxsize=64)
def media_of(mount_point: str, device: str, is_remote: bool) -> Media:
    """Media of a volume (cached: it does not change while the device stays connected)."""
    if is_remote:
        return "network"
    try:
        if sys.platform == "win32":
            return _win_media(mount_point)
        if sys.platform == "darwin":
            return _darwin_media(mount_point)
        return _linux_media(device)
    except Exception:  # a probe must never break the volume list
        return "unknown"


def system_mount() -> str:
    """Mount point of the system volume (selected by default on the start page, UI-W1-19)."""
    if sys.platform == "win32":
        return os.environ.get("SYSTEMDRIVE", "C:").rstrip("\\") + "\\"
    return "/"


__all__ = ["Media", "classify", "media_of", "system_mount"]

"""Volume → physical disk mapping (UI-W2-36: load is measured and limited per physical disk).

Read-only probes that need no elevation:

* Windows: ``IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`` on the volume device opened with access 0
  (falls back to ``IOCTL_STORAGE_GET_DEVICE_NUMBER``); the disk is ``PhysicalDrive<N>``.
* Linux: ``/sys/dev/block/<maj>:<min>`` → whole disk (partitions resolved to their parent, device
  mapper / LVM / LUKS / md to their first backing device via ``slaves/``).
* macOS: ``diskutil info -plist`` (APFS physical store → parent whole disk), best effort.

A :data:`DiskId` is a stable string: ``PhysicalDrive3`` · ``nvme0n1`` · ``disk0`` for physical disks,
``net:<remote>`` for network volumes and ``vol:<mount point>`` when the disk cannot be resolved
(both are governed open loop: there is nothing to sample).
"""

from __future__ import annotations

import os
import plistlib
import re
import struct
import subprocess
import sys
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from functools import lru_cache
from typing import Literal, NewType

from ..scan.media import Media, media_of
from ..scan.volume import volume_for

DiskId = NewType("DiskId", str)
DiskKind = Literal["physical", "network", "volume"]


@dataclass(frozen=True, slots=True)
class DiskRef:
    """A physical disk (or an unsampled stand-in) and what is known about it."""

    id: DiskId
    kind: DiskKind = "physical"
    number: int | None = None  # Windows disk number (\\.\PhysicalDriveN, PDH instance "N C:")
    name: str = ""  # kernel / BSD name on Linux and macOS (sysfs: /sys/block/<name>)
    media: Media = "unknown"
    mount: str = ""  # one volume on the disk, for display

    @property
    def sampled(self) -> bool:
        return self.kind == "physical"


def win_disk_id(number: int) -> DiskId:
    return DiskId(f"PhysicalDrive{number}")


# --------------------------------------------------------------------------- Windows


def _win_disk_numbers(device: str) -> list[int]:
    """Disk numbers behind a volume device (``\\\\.\\C:`` or ``\\\\?\\Volume{…}``)."""
    from . import _win

    raw = _win.device_ioctl(device, _win.IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, 8 + 24 * 4)
    if raw and len(raw) >= 4:
        count = struct.unpack_from("<I", raw)[0]
        # VOLUME_DISK_EXTENTS: count, pad, then DISK_EXTENT {DWORD disk, pad, i64 offset, i64 length}
        disks = [
            struct.unpack_from("<I", raw, 8 + 24 * i)[0] for i in range(count) if 8 + 24 * (i + 1) <= len(raw)
        ]
        if disks:
            return list(dict.fromkeys(disks))
    raw = _win.device_ioctl(device, _win.IOCTL_STORAGE_GET_DEVICE_NUMBER, 12, grow=False)
    if raw and len(raw) >= 12:
        dev_type, number, _part = struct.unpack_from("<III", raw)
        if dev_type == _win.FILE_DEVICE_DISK:
            return [number]
    return []


def _win_root(path: str) -> str:
    import ctypes

    from . import _win

    buf = ctypes.create_unicode_buffer(1024)
    if not _win.kernel32().GetVolumePathNameW(os.path.abspath(path), buf, 1024):
        raise OSError(f"no volume for {path}")
    root = buf.value
    return root if root.endswith("\\") else root + "\\"


def _win_volume_device(root: str) -> str:
    """Device path of a mounted volume: ``\\\\?\\Volume{GUID}`` (or ``\\\\.\\X:``)."""
    import ctypes

    from . import _win

    buf = ctypes.create_unicode_buffer(64)
    if _win.kernel32().GetVolumeNameForVolumeMountPointW(root, buf, 64) and buf.value:
        return buf.value.rstrip("\\")
    return "\\\\.\\" + root[:2]


def _win_remote(root: str) -> str:
    import ctypes
    from ctypes import wintypes

    mpr = ctypes.WinDLL("mpr")  # type: ignore[attr-defined,unused-ignore]
    buf = ctypes.create_unicode_buffer(1024)
    size = wintypes.DWORD(1024)
    if mpr.WNetGetConnectionW(ctypes.c_wchar_p(root.rstrip("\\")), buf, ctypes.byref(size)) == 0:
        return str(buf.value)
    return root.rstrip("\\")


@lru_cache(maxsize=256)
def _win_ref_for_root(root: str) -> DiskRef:
    from . import _win

    if root.startswith("\\\\") and not root.startswith("\\\\?\\"):
        return DiskRef(DiskId("net:" + root.rstrip("\\")), "network", media="network", mount=root)
    drive_type = int(_win.kernel32().GetDriveTypeW(root))
    if drive_type == 4:  # DRIVE_REMOTE
        return DiskRef(DiskId("net:" + _win_remote(root)), "network", media="network", mount=root)
    numbers = _win_disk_numbers(_win_volume_device(root))
    if not numbers:
        return DiskRef(DiskId("vol:" + root), "volume", mount=root)
    media = media_of(root, root.rstrip("\\"), False) if len(root) == 3 else "unknown"
    return DiskRef(win_disk_id(numbers[0]), number=numbers[0], media=media, mount=root)


def _win_ref(path: str) -> DiskRef:
    return _win_ref_for_root(_win_root(path))


def _win_local() -> list[DiskRef]:
    from . import _win

    mask = int(_win.kernel32().GetLogicalDrives())
    seen: dict[DiskId, DiskRef] = {}
    for i in range(26):
        if not mask & (1 << i):
            continue
        root = f"{chr(65 + i)}:\\"
        if int(_win.kernel32().GetDriveTypeW(root)) not in (2, 3):  # removable, fixed
            continue
        ref = _win_ref_for_root(root)
        if ref.sampled and ref.id not in seen:
            seen[ref.id] = ref
    return list(seen.values())


# --------------------------------------------------------------------------- Linux

_SYS = "/sys"
_OCTAL = re.compile(r"\\([0-7]{3})")


def linux_whole_disk(sys_path: str, root: str = _SYS) -> str | None:
    """Kernel name of the whole disk behind a sysfs block device directory."""
    path = os.path.realpath(sys_path)
    for _ in range(8):  # dm on dm on md …: bounded
        if not os.path.isdir(path):
            return None
        slaves = os.path.join(path, "slaves")
        inner = sorted(os.listdir(slaves)) if os.path.isdir(slaves) else []
        if inner:
            path = os.path.realpath(os.path.join(root, "class", "block", inner[0]))
            continue
        if os.path.exists(os.path.join(path, "partition")):
            path = os.path.dirname(path)
        return os.path.basename(path)
    return None


def _linux_name_for_dev(major: int, minor: int, root: str = _SYS) -> str | None:
    return linux_whole_disk(os.path.join(root, "dev", "block", f"{major}:{minor}"), root)


def _linux_ref(path: str) -> DiskRef:
    st = os.stat(path)
    major, minor = os.major(st.st_dev), os.minor(st.st_dev)  # type: ignore[attr-defined,unused-ignore]
    name = _linux_name_for_dev(major, minor) if major != 0 else None
    vol = None
    if name is None:  # btrfs / overlay / network: anonymous st_dev, go through the mount source
        vol = volume_for(path)
        if vol.is_remote:
            # mountinfo escapes the source like "C:\134" for "C:\"
            remote = _OCTAL.sub(lambda m: chr(int(m.group(1), 8)), vol.remote_path or vol.mount_point)
            return DiskRef(DiskId("net:" + remote), "network", media="network")
        if vol.device.startswith("/dev/"):
            dev = os.path.basename(os.path.realpath(vol.device))
            name = linux_whole_disk(os.path.join(_SYS, "class", "block", dev))
    if name is None:
        vol = vol or volume_for(path)
        return DiskRef(DiskId("vol:" + vol.mount_point), "volume", mount=vol.mount_point)
    return DiskRef(DiskId(name), name=name, media=media_of("", f"/dev/{name}", False))


def linux_physical_disks(root: str = _SYS) -> list[str]:
    """Whole disks worth sampling: ``/sys/block`` minus virtual devices (loop, ram, zram, dm, md …)."""
    base = os.path.join(root, "block")
    try:
        names = sorted(os.listdir(base))
    except OSError:
        return []
    out = []
    for name in names:
        real = os.path.realpath(os.path.join(base, name))
        if "/devices/virtual/" in real.replace("\\", "/"):
            continue
        out.append(name)
    return out


def _linux_local() -> list[DiskRef]:
    return [
        DiskRef(DiskId(n), name=n, media=media_of("", f"/dev/{n}", False)) for n in linux_physical_disks()
    ]


# --------------------------------------------------------------------------- macOS


def _diskutil(target: str) -> dict[str, object]:
    try:
        raw = subprocess.run(
            ["diskutil", "info", "-plist", target], capture_output=True, timeout=5, check=True
        ).stdout
        info = plistlib.loads(raw)
    except (OSError, subprocess.SubprocessError, plistlib.InvalidFileException, ValueError):
        return {}
    return info if isinstance(info, dict) else {}


_BSD_DISK = re.compile(r"^(disk\d+)")


def darwin_whole_disk(
    info: Mapping[str, object], lookup: Callable[[str], Mapping[str, object]] = _diskutil
) -> str | None:
    """BSD name of the physical whole disk from ``diskutil info -plist`` (APFS: via the physical store)."""
    get = lookup
    stores = info.get("APFSPhysicalStores")
    if isinstance(stores, list) and stores and isinstance(stores[0], dict):
        store = str(stores[0].get("APFSPhysicalStore", ""))
        if store:
            parent = get(store).get("ParentWholeDisk")
            if isinstance(parent, str) and parent:
                return parent
            if m := _BSD_DISK.match(store):
                return m.group(1)
    parent = info.get("ParentWholeDisk")
    if isinstance(parent, str) and parent:
        return parent
    ident = str(info.get("DeviceIdentifier", ""))
    m = _BSD_DISK.match(ident)
    return m.group(1) if m else None


@lru_cache(maxsize=64)
def _darwin_ref_for_mount(mount: str, remote: bool, remote_path: str) -> DiskRef:
    if remote:
        return DiskRef(DiskId("net:" + (remote_path or mount)), "network", media="network", mount=mount)
    name = darwin_whole_disk(_diskutil(mount))
    if name is None:
        return DiskRef(DiskId("vol:" + mount), "volume", mount=mount)
    return DiskRef(DiskId(name), name=name, media=media_of(mount, "", False), mount=mount)


def _darwin_ref(path: str) -> DiskRef:
    vol = volume_for(path)
    return _darwin_ref_for_mount(vol.mount_point, vol.is_remote, vol.remote_path)


def _darwin_local() -> list[DiskRef]:
    from ..scan.volume import list_volumes

    seen: dict[DiskId, DiskRef] = {}
    for vol in list_volumes():
        if vol.is_remote:
            continue
        ref = _darwin_ref_for_mount(vol.mount_point, False, "")
        if ref.sampled:
            seen.setdefault(ref.id, ref)
    return list(seen.values())


# --------------------------------------------------------------------------- public


def disk_ref_for_path(path: str) -> DiskRef:
    """The physical disk holding ``path``; never raises (falls back to a ``vol:`` stand-in)."""
    try:
        if sys.platform == "win32":
            return _win_ref(path)
        if sys.platform == "darwin":
            return _darwin_ref(path)
        return _linux_ref(path)
    except Exception:  # a probe must never break a scan
        return DiskRef(DiskId("vol:" + os.path.abspath(path)), "volume", mount=os.path.abspath(path))


def disk_for_path(path: str) -> DiskId:
    return disk_ref_for_path(path).id


def local_disks() -> list[DiskRef]:
    """Local physical disks (sampled from backend start so W2 has a pre-scan baseline)."""
    try:
        if sys.platform == "win32":
            return _win_local()
        if sys.platform == "darwin":
            return _darwin_local()
        return _linux_local()
    except Exception:
        return []


__all__ = [
    "DiskId",
    "DiskRef",
    "darwin_whole_disk",
    "disk_for_path",
    "disk_ref_for_path",
    "linux_physical_disks",
    "linux_whole_disk",
    "local_disks",
    "win_disk_id",
]

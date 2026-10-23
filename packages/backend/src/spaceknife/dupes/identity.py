"""Volumes as the duplicate-file engine sees them (docs/dupes.md §4).

- **In-run identity** of a file: (S1's ``dev``, 128-bit file id), compared exactly. Windows: the
  64-bit VolumeSerialNumber of FILE_ID_INFO; POSIX: ``st_dev`` (each btrfs subvolume has its own).
- **Cache key** of a volume (:func:`volume_key`), stable across reboots: Windows the 64-bit serial
  and the volume GUID; Linux the file system UUID (FS_IOC_GETFSUUID, 6.5+) and ``f_fsid`` (on btrfs
  it mixes in the subvolume); macOS ``f_fsid`` (not verified). A key that is not stable only makes
  the cache miss; joining two files from the cache still needs every stamp to match.
- **Trust** (:func:`trust_of`): whether a cached digest may *join* two files on a later run
  (strong) or only *separate* them (weak: FAT, exFAT, network and FUSE file systems, unknown ones,
  and the Python fallback on Windows, which has no change time).
- **Block sharing** (:func:`clonable`): whether two files can share data blocks there (ReFS and
  other volumes reporting FILE_SUPPORTS_BLOCK_REFCOUNTING, btrfs, xfs, bcachefs, ocfs2, APFS).
"""

from __future__ import annotations

import contextlib
import os
import sys
from dataclasses import dataclass, field
from typing import Literal

from ..scan.volume import NETWORK_FS, VolumeInfo

Trust = Literal["strong", "weak"]

FILE_SUPPORTS_BLOCK_REFCOUNTING = 0x0800_0000
STRONG_FS = {
    "ntfs",
    "refs",
    "btrfs",
    "ext2",
    "ext3",
    "ext4",
    "xfs",
    "f2fs",
    "bcachefs",
    "zfs",
    "apfs",
    "hfs",
    "hfsplus",
    "tmpfs",
    "jfs",
    "reiserfs",
    "ocfs2",
    "nilfs2",
}
CLONABLE_FS = {"refs", "btrfs", "xfs", "bcachefs", "ocfs2", "apfs"}
WEAK_FS = {"fat", "fat12", "fat16", "fat32", "vfat", "msdos", "exfat"}


@dataclass(slots=True)
class Volume:
    """One volume taking part in a job (an index in the candidate table's ``vol`` column)."""

    index: int
    dev: int  # S1's device / volume serial: the in-run identity
    key: bytes  # the cache file's key
    fs_type: str
    trust: Trust
    clonable: bool
    network: bool
    mount: str
    label: str = ""
    sample_path: str = ""  # a path on it (governor lookup, block maps)
    removable: bool = False
    notes: list[str] = field(default_factory=list)
    fs_group: bytes = b""  # volumes of one file system (btrfs subvolumes): S1b compares across them

    @property
    def name(self) -> str:
        """Short name for the user: the drive (``D:``) or the mount point."""
        m = self.mount.rstrip("\\/") if len(self.mount) > 1 else self.mount
        return m or self.mount


def fs_name(info: VolumeInfo | None) -> str:
    return (info.fs_type if info is not None else "").lower()


def is_network(info: VolumeInfo | None) -> bool:
    if info is None:
        return False
    fs = fs_name(info)
    return info.is_remote or fs in NETWORK_FS or fs.startswith("fuse")


def trust_of(info: VolumeInfo | None, *, native: bool) -> Trust:
    fs = fs_name(info)
    if info is None or is_network(info) or fs in WEAK_FS:
        return "weak"
    if sys.platform == "win32" and not native:
        return "weak"  # os.stat has no ChangeTime
    return "strong" if fs in STRONG_FS else "weak"


def clonable(info: VolumeInfo | None) -> bool:
    if info is None:
        return False
    if info.fs_flags & FILE_SUPPORTS_BLOCK_REFCOUNTING:
        return True
    return fs_name(info) in CLONABLE_FS


def fs_group(info: VolumeInfo | None, mount: str, dev: int) -> bytes:
    """What volumes of one file system share (block maps compare across them): the file system
    UUID where Linux gives it (FS_IOC_GETFSUUID, 6.5+), else the mounted device for btrfs (each
    subvolume has its own ``st_dev`` but the same source), else the device id itself."""
    if info is not None and fs_name(info) == "btrfs":
        uuid = _linux_uuid(mount)
        if uuid:
            return b"uuid:" + uuid
        if info.device:
            return b"src:" + info.device.encode("utf-8", "surrogateescape")
    return b"dev:" + dev.to_bytes(8, "little", signed=False)


def _win_guid(path: str) -> str:  # pragma: no cover - Windows
    import ctypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    root = ctypes.create_unicode_buffer(1024)
    if not k32.GetVolumePathNameW(ctypes.c_wchar_p(path), root, 1024):
        return ""
    name = ctypes.create_unicode_buffer(128)
    if not k32.GetVolumeNameForVolumeMountPointW(ctypes.c_wchar_p(root.value), name, 128):
        return ""
    return str(name.value)


def _linux_uuid(path: str) -> bytes:
    """The file system UUID (FS_IOC_GETFSUUID, Linux 6.5+), or b""."""
    if not sys.platform.startswith("linux"):
        return b""
    import fcntl

    fd = -1
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_DIRECTORY", 0))
        buf = bytearray(17)
        fcntl.ioctl(fd, 0x80111500, buf)
        n = min(buf[0], 16)
        return bytes(buf[1 : 1 + n])
    except OSError:
        return b""
    finally:
        if fd >= 0:
            with contextlib.suppress(OSError):
                os.close(fd)


def volume_key(mount: str, dev: int) -> bytes:
    """The cache key of the volume mounted at ``mount`` whose S1 device id is ``dev``."""
    if sys.platform == "win32":  # pragma: no cover - Windows
        return b"win:" + dev.to_bytes(8, "little") + _win_guid(mount).encode()
    fsid = 0
    with contextlib.suppress(OSError):
        fsid = int(os.statvfs(mount).f_fsid) & ((1 << 64) - 1)
    tag = b"mac:" if sys.platform == "darwin" else b"lnx:"
    return tag + fsid.to_bytes(8, "little") + _linux_uuid(mount)

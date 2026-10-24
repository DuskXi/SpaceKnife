"""What can be done where (docs/dupes.md §16.5.2): per volume, then per copy with its target.

A volume's capabilities come from its file system and, on Windows, its flags
(``FILE_SUPPORTS_HARD_LINKS``, ``FILE_SUPPORTS_REPARSE_POINTS``, ``FILE_SUPPORTS_BLOCK_REFCOUNTING``);
on Linux whether names can be swapped atomically is asked once per volume (a ``RENAME_EXCHANGE`` of
two names that do not exist: nothing is written). The recycle bin from ``ops.trash.trash_status``.
Each copy then gets, per action, either a target keeper (links, clones) or the reason it cannot be
done (``NA_*``): an action is offered only where it can be done, and why the others cannot is said.

Facts that cannot be established make the action unavailable or a risk, never safe (I7).
"""

from __future__ import annotations

import contextlib
import os
import sys
from dataclasses import dataclass
from typing import Any

from . import codes as K

HARDLINK_FS = frozenset(
    {
        "ntfs",
        "ext2",
        "ext3",
        "ext4",
        "xfs",
        "btrfs",
        "f2fs",
        "bcachefs",
        "zfs",
        "jfs",
        "reiserfs",
        "nilfs2",
        "tmpfs",
        "ocfs2",
        "apfs",
        "hfs",
        "hfsplus",
    }
)
SYMLINK_FS = HARDLINK_FS | {"refs", "overlay"}
NO_LINK_FS = frozenset({"fat", "fat12", "fat16", "fat32", "vfat", "msdos", "exfat"})
CLONE_FS_LINUX = frozenset({"btrfs", "xfs", "bcachefs", "ocfs2"})
LINK_MAX = {"ntfs": 1024, "ext4": 65000, "ext3": 32000, "ext2": 32000, "btrfs": 65535, "refs": 1024}
FILE_SUPPORTS_HARD_LINKS = 0x00400000
FILE_SUPPORTS_REPARSE_POINTS = 0x00000080
FILE_SUPPORTS_BLOCK_REFCOUNTING = 0x08000000


@dataclass(slots=True)
class VolCaps:
    index: int
    name: str
    mount: str
    fs: str
    dev: int
    fs_group: bytes
    network: bool
    removable: bool
    flags: int
    hardlink: bool
    symlink: bool
    clone: bool
    link_max: int
    trash: bool
    trash_reason: str
    exchange: bool  # names swap atomically (POSIX); Windows uses two no-replace renames instead
    media: str = "unknown"


def _flags_of(path: str) -> int:
    with contextlib.suppress(Exception):
        from ...scan.volume import volume_for

        return int(volume_for(path).fs_flags)
    return 0


def volume_caps(vol: Any, *, sys_facts: Any, trash_of: Any = None, probe_exchange: bool = True) -> VolCaps:
    """Capabilities of one of the job's volumes (``identity.Volume``)."""
    fs = (vol.fs_type or "").lower()
    win = sys.platform == "win32"
    flags = _flags_of(vol.sample_path or vol.mount) if win else 0
    mac = sys.platform == "darwin"
    if win:
        hard = bool(flags & FILE_SUPPORTS_HARD_LINKS) and fs not in NO_LINK_FS
        sym = bool(flags & FILE_SUPPORTS_REPARSE_POINTS) and fs not in NO_LINK_FS
        clone = bool(flags & FILE_SUPPORTS_BLOCK_REFCOUNTING) or (fs == "refs" and not flags)
    else:
        hard = fs in HARDLINK_FS
        sym = fs in SYMLINK_FS or (fs not in NO_LINK_FS and fs != "" and not vol.network)
        clone = fs in CLONE_FS_LINUX and not mac
        if clone and fs == "xfs":
            # made with reflink=0 the kernel refuses every share: say so in the plan already
            where = os.path.dirname(vol.sample_path) if vol.sample_path else vol.mount
            clone = xfs_reflink(where) is not False
    if vol.network:
        hard = sym = clone = False
    trash, why = True, ""
    if trash_of is not None:
        st = trash_of(vol.mount)
        trash, why = bool(st.available), str(st.reason)
    exchange = False
    if not win and probe_exchange and not vol.network:
        exchange = _exchange_here(os.path.dirname(vol.sample_path) if vol.sample_path else vol.mount)
    return VolCaps(
        index=vol.index,
        name=vol.name,
        mount=vol.mount,
        fs=fs,
        dev=vol.dev,
        fs_group=vol.fs_group,
        network=vol.network,
        removable=vol.removable,
        flags=flags,
        hardlink=hard,
        symlink=sym,
        clone=clone,
        link_max=LINK_MAX.get(fs, 65000 if not win else 1024),
        trash=trash,
        trash_reason=why,
        exchange=exchange,
    )


XFS_IOC_FSGEOMETRY = 0x8100587E  # _IOR('X', 126, struct xfs_fsop_geom): 256 bytes, Linux 5.1+
XFS_GEOM_FLAGS_AT = 92
XFS_FSOP_GEOM_FLAGS_REFLINK = 0x100000
_XFS_REFLINK: dict[str, bool | None] = {}


def xfs_reflink(folder: str) -> bool | None:
    """Whether the xfs holding ``folder`` was made with reflink (its geometry says), or None when
    it cannot be asked (an older kernel): sharing is then tried and the first refusal stops it."""
    if not sys.platform.startswith("linux"):
        return None
    if folder in _XFS_REFLINK:
        return _XFS_REFLINK[folder]
    import fcntl
    import struct

    got: bool | None = None
    try:
        fd = os.open(folder, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0))
    except OSError:
        return None
    try:
        buf = bytearray(256)
        fcntl.ioctl(fd, XFS_IOC_FSGEOMETRY, buf, True)
        got = bool(struct.unpack_from("<I", buf, XFS_GEOM_FLAGS_AT)[0] & XFS_FSOP_GEOM_FLAGS_REFLINK)
    except OSError:
        got = None
    finally:
        os.close(fd)
    _XFS_REFLINK[folder] = got
    return got


_EXCHANGE: dict[str, bool] = {}


def _exchange_here(folder: str) -> bool:
    hit = _EXCHANGE.get(folder)
    if hit is not None:
        return hit
    from .fsops import PosixFs, exchange_supported

    ok = False
    fs = PosixFs()
    with contextlib.suppress(Exception):
        fd = fs.open_dir(folder)
        try:
            ok = exchange_supported(fs, fd)
        finally:
            fs.close(fd)
    _EXCHANGE[folder] = ok
    return ok


@dataclass(slots=True)
class CopyFacts:
    """What availability needs of one copy (and the probe of it, when done)."""

    vol: int
    tags: int
    probe_ok: bool
    writable: bool
    uid: int
    immutable: bool
    nocow: bool
    nlink: int
    dir_writable: bool = True


def availability(
    copy: CopyFacts,
    keepers: list[tuple[int, CopyFacts]],
    vols: list[VolCaps],
    *,
    sys_facts: Any,
    stale: bool,
    planned_links: dict[int, int],
    no_trash_recovery: bool = True,
) -> tuple[list[int], list[int]]:
    """(reason code per action — NA_OK when it can be done —, target keeper row per action) for
    the actions leave, trash, hardlink, symlink, clone (index ``ACTION_CODE - 1``)."""
    na = [K.NA_OK] * 5
    tgt = [-1] * 5
    v = vols[copy.vol]
    mac = sys.platform == "darwin"
    win = sys.platform == "win32"

    def block(code: int, *which: int) -> None:
        for a in which or (2, 3, 4, 5):
            if na[a - 1] == K.NA_OK:
                na[a - 1] = code

    if stale:
        block(K.NA_STALE)
    if v.network or copy.tags & K.T_NETWORK:
        block(K.NA_NETWORK)
    if copy.tags & K.T_VCS_INTERNAL:
        block(K.NA_VCS_INTERNAL)
    if copy.tags & K.T_PACKAGE:
        block(K.NA_IN_PACKAGE)
    if copy.immutable:
        block(K.NA_IMMUTABLE)
    if copy.tags & K.T_IN_BASKET or any(kf.tags & K.T_IN_BASKET for _, kf in keepers):
        block(K.NA_IN_BASKET)
    if copy.probe_ok and not copy.dir_writable:
        block(K.NA_FOLDER_NOT_WRITABLE)
    # the recycle bin (or the recovery area where a volume has none)
    if not v.trash and not no_trash_recovery:
        block(K.NA_NO_TRASH, 2)
    # hard links: a keeper on the same volume (the same btrfs subvolume)
    same = [(r, kf) for r, kf in keepers if kf.vol == copy.vol]
    if not v.hardlink:
        block(K.NA_MACOS_OFF if mac and not sys_facts.macos_links else K.NA_FS_NO_HARDLINK, 3)
    elif mac and not sys_facts.macos_links:
        block(K.NA_MACOS_OFF, 3)
    elif not same:
        if any(vols[kf.vol].fs_group == v.fs_group for _, kf in keepers) and v.fs == "btrfs":
            block(K.NA_OTHER_SUBVOLUME, 3)
        else:
            block(K.NA_OTHER_VOLUME, 3)
    else:
        ok_same = [
            (r, kf)
            for r, kf in same
            if kf.nlink + planned_links.get(r, 0) < vols[kf.vol].link_max or not kf.nlink
        ]
        if not ok_same:
            block(K.NA_LINK_LIMIT, 3)
        elif sys_facts.protected_hardlinks and sys_facts.uid not in (-1, 0):
            mine = [(r, kf) for r, kf in ok_same if kf.uid in (-1, sys_facts.uid) or kf.writable]
            if not mine:
                block(K.NA_PROTECTED_HARDLINKS, 3)
            else:
                tgt[2] = mine[0][0]
        else:
            tgt[2] = ok_same[0][0]
    # symbolic links: any keeper
    if not v.symlink:
        block(K.NA_MACOS_OFF if mac and not sys_facts.macos_links else K.NA_FS_NO_SYMLINK, 4)
    elif mac and not sys_facts.macos_links:
        block(K.NA_MACOS_OFF, 4)
    elif win and not sys_facts.symlink_right:
        block(K.NA_NO_SYMLINK_RIGHT, 4)
    elif keepers:
        tgt[3] = keepers[0][0]
    else:
        block(K.NA_NO_KEEPER_HERE, 4)
    # replacements need atomic swaps (POSIX), the search's identities (Windows), no sync folder
    for a in (3, 4):
        if copy.tags & K.T_SYNC:
            block(K.NA_IN_SYNC_ROOT, a)
        if not win and not v.exchange:
            block(K.NA_NO_EXCHANGE, a)
        if win and not sys_facts.native:
            block(K.NA_NO_NATIVE, a)
    # block clones: in place, on one file system
    if mac:
        block(K.NA_MACOS_OFF, 5)
    elif not v.clone:
        block(K.NA_FS_NO_CLONE, 5)
    else:
        fit = [(r, kf) for r, kf in keepers if vols[kf.vol].fs_group == v.fs_group and vols[kf.vol].clone]
        fit.sort(key=lambda x: x[1].vol != copy.vol)
        if not fit:
            block(K.NA_OTHER_VOLUME, 5)
        elif win and not sys_facts.native:
            block(K.NA_NO_NATIVE, 5)
        elif win and copy.tags & K.T_READONLY:
            block(K.NA_READONLY_INPLACE, 5)
        elif copy.tags & K.T_ENCRYPTED or fit[0][1].tags & K.T_ENCRYPTED:
            block(K.NA_ENCRYPTED, 5)
        elif bool(copy.tags & K.T_SPARSE) != bool(fit[0][1].tags & K.T_SPARSE) and win:
            block(K.NA_SPARSE_MISMATCH, 5)
        elif copy.nocow or fit[0][1].nocow:
            block(K.NA_FS_NO_CLONE, 5)
        elif (
            not win
            and copy.probe_ok
            and not copy.writable
            and sys_facts.uid not in (-1, 0)
            and copy.uid not in (-1, sys_facts.uid)
        ):
            block(K.NA_NOT_OWNER, 5)
        else:
            tgt[4] = fit[0][0]
    return na, tgt

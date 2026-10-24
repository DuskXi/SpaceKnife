"""Volume information: filesystem type, capacity, free space, cluster size, remote/removable.

Free space is what the user can still write (``f_bavail`` / ``GetDiskFreeSpaceEx`` total free),
and ``used = total - free``, so for a whole-volume scan

    scanned allocation + unknown + free == total

where *unknown* is whatever the scan could not see (no permission, filesystem metadata,
snapshots, reserved blocks …).
"""

from __future__ import annotations

import functools
import os
import re
import sys
from collections.abc import Callable, Iterable, Sequence
from dataclasses import asdict, dataclass, replace
from typing import Any

from ..i18n import Texts

# User-facing text (M7.5): reasons a volume cannot be scanned, the WSL drive label and note.
TEXT = Texts(
    {
        "offline": "网络卷未连接",
        "no_media": "没有介质",
        "unreadable": "无法读取卷：{reason}",
        "wsl_label": "{drive}（Windows 盘）",
        "wsl_note": "WSL 经 9P 协议访问的 Windows 盘，扫描会非常慢；建议用 Windows 版的 SpaceKnife 扫描",
    }
)

NETWORK_FS = {
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "afpfs",
    "webdav",
    "davfs",
    "9p",
    "ceph",
    "glusterfs",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.gvfsd-fuse",
}
SUBVOLUME_FS = {"btrfs", "bcachefs"}  # subvolumes have device numbers of their own
PSEUDO_FS = {
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "cgroup",
    "cgroup2",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "configfs",
    "efivarfs",
    "autofs",
    "mqueue",
    "hugetlbfs",
    "fusectl",
    "binfmt_misc",
    "rpc_pipefs",
    "nsfs",
    "squashfs",
    "overlay",
    "ramfs",
}


@dataclass(frozen=True, slots=True)
class VolumeInfo:
    mount_point: str
    device: str
    fs_type: str
    label: str
    total: int
    free: int
    cluster_size: int
    is_remote: bool
    is_removable: bool
    serial: str = ""
    dev: int = 0  # st_dev on POSIX
    remote_path: str = ""  # network volumes: the server path (UNC share, host:/export)
    reason: str = ""  # cannot be scanned now (no media, disconnected, locked)
    note: str = ""  # can be scanned but is not recommended, and why (shown on the volume card)
    reserved: int = 0  # free only for the super-user (ext4 keeps 5 % by default): neither used nor free
    fs_flags: int = 0  # Windows: GetVolumeInformationW's file system flags (block cloning, ...)

    @property
    def used(self) -> int:
        return max(0, self.total - self.free - self.reserved)

    def to_dict(self) -> dict[str, object]:
        return {**asdict(self), "used": self.used}


def is_volume_root(path: str, volume: VolumeInfo) -> bool:
    norm = os.path.normcase(os.path.normpath(os.path.abspath(path)))
    return norm == os.path.normcase(os.path.normpath(volume.mount_point))


# --------------------------------------------------------------------------- Windows


def _win_volume(path: str) -> VolumeInfo:
    import ctypes
    from ctypes import wintypes

    assert sys.platform == "win32"
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    buf = ctypes.create_unicode_buffer(1024)
    if not k32.GetVolumePathNameW(ctypes.c_wchar_p(os.path.abspath(path)), buf, 1024):
        raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]
    root = buf.value
    if not root.endswith("\\"):
        root += "\\"
    label = ctypes.create_unicode_buffer(261)
    fsname = ctypes.create_unicode_buffer(261)
    serial = wintypes.DWORD(0)
    maxlen = wintypes.DWORD(0)
    fsflags = wintypes.DWORD(0)
    ok = k32.GetVolumeInformationW(
        ctypes.c_wchar_p(root),
        label,
        261,
        ctypes.byref(serial),
        ctypes.byref(maxlen),
        ctypes.byref(fsflags),
        fsname,
        261,
    )
    free_caller = ctypes.c_ulonglong(0)
    total = ctypes.c_ulonglong(0)
    total_free = ctypes.c_ulonglong(0)
    k32.GetDiskFreeSpaceExW(
        ctypes.c_wchar_p(root), ctypes.byref(free_caller), ctypes.byref(total), ctypes.byref(total_free)
    )
    spc = wintypes.DWORD(0)
    bps = wintypes.DWORD(0)
    nfree = wintypes.DWORD(0)
    nclusters = wintypes.DWORD(0)
    cluster = 4096
    if k32.GetDiskFreeSpaceW(
        ctypes.c_wchar_p(root),
        ctypes.byref(spc),
        ctypes.byref(bps),
        ctypes.byref(nfree),
        ctypes.byref(nclusters),
    ):
        cluster = int(spc.value * bps.value) or 4096
    drive_type = int(k32.GetDriveTypeW(ctypes.c_wchar_p(root)))
    return VolumeInfo(
        mount_point=root,
        device=root.rstrip("\\"),
        fs_type=fsname.value if ok else "",
        label=label.value if ok else "",
        total=int(total.value),
        free=int(total_free.value),
        cluster_size=cluster,
        is_remote=drive_type == 4,
        is_removable=drive_type in (2, 5),
        serial=f"{serial.value:08X}" if ok else "",
        fs_flags=int(fsflags.value) if ok else 0,
    )


def _win_list() -> list[VolumeInfo]:
    assert sys.platform == "win32"
    import ctypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    buf = ctypes.create_unicode_buffer(4096)
    n = k32.GetLogicalDriveStringsW(4096, buf)
    roots = [r for r in ctypes.wstring_at(buf, n).split("\0") if r]
    out = []
    for root in roots:
        drive_type = int(k32.GetDriveTypeW(ctypes.c_wchar_p(root)))
        if drive_type in (0, 1, 5):  # unknown, no root, CD-ROM
            continue
        remote = _win_remote_path(root) if drive_type == 4 else ""
        try:
            vol = _win_volume(root)
        except OSError as exc:
            # still listed, but disabled with the reason (UI-W1-22)
            reason = (
                TEXT["offline"]
                if drive_type == 4
                else TEXT["no_media"]
                if getattr(exc, "winerror", 0) == 21  # ERROR_NOT_READY
                else TEXT["unreadable"].format(reason=exc.strerror or exc)
            )
            vol = VolumeInfo(
                root, root.rstrip("\\"), "", "", 0, 0, 4096, drive_type == 4, drive_type == 2, reason=reason
            )
        if vol.total == 0 and not vol.reason:
            vol = replace(vol, reason=TEXT["no_media"])
        out.append(replace(vol, remote_path=remote) if remote else vol)
    return out


def _win_remote_path(root: str) -> str:
    """\\\\server\\share behind a mapped drive letter, or ''."""
    import ctypes
    from ctypes import wintypes

    mpr = ctypes.WinDLL("mpr")  # type: ignore[attr-defined,unused-ignore]
    buf = ctypes.create_unicode_buffer(1024)
    size = wintypes.DWORD(1024)
    if mpr.WNetGetConnectionW(ctypes.c_wchar_p(root.rstrip("\\")), buf, ctypes.byref(size)) == 0:
        return buf.value
    return ""


# --------------------------------------------------------------------------- Linux


@dataclass(frozen=True, slots=True)
class _Mount:
    mount_point: str
    fs_type: str
    source: str
    major: int
    minor: int


@functools.cache
def in_wsl() -> bool:
    """Running inside the Windows Subsystem for Linux."""
    try:
        with open("/proc/sys/kernel/osrelease", encoding="ascii", errors="replace") as fh:
            return "microsoft" in fh.read().lower()
    except OSError:
        return False


# WSL plumbing mounted into every distribution (GUI, drivers, Docker Desktop): never a scan target
_WSL_HIDDEN = ("/mnt/wsl", "/mnt/wslg", "/usr/lib/wsl", "/usr/lib/modules", "/Docker/", "/init")
_WSL_DRIVE = re.compile(r"[A-Za-z]:\\?")
WSL_DRIVE_NOTE = TEXT.raw("wsl_note")  # the Chinese text (message id); shown as TEXT["wsl_note"]


def _wsl_drive(m: _Mount) -> str:
    """``C:`` for a Windows drive mounted into WSL over 9P (DrvFs), else ``""``."""
    if m.fs_type in ("9p", "drvfs", "v9fs") and _WSL_DRIVE.fullmatch(m.source):
        return m.source[:2].upper()
    return ""


def _linux_mounts() -> list[_Mount]:
    mounts = []
    try:
        with open("/proc/self/mountinfo", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                left, _, right = line.partition(" - ")
                fields = left.split()
                rfields = right.split()
                if len(fields) < 5 or len(rfields) < 2:
                    continue
                major, _, minor = fields[2].partition(":")
                point = fields[4].encode("latin-1", "replace").decode("unicode_escape")
                source = rfields[1].encode("latin-1", "replace").decode("unicode_escape")  # "C:\134" → "C:\\"
                mounts.append(_Mount(point, rfields[0], source, int(major), int(minor)))
    except OSError:
        pass
    return mounts


def _posix_from_statvfs(path: str, mount_point: str, fs_type: str, source: str, remote: bool) -> VolumeInfo:
    assert sys.platform != "win32"
    st = os.statvfs(path)  # type: ignore[attr-defined,unused-ignore]
    frsize = st.f_frsize or st.f_bsize
    return VolumeInfo(
        mount_point=mount_point,
        device=source,
        fs_type=fs_type,
        label=os.path.basename(mount_point.rstrip("/")) or mount_point,
        total=st.f_blocks * frsize,
        free=st.f_bavail * frsize,
        reserved=max(0, st.f_bfree - st.f_bavail) * frsize,
        cluster_size=st.f_bsize or 4096,
        is_remote=remote,
        is_removable=mount_point.startswith(("/media/", "/run/media/", "/Volumes/")),
        dev=os.stat(path).st_dev,
        remote_path=source if remote else "",
    )


def _mount_point_by_dev(path: str) -> str:
    path = os.path.realpath(path)
    dev = os.stat(path).st_dev
    while True:
        parent = os.path.dirname(path)
        if parent == path or os.stat(parent).st_dev != dev:
            return path
        path = parent


def _linux_volume(path: str) -> VolumeInfo:
    assert sys.platform != "win32"
    real = os.path.realpath(path)
    dev = os.stat(real).st_dev
    best: _Mount | None = None
    for m in _linux_mounts():
        same_dev = (m.major, m.minor) == (os.major(dev), os.minor(dev))  # type: ignore[attr-defined,unused-ignore]
        inside = real == m.mount_point or real.startswith(m.mount_point.rstrip("/") + "/")
        if same_dev and inside and (best is None or len(m.mount_point) > len(best.mount_point)):
            best = m
    if best is None:
        point = _mount_point_by_dev(real)
        # btrfs: files carry their subvolume's own device number, not the one mountinfo lists; the
        # mount whose point is where the device number changes is theirs
        at = [m for m in _linux_mounts() if m.mount_point == point]
        if at:
            m = at[-1]
            return _wsl_marked(
                m, _posix_from_statvfs(real, point, m.fs_type, m.source, m.fs_type in NETWORK_FS)
            )
        # a subvolume inside a mounted btrfs (or bcachefs): its device number changes at the
        # subvolume, which is no mount point; the file system is the mount above it
        above = [
            m
            for m in _linux_mounts()
            if m.fs_type in SUBVOLUME_FS
            and (real == m.mount_point or real.startswith(m.mount_point.rstrip("/") + "/"))
        ]
        if above:
            m = max(above, key=lambda m: len(m.mount_point))
            return _wsl_marked(m, _posix_from_statvfs(real, m.mount_point, m.fs_type, m.source, False))
        return _posix_from_statvfs(real, point, "", "", False)
    return _wsl_marked(
        best,
        _posix_from_statvfs(real, best.mount_point, best.fs_type, best.source, best.fs_type in NETWORK_FS),
    )


def _wsl_marked(m: _Mount, vol: VolumeInfo) -> VolumeInfo:
    """A Windows drive seen from WSL: named after the drive and marked as not recommended."""
    drive = _wsl_drive(m) if in_wsl() else ""
    if not drive:
        return vol
    return replace(vol, label=TEXT["wsl_label"].format(drive=drive), note=TEXT["wsl_note"], remote_path="")


def _linux_list() -> list[VolumeInfo]:
    out, seen = [], set()
    wsl = in_wsl()
    for m in _linux_mounts():
        if m.fs_type in PSEUDO_FS or m.mount_point.startswith(
            ("/proc", "/sys", "/dev", "/run/user", "/snap/")
        ):
            continue
        if wsl and (m.mount_point.startswith(_WSL_HIDDEN) or (m.fs_type == "9p" and not _wsl_drive(m))):
            continue
        if (m.major, m.minor) in seen:
            continue
        seen.add((m.major, m.minor))
        try:
            vol = _posix_from_statvfs(
                m.mount_point, m.mount_point, m.fs_type, m.source, m.fs_type in NETWORK_FS
            )
        except OSError:
            continue
        out.append(_wsl_marked(m, vol))
    return out


# --------------------------------------------------------------------------- macOS


@functools.cache
def _statfs_struct() -> Any:
    """``struct statfs`` (macOS, 64-bit inodes: the layout statfs$INODE64 / getfsstat$INODE64 fill)."""
    import ctypes

    class StatFs(ctypes.Structure):
        _fields_ = [
            ("f_bsize", ctypes.c_uint32),
            ("f_iosize", ctypes.c_int32),
            ("f_blocks", ctypes.c_uint64),
            ("f_bfree", ctypes.c_uint64),
            ("f_bavail", ctypes.c_uint64),
            ("f_files", ctypes.c_uint64),
            ("f_ffree", ctypes.c_uint64),
            ("f_fsid", ctypes.c_int32 * 2),
            ("f_owner", ctypes.c_uint32),
            ("f_type", ctypes.c_uint32),
            ("f_flags", ctypes.c_uint32),
            ("f_fssubtype", ctypes.c_uint32),
            ("f_fstypename", ctypes.c_char * 16),
            ("f_mntonname", ctypes.c_char * 1024),
            ("f_mntfromname", ctypes.c_char * 1024),
            ("f_flags_ext", ctypes.c_uint32),
            ("f_reserved", ctypes.c_uint32 * 7),
        ]

    return StatFs


def _libc_fn(*symbols: str) -> Any:
    """The first of ``symbols`` libc exports (x86_64 Macs: the $INODE64 variants), or None."""
    import ctypes

    libc = ctypes.CDLL(None, use_errno=True)
    for symbol in symbols:
        try:
            return getattr(libc, symbol)
        except AttributeError:
            continue
    return None


def _darwin_statfs(path: str) -> tuple[str, str, str, bool] | None:
    """(fs type, mount point, source, is_local) via statfs(2); None if unavailable."""
    import ctypes

    fn = _libc_fn("statfs$INODE64", "statfs")
    if fn is None:
        return None
    buf = _statfs_struct()()
    if fn(os.fsencode(path), ctypes.byref(buf)) != 0:
        return None
    mnt_local = 0x00001000
    return (
        buf.f_fstypename.decode(errors="replace"),
        buf.f_mntonname.decode(errors="replace"),
        buf.f_mntfromname.decode(errors="replace"),
        bool(buf.f_flags & mnt_local),
    )


# The APFS Data volume: macOS presents it at "/" together with the sealed system volume (one
# volume group, joined by firmlinks), so user files under /Users, /private/var… belong to "/".
DARWIN_DATA = "/System/Volumes/Data"
FIRMLINKS = "/usr/share/firmlinks"  # macOS 10.15+: "<path under />	<path in the Data volume>"


def _darwin_mounts() -> list[str]:
    """Mount point of every mounted file system (getfsstat(2)); [] if unavailable."""
    import ctypes

    fn = _libc_fn("getfsstat$INODE64", "getfsstat")
    if fn is None:
        return []
    fn.restype = ctypes.c_int
    fn.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int]
    nowait = 2  # MNT_NOWAIT: what the kernel has, without asking each file system
    StatFs = _statfs_struct()
    for _ in range(3):  # mounts may come and go between the two calls
        count = fn(None, 0, nowait)
        if count <= 0:
            return []
        buf = (StatFs * (count + 8))()
        got = fn(buf, ctypes.sizeof(buf), nowait)
        if 0 <= got < count + 8:
            return [buf[i].f_mntonname.decode(errors="replace") for i in range(got)]
    return []


def volume_group_twins(
    root: str,
    mounts: Iterable[str] | None = None,
    dev_of: Callable[[str], int] | None = None,
) -> tuple[str, ...]:
    """Mount points below a mounted ``root`` that report the same device as ``root``.

    On macOS that is the other half of an APFS volume group: the Data volume, mounted at
    /System/Volumes/Data below the system volume at "/" and reporting the same st_dev. Its files
    are already in the tree through the firmlinks (/Users, /Applications, /Library, /private…),
    so a scan of "/" must not enter it a second time. Elsewhere (and for roots that are not
    mount points) there are none.
    """
    if mounts is None:
        if sys.platform != "darwin":
            return ()
        mounts = _darwin_mounts()
    stat_dev = dev_of or (lambda p: os.stat(p).st_dev)
    points = {m.rstrip("/") or "/" for m in mounts}
    base = root.rstrip("/") or "/"
    if base not in points:
        return ()
    try:
        dev = stat_dev(base)
    except OSError:
        return ()
    prefix = base if base == "/" else base + "/"
    twins = []
    for point in sorted(points):
        if point == base or not point.startswith(prefix):
            continue
        try:
            if stat_dev(point) == dev:
                twins.append(point)
        except OSError:
            continue
    return tuple(twins)


def parse_firmlinks(text: str) -> tuple[tuple[str, str], ...]:
    """(path under "/", path inside the Data volume) per line of /usr/share/firmlinks, the longest
    Data-volume path first (a deeper firmlink wins over its parent)."""
    pairs = []
    for line in text.splitlines():
        source, tab, target = line.partition("	")
        source, target = source.strip().rstrip("/"), target.strip().strip("/")
        if tab and source.startswith("/") and target:
            pairs.append((source, target))
    return tuple(sorted(pairs, key=lambda p: len(p[1]), reverse=True))


@functools.cache
def darwin_firmlinks() -> tuple[tuple[str, str], ...]:
    try:
        with open(FIRMLINKS, encoding="utf-8") as f:
            return parse_firmlinks(f.read())
    except OSError:
        return ()


def through_firmlinks(path: str, twin: str, firmlinks: Sequence[tuple[str, str]] | None = None) -> str | None:
    """A path inside a volume-group twin as the firmlinks show it under "/"
    (/System/Volumes/Data/Users/x → /Users/x); None when no firmlink leads there."""
    rel = path[len(twin) :].strip("/")
    for source, target in darwin_firmlinks() if firmlinks is None else firmlinks:
        if rel == target or rel.startswith(target + "/"):
            return source + rel[len(target) :]
    return None


def _darwin_volume(path: str) -> VolumeInfo:
    info = _darwin_statfs(path)
    if info is None:
        return _posix_from_statvfs(path, _mount_point_by_dev(path), "", "", False)
    fs_type, point, source, local = info
    if point == DARWIN_DATA:
        point = "/"
    return _posix_from_statvfs(path, point, fs_type, source, not local)


def _darwin_list() -> list[VolumeInfo]:
    paths = [
        DARWIN_DATA if os.path.isdir(DARWIN_DATA) else "/",  # "/" with the size of the user's data
        *(os.path.join("/Volumes", n) for n in sorted(os.listdir("/Volumes")) if not n.startswith(".")),
    ]
    out, seen = [], set()
    for p in paths:
        try:
            vol = _darwin_volume(p)
        except OSError:
            continue
        if vol.mount_point in seen:
            continue
        seen.add(vol.mount_point)
        out.append(vol)
    return out


# --------------------------------------------------------------------------- public


def volume_for(path: str) -> VolumeInfo:
    """Volume that contains ``path``."""
    if sys.platform == "win32":
        return _win_volume(path)
    if sys.platform == "darwin":
        return _darwin_volume(path)
    return _linux_volume(path)


def list_volumes() -> list[VolumeInfo]:
    """Volumes worth offering on the start page (no pseudo filesystems)."""
    if sys.platform == "win32":
        return _win_list()
    if sys.platform == "darwin":
        return _darwin_list()
    return _linux_list()

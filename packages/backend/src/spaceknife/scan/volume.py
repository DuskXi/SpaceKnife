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
from dataclasses import asdict, dataclass, replace

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

    @property
    def used(self) -> int:
        return max(0, self.total - self.free)

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
                "网络卷未连接"
                if drive_type == 4
                else "没有介质"
                if getattr(exc, "winerror", 0) == 21  # ERROR_NOT_READY
                else f"无法读取卷：{exc.strerror or exc}"
            )
            vol = VolumeInfo(
                root, root.rstrip("\\"), "", "", 0, 0, 4096, drive_type == 4, drive_type == 2, reason=reason
            )
        if vol.total == 0 and not vol.reason:
            vol = replace(vol, reason="没有介质")
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
WSL_DRIVE_NOTE = "WSL 经 9P 协议访问的 Windows 盘，扫描会非常慢；建议用 Windows 版的 SpaceKnife 扫描"


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
        return _posix_from_statvfs(real, point, "", "", False)
    return _wsl_marked(
        best,
        _posix_from_statvfs(real, best.mount_point, best.fs_type, best.source, best.fs_type in NETWORK_FS),
    )


def _wsl_marked(m: _Mount, vol: VolumeInfo) -> VolumeInfo:
    """A Windows drive seen from WSL: named after the drive and marked as not recommended."""
    drive = _wsl_drive(m) if in_wsl() else ""
    return replace(vol, label=f"{drive}（Windows 盘）", note=WSL_DRIVE_NOTE) if drive else vol


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


def _darwin_statfs(path: str) -> tuple[str, str, str, bool] | None:
    """(fs type, mount point, source, is_local) via statfs(2); None if unavailable."""
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

    libc = ctypes.CDLL(None, use_errno=True)
    fn = None
    for symbol in ("statfs$INODE64", "statfs"):
        fn = getattr(libc, symbol, None)
        if fn is not None:
            break
    if fn is None:
        return None
    buf = StatFs()
    if fn(os.fsencode(path), ctypes.byref(buf)) != 0:
        return None
    mnt_local = 0x00001000
    return (
        buf.f_fstypename.decode(errors="replace"),
        buf.f_mntonname.decode(errors="replace"),
        buf.f_mntfromname.decode(errors="replace"),
        bool(buf.f_flags & mnt_local),
    )


def _darwin_volume(path: str) -> VolumeInfo:
    info = _darwin_statfs(path)
    if info is None:
        return _posix_from_statvfs(path, _mount_point_by_dev(path), "", "", False)
    fs_type, point, source, local = info
    return _posix_from_statvfs(path, point, fs_type, source, not local)


def _darwin_list() -> list[VolumeInfo]:
    paths = [
        "/",
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

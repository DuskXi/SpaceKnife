"""Facts the cleanup plan decides and warns from (docs/dupes.md §16.6.2).

Two kinds:

- **cheap facts**, worked out when a plan is built, from what the scan and the search already know
  (paths, the scan's attribute flags, volumes, the insights rules, the scan tree itself for ``.git``
  and project markers, a few small configuration files for the sync folders): no system call per
  file. They become tag bits (``codes.T_*``), one ``u8`` per copy;
- **the risk check** (:func:`probe`): per copy, one metadata-only look (owner, permissions, extra
  streams and extended attributes, file flags, link count). It runs in the background through the
  disk's permits; it never opens file data, never recalls a cloud file, never moves an access time.

Whatever cannot be found out counts as a risk (I7): a failed probe leaves ``D_UNKNOWN``.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import re
import stat
import sys
import time
from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from functools import lru_cache
from typing import Any

from . import codes as K

_SEP = re.compile(r"[\\/]+")

INPLACE_EXT = frozenset(
    [
        ".vhd",
        ".vhdx",
        ".vmdk",
        ".qcow2",
        ".vdi",
        ".img",
        ".sqlite",
        ".sqlite3",
        ".db",
        ".mdb",
        ".accdb",
        ".pst",
        ".ost",
        ".kdbx",
        ".psd",
        ".blend",
        ".vmem",
    ]
)
EXEC_EXT = frozenset(
    [
        ".exe",
        ".dll",
        ".msi",
        ".msix",
        ".appx",
        ".bat",
        ".cmd",
        ".ps1",
        ".vbs",
        ".js",
        ".jse",
        ".wsf",
        ".hta",
        ".scr",
        ".com",
        ".jar",
        ".sh",
        ".py",
        ".lnk",
        ".docm",
        ".xlsm",
        ".pptm",
        ".dotm",
        ".xltm",
        ".apk",
        ".dmg",
        ".pkg",
        ".app",
        ".iso",
        ".zip",
        ".7z",
        ".rar",
        ".cab",
        ".tar",
        ".gz",
        ".tgz",
        ".xz",
    ]
)
ARCHIVE_EXT = frozenset(
    [
        ".zip",
        ".7z",
        ".rar",
        ".cab",
        ".tar",
        ".gz",
        ".tgz",
        ".xz",
        ".bz2",
        ".zst",
        ".iso",
        ".msi",
        ".exe",
        ".dmg",
        ".pkg",
        ".deb",
        ".rpm",
    ]
)
FONT_DRIVER_EXT = frozenset(
    [".ttf", ".otf", ".ttc", ".fon", ".woff", ".woff2", ".sys", ".inf", ".cat", ".ko", ".kext"]
)
DOC_CLASSES = frozenset({"document", "code", "sheet", "spreadsheet", "text"})
MEDIA_CLASSES = frozenset({"image", "video", "audio", "music", "photo"})
BACKUP_NAMES = frozenset(
    {
        "backup",
        "backups",
        "bak",
        "备份",  # i18n: ignore
        "副本",  # i18n: ignore
        "归档",  # i18n: ignore
        "archive",
        "archives",
        "old",
        "旧",  # i18n: ignore
        "file history",
        "backups.backupdb",
        "time machine backups",
    }
)
PROJECT_MARKERS = frozenset(
    {
        "package.json",
        "cargo.toml",
        "pyproject.toml",
        "cmakelists.txt",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "composer.json",
        "gemfile",
    }
)
PROJECT_EXT_MARKERS = (".sln", ".csproj", ".vcxproj", ".xcodeproj", ".uproject", ".lrcat", ".prproj", ".aep")
VCS_NAMES = (".git", ".hg", ".svn")
MEDIA_LIB_MARKERS = (".lrcat", ".photoslibrary", "itunes media", "music media", "itunes library.itl")
PACKAGE_EXT = (".app", ".photoslibrary", ".bundle", ".framework", ".pkg", ".plugin", ".xcarchive")


# ------------------------------------------------------------------ system facts
@dataclass(slots=True)
class SystemFacts:
    platform: str = sys.platform
    symlink_right: bool = True  # Windows: developer mode or SeCreateSymbolicLinkPrivilege
    developer_mode: bool = False
    protected_hardlinks: bool = False  # Linux fs.protected_hardlinks = 1
    uid: int = -1
    macos_links: bool = False  # setting dupes.cleanup.macos_links
    native: bool = True  # the job's reads were native (Windows replacements need the same identity)


def _win_developer_mode() -> bool:  # pragma: no cover - Windows
    if sys.platform == "win32":  # the registry (and its type stubs) exist on Windows only
        import winreg

        try:
            with winreg.OpenKey(
                winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock"
            ) as k:
                value, _ = winreg.QueryValueEx(k, "AllowDevelopmentWithoutDevLicense")
                return int(value) == 1
        except OSError:
            return False
    return False


def _win_has_symlink_privilege() -> bool:  # pragma: no cover - Windows
    """Whether the process token holds SeCreateSymbolicLinkPrivilege (enabled or not)."""
    import ctypes
    from ctypes import wintypes

    advapi = ctypes.WinDLL("advapi32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    k32.GetCurrentProcess.restype = wintypes.HANDLE
    advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    advapi.GetTokenInformation.argtypes = [
        wintypes.HANDLE,
        ctypes.c_int,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
    ]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    token = wintypes.HANDLE()
    if not advapi.OpenProcessToken(k32.GetCurrentProcess(), 0x0008, ctypes.byref(token)):  # TOKEN_QUERY
        return False
    try:

        class LUID(ctypes.Structure):
            _fields_ = [("LowPart", wintypes.DWORD), ("HighPart", wintypes.LONG)]

        want = LUID()
        if not advapi.LookupPrivilegeValueW(None, "SeCreateSymbolicLinkPrivilege", ctypes.byref(want)):
            return False
        size = wintypes.DWORD()
        advapi.GetTokenInformation(token, 3, None, 0, ctypes.byref(size))  # TokenPrivileges
        buf = ctypes.create_string_buffer(size.value)
        if not advapi.GetTokenInformation(token, 3, buf, size, ctypes.byref(size)):
            return False
        count = int.from_bytes(buf.raw[:4], "little")
        for i in range(count):
            at = 4 + 12 * i
            low = int.from_bytes(buf.raw[at : at + 4], "little")
            high = int.from_bytes(buf.raw[at + 4 : at + 8], "little", signed=True)
            if low == want.LowPart and high == want.HighPart:
                return True
        return False
    finally:
        k32.CloseHandle(token)


@lru_cache(maxsize=1)
def _system_base() -> tuple[bool, bool, bool, int]:
    if sys.platform == "win32":  # pragma: no cover - Windows
        dev = _win_developer_mode()
        return (dev or _win_has_symlink_privilege(), dev, False, -1)
    protected = False
    if sys.platform.startswith("linux"):
        with (
            contextlib.suppress(OSError, ValueError),
            open("/proc/sys/fs/protected_hardlinks", encoding="ascii") as f,
        ):
            protected = f.read().strip() == "1"
    return (True, False, protected, os.getuid() if hasattr(os, "getuid") else -1)


def system_facts(*, macos_links: bool = False, native: bool = True) -> SystemFacts:
    right, dev, protected, uid = _system_base()
    if os.environ.get("SPACEKNIFE_CLEANUP_NO_SYMLINK_RIGHT") == "1" and os.environ.get(
        "SPACEKNIFE_CLEANUP_SANDBOX"
    ):
        right, dev = False, False  # tests: a process without the right (only inside a sandbox)
    return SystemFacts(
        symlink_right=right,
        developer_mode=dev,
        protected_hardlinks=protected,
        uid=uid,
        macos_links=macos_links,
        native=native,
    )


# ------------------------------------------------------------------ sync folders
@dataclass(frozen=True, slots=True)
class SyncRoot:
    path: str
    provider: str


_SYNC_CACHE: list[tuple[float, list[SyncRoot]]] = []
SYNC_TTL_S = 60.0


def _read(path: str, limit: int = 1 << 20) -> str:
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read(limit)
    except OSError:
        return ""


def _win_sync_roots() -> list[SyncRoot]:  # pragma: no cover - Windows
    if sys.platform != "win32":
        return []
    return _win_sync_roots_impl()


def _win_sync_roots_impl() -> list[SyncRoot]:  # pragma: no cover - Windows
    if sys.platform == "win32":
        import winreg

        out: list[SyncRoot] = []

        def values(root: Any, path: str) -> list[tuple[str, Any]]:
            got: list[tuple[str, Any]] = []
            try:
                with winreg.OpenKey(root, path) as k:
                    i = 0
                    while True:
                        try:
                            got.append(winreg.EnumValue(k, i)[:2])
                        except OSError:
                            break
                        i += 1
            except OSError:
                pass
            return got

        def subkeys(root: Any, path: str) -> list[str]:
            got: list[str] = []
            try:
                with winreg.OpenKey(root, path) as k:
                    i = 0
                    while True:
                        try:
                            got.append(winreg.EnumKey(k, i))
                        except OSError:
                            break
                        i += 1
            except OSError:
                pass
            return got

        base = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager"
        for name in subkeys(winreg.HKEY_LOCAL_MACHINE, base):
            provider = name.split("!")[0]
            for _, v in values(winreg.HKEY_LOCAL_MACHINE, base + "\\" + name + "\\UserSyncRoots"):
                if isinstance(v, str) and v:
                    out.append(SyncRoot(v, provider))
        for acc in subkeys(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\OneDrive\Accounts"):
            for k, v in values(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\OneDrive\Accounts\\" + acc):
                if k == "UserFolder" and isinstance(v, str) and v:
                    out.append(SyncRoot(v, "OneDrive"))
        for k, v in os.environ.items():
            if k.lower().startswith("onedrive") and v and os.path.isabs(v):
                out.append(SyncRoot(v, "OneDrive"))
        for base_dir in (os.environ.get("APPDATA", ""), os.environ.get("LOCALAPPDATA", "")):
            if base_dir:
                out.extend(_dropbox(os.path.join(base_dir, "Dropbox", "info.json")))
        appdata = os.environ.get("APPDATA", "")
        if appdata:
            out.extend(_nextcloud(os.path.join(appdata, "Nextcloud", "nextcloud.cfg")))
            out.extend(
                _syncthing(os.path.join(os.environ.get("LOCALAPPDATA", ""), "Syncthing", "config.xml"))
            )
        return out
    return []


def _dropbox(info: str) -> list[SyncRoot]:
    text = _read(info)
    if not text:
        return []
    out: list[SyncRoot] = []
    with contextlib.suppress(ValueError):
        data = json.loads(text)
        for v in data.values() if isinstance(data, dict) else ():
            if isinstance(v, dict) and isinstance(v.get("path"), str):
                out.append(SyncRoot(v["path"], "Dropbox"))
    return out


def _nextcloud(cfg: str) -> list[SyncRoot]:
    out = []
    for m in re.finditer(r"(?m)^\d+\\Folders\\\d+\\localPath=(.+)$", _read(cfg)):
        out.append(SyncRoot(m.group(1).strip().rstrip("/\\"), "Nextcloud"))
    return out


def _syncthing(cfg: str) -> list[SyncRoot]:
    return [
        SyncRoot(m.group(1), "Syncthing") for m in re.finditer(r'<folder [^>]*path="([^"]+)"', _read(cfg))
    ]


def _posix_sync_roots() -> list[SyncRoot]:
    home = os.path.expanduser("~")
    out: list[SyncRoot] = []
    out.extend(_dropbox(os.path.join(home, ".dropbox", "info.json")))
    for cfg in (
        os.path.join(home, ".config", "syncthing", "config.xml"),
        os.path.join(home, ".local", "state", "syncthing", "config.xml"),
        os.path.join(home, "Library", "Application Support", "Syncthing", "config.xml"),
    ):
        out.extend(_syncthing(cfg))
    for cfg in (
        os.path.join(home, ".config", "Nextcloud", "nextcloud.cfg"),
        os.path.join(home, ".config", "ownCloud", "owncloud.cfg"),
    ):
        out.extend(_nextcloud(cfg))
    m = re.search(
        r'(?m)^\s*sync_dir\s*=\s*"([^"]+)"', _read(os.path.join(home, ".config", "onedrive", "config"))
    )
    if m:
        out.append(SyncRoot(os.path.expanduser(m.group(1)), "OneDrive"))
    if sys.platform == "darwin":  # not verified
        cloud = os.path.join(home, "Library", "CloudStorage")
        with contextlib.suppress(OSError):
            for name in os.listdir(cloud):
                out.append(SyncRoot(os.path.join(cloud, name), name.split("-")[0]))
        out.append(SyncRoot(os.path.join(home, "Library", "Mobile Documents"), "iCloud"))
    return out


def sync_roots() -> list[SyncRoot]:
    """Cloud-sync folders of this user (a few small files and registry keys, cached 60 s);
    ``SPACEKNIFE_CLEANUP_SYNC_ROOTS`` (``provider=path;...``, tests) adds to them."""
    now = time.monotonic()
    if _SYNC_CACHE and now - _SYNC_CACHE[0][0] < SYNC_TTL_S:
        roots = list(_SYNC_CACHE[0][1])
    else:
        try:
            roots = _win_sync_roots() if sys.platform == "win32" else _posix_sync_roots()
        except Exception:  # advisory: a broken config file is no reason to fail the plan
            roots = []
        _SYNC_CACHE[:] = [(now, roots)]
    extra = os.environ.get("SPACEKNIFE_CLEANUP_SYNC_ROOTS", "")
    for part in extra.split(";"):
        if "=" in part:
            prov, path = part.split("=", 1)
            roots.append(SyncRoot(path, prov))
    seen: set[str] = set()
    out: list[SyncRoot] = []
    for r in roots:
        key = _key(r.path)
        if key and key not in seen:
            seen.add(key)
            out.append(r)
    return out


# ------------------------------------------------------------------ known folders
def _key(path: str) -> str:
    p = os.path.normpath(path)
    return os.path.normcase(p).rstrip("\\/") if p else ""


@lru_cache(maxsize=4)
def known_folders(home: str) -> dict[str, int]:
    """Folder key -> tag bit: downloads, desktop, temp, app data, caches."""
    out: dict[str, int] = {}

    def add(path: str | None, bit: int) -> None:
        if path:
            out[_key(path)] = out.get(_key(path), 0) | bit

    add(os.path.join(home, "Downloads"), K.T_DOWNLOADS)
    add(os.path.join(home, "Desktop"), K.T_DOWNLOADS)
    add(os.path.join(home, "下载"), K.T_DOWNLOADS)  # i18n: ignore
    add(os.path.join(home, "桌面"), K.T_DOWNLOADS)  # i18n: ignore
    add(os.environ.get("TEMP"), K.T_TEMP)
    add(os.environ.get("TMP"), K.T_TEMP)
    if sys.platform == "win32":
        add(os.environ.get("APPDATA"), K.T_APP)
        add(os.environ.get("LOCALAPPDATA"), K.T_APP)
        add(os.environ.get("PROGRAMDATA"), K.T_APP)
    else:
        add("/tmp", K.T_TEMP)
        add("/var/tmp", K.T_TEMP)
        add(os.path.join(home, ".config"), K.T_APP)
        add(os.path.join(home, ".local", "share"), K.T_APP)
        add(os.path.join(home, ".cache"), K.T_CACHE)
        add(os.path.join(home, "Library"), K.T_APP)
        add(os.path.join(home, "Library", "Caches"), K.T_CACHE)
        xdg = os.environ.get("XDG_DOWNLOAD_DIR")
        add(xdg, K.T_DOWNLOADS)
    return out


def _ancestors(key: str) -> Iterable[str]:
    cur = key
    while True:
        yield cur
        up = os.path.dirname(cur)
        if up == cur or not up:
            return
        cur = up


# ------------------------------------------------------------------ cheap facts of the rows
@dataclass(slots=True)
class DirFacts:
    tags: int = 0
    sync: str = ""  # provider
    sync_root: str = ""
    repo: str = ""  # the work tree's folder
    project: str = ""  # the marker found
    app: str = ""  # insights: the program owning it
    media_lib: str = ""
    backup: str = ""  # the folder name that looks like a backup


class TagIndex:
    """Cheap facts of folders, worked out once per distinct parent folder of the copies."""

    def __init__(self, stores: Sequence[Any], *, now: float | None = None) -> None:
        self.stores = stores
        self.home = os.path.expanduser("~")
        self.known = known_folders(self.home)
        self.sync = [(r, _key(r.path)) for r in sync_roots()]
        self.now = time.time() if now is None else now
        self._dirs: dict[tuple[int, str], DirFacts] = {}
        self._has: dict[tuple[int, str], bool] = {}
        self._insight: dict[str, Any] = {}
        self.link_targets: set[str] = set()  # keys of paths SpaceKnife's links point at

    def _child(self, si: int, folder: str, name: str) -> bool:
        k = (si, os.path.join(folder, name))
        hit = self._has.get(k)
        if hit is None:
            store = self.stores[si]
            hit = False
            with contextlib.suppress(Exception):
                hit = store.find(k[1]) is not None
            self._has[k] = hit
        return hit

    def _insight_of(self, folder: str) -> Any:
        hit = self._insight.get(folder, False)
        if hit is False:
            hit = None
            with contextlib.suppress(Exception):
                from ...insights.rules import match

                hit = match(folder, "dir")
            self._insight[folder] = hit
        return hit

    def dir_facts(self, si: int, folder: str) -> DirFacts:
        k = (si, folder)
        hit = self._dirs.get(k)
        if hit is not None:
            return hit
        f = DirFacts()
        key = _key(folder)
        parts = [p.lower() for p in _SEP.split(folder) if p]
        if any(p in VCS_NAMES for p in parts):
            f.tags |= K.T_VCS_INTERNAL
        for p in parts:
            if p.endswith(PACKAGE_EXT):
                f.tags |= K.T_PACKAGE
                break
        for p in parts[-3:]:
            if p in BACKUP_NAMES or p.startswith(("backup", "备份")):  # i18n: ignore
                f.tags |= K.T_BACKUP
                f.backup = p
                break
        for a in _ancestors(key):
            bit = self.known.get(a)
            if bit:
                f.tags |= bit
        for r, rk in self.sync:
            if key == rk or key.startswith(rk + os.sep) or (len(rk) <= 3 and key.startswith(rk)):
                f.tags |= K.T_SYNC
                f.sync, f.sync_root = r.provider, r.path
                break
        # the scan tree: a version-control work tree, a project, a media library, a Syncthing folder
        cur = folder
        for _ in range(64):
            if not f.repo:
                for v in VCS_NAMES:
                    if self._child(si, cur, v):
                        f.repo = cur
                        f.tags |= K.T_VCS
                        break
            if not f.project:
                for m in PROJECT_MARKERS:
                    if self._child(si, cur, m):
                        f.project = m
                        f.tags |= K.T_PROJECT
                        break
            if not f.tags & K.T_SYNC and self._child(si, cur, ".stfolder"):
                f.tags |= K.T_SYNC
                f.sync, f.sync_root = "Syncthing", cur
            up = os.path.dirname(cur)
            if up == cur or not up:
                break
            cur = up
        if any(p.endswith(MEDIA_LIB_MARKERS) or p in MEDIA_LIB_MARKERS for p in parts):
            f.tags |= K.T_MEDIA_LIB
            f.media_lib = next(p for p in parts if p.endswith(MEDIA_LIB_MARKERS) or p in MEDIA_LIB_MARKERS)
        ins = self._insight_of(folder)
        if ins is not None:
            kind = getattr(ins, "kind", "")
            cleanup = getattr(ins, "cleanup", "")
            f.app = getattr(ins, "app", "") or getattr(ins, "title", "")
            if kind in ("app", "chat", "vm"):
                f.tags |= K.T_APP
            elif kind == "game":
                f.tags |= K.T_GAME
            elif kind == "cache" or kind == "build":
                f.tags |= K.T_CACHE
            elif kind == "system":
                f.tags |= K.T_PROGRAM
            if cleanup == "safe":
                f.tags |= K.T_SAFE_CLEAN | K.T_CACHE
            elif cleanup == "keep":
                f.tags |= K.T_LOCKED_APP | K.T_APP
        with contextlib.suppress(Exception):
            from ...ops.paths import protected_reason

            if protected_reason(folder) is not None:
                f.tags |= K.T_PROGRAM
        self._dirs[k] = f
        return f


def name_tags(name: str, cls: list[str]) -> int:
    ext = os.path.splitext(name)[1].lower()
    t = 0
    if ext in INPLACE_EXT:
        t |= K.T_INPLACE
    if ext in EXEC_EXT:
        t |= K.T_EXEC
    if ext in ARCHIVE_EXT:
        t |= K.T_ARCHIVE
    if ext in FONT_DRIVER_EXT:
        t |= K.T_FONT_DRIVER
    cs = set(cls)
    if cs & DOC_CLASSES:
        t |= K.T_DOCLIKE
    if cs & MEDIA_CLASSES:
        t |= K.T_MEDIA
    if name.startswith(".") and sys.platform != "win32":
        t |= K.T_HIDDEN
    return t


# ------------------------------------------------------------------ the risk check (one look per file)
F_MOTW = 1 << 0  # marked as downloaded (Zone.Identifier, com.apple.quarantine, user.xdg.origin.url)
F_EFS = 1 << 1
F_IMMUTABLE = 1 << 2  # chattr +i / +a, uchg / schg
F_NOCOW = 1 << 3
F_READONLY = 1 << 4
F_HIDDEN = 1 << 5
F_SYSTEM = 1 << 6
F_SPARSE = 1 << 7
F_COMPRESSED = 1 << 8
F_ACL = 1 << 9  # a POSIX ACL / explicit Windows ACEs
F_DEDUP = 1 << 10
F_INTEGRITY = 1 << 11  # ReFS integrity streams


@dataclass(slots=True)
class Probe:
    ok: bool = False
    error: str = ""
    owner: str = ""
    owner_key: int = 0
    acl_explicit: int = 0
    acl_inherited: int = 0
    mode: int = 0
    bits: int = 0
    streams: list[tuple[str, int]] = field(
        default_factory=list
    )  # (name, bytes), the mark of the web excluded
    streams_bytes: int = 0
    nlink: int = 0
    selinux: str = ""
    uid: int = -1
    writable: bool = True  # this process may write it (Linux in-place sharing, Windows read-only)
    dir_writable: bool = True  # its folder takes new names (a stand-in, a move to the recovery area)


def _h64(text: str) -> int:
    return int.from_bytes(
        hashlib.blake2b(text.encode("utf-8", "surrogatepass"), digest_size=8).digest(), "little"
    )


_LISTXATTR: Any = getattr(os, "listxattr", lambda *a, **k: [])
_GETXATTR: Any = getattr(os, "getxattr", lambda *a, **k: b"")


def _probe_posix(path: str) -> Probe:
    p = Probe()
    st = os.lstat(path)
    if not stat.S_ISREG(st.st_mode):
        raise OSError(0, "not a file")
    p.owner = str(st.st_uid)
    with contextlib.suppress(Exception):
        import pwd

        getpw: Any = getattr(pwd, "getpwuid")  # noqa: B009 (no pwd in the Windows stubs)
        p.owner = str(getpw(st.st_uid).pw_name)
    p.uid = st.st_uid
    p.owner_key = st.st_uid + 1
    p.mode = stat.S_IMODE(st.st_mode)
    p.nlink = st.st_nlink
    flags = getattr(st, "st_flags", 0)
    if flags & (0x2 | 0x4 | 0x20000 | 0x40000):  # UF_IMMUTABLE, UF_APPEND, SF_IMMUTABLE, SF_APPEND
        p.bits |= F_IMMUTABLE
    names: list[str] = []
    with contextlib.suppress(OSError, AttributeError):
        names = _LISTXATTR(path, follow_symlinks=False)
    for n in names:
        size = 0
        with contextlib.suppress(OSError):
            size = len(_GETXATTR(path, n, follow_symlinks=False))
        if n in ("user.xdg.origin.url", "user.xdg.referrer.url", "com.apple.quarantine"):
            p.bits |= F_MOTW
            continue
        if n == "system.posix_acl_access":
            p.bits |= F_ACL
            with contextlib.suppress(OSError):
                p.acl_explicit = _h64(_GETXATTR(path, n, follow_symlinks=False).hex())
            continue
        if n == "security.selinux":
            with contextlib.suppress(OSError):
                p.selinux = _GETXATTR(path, n, follow_symlinks=False).decode("utf-8", "replace").rstrip("\0")
            continue
        if n.startswith(("trusted.", "security.")):
            continue
        p.streams.append((n, size))
        p.streams_bytes += size
    if sys.platform.startswith("linux"):
        import fcntl
        import struct

        fd = -1
        try:
            fd = os.open(
                path, os.O_RDONLY | os.O_NOFOLLOW | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NONBLOCK", 0)
            )
            ioctl: Any = getattr(fcntl, "ioctl")  # noqa: B009 (no fcntl.ioctl in the Windows stubs)
            buf = ioctl(fd, 0x80086601, b"\0" * 8)  # FS_IOC_GETFLAGS
            fl = struct.unpack("<q", buf)[0] if len(buf) == 8 else 0
            if fl & (0x10 | 0x20):  # FS_IMMUTABLE_FL, FS_APPEND_FL
                p.bits |= F_IMMUTABLE
            if fl & 0x00800000:  # FS_NOCOW_FL
                p.bits |= F_NOCOW
        except OSError:
            pass
        finally:
            if fd >= 0:
                os.close(fd)
    p.writable = os.access(path, os.W_OK)
    p.dir_writable = os.access(os.path.dirname(path) or ".", os.W_OK | os.X_OK)
    p.ok = True
    return p


def _probe_windows(path: str) -> Probe:  # pragma: no cover - Windows
    import ctypes
    from ctypes import wintypes

    from .winfs import verbatim

    p = Probe()
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    advapi = ctypes.WinDLL("advapi32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    vpath = verbatim(path)
    attrs = k32.GetFileAttributesW(ctypes.c_wchar_p(vpath))
    if attrs == 0xFFFFFFFF:
        raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]
    for bit, flag in (
        (0x1, F_READONLY),
        (0x2, F_HIDDEN),
        (0x4, F_SYSTEM),
        (0x200, F_SPARSE),
        (0x800, F_COMPRESSED),
        (0x4000, F_EFS),
        (0x8000, F_INTEGRITY),
    ):
        if attrs & bit:
            p.bits |= flag
    p.writable = not attrs & 0x1
    # owner and DACL as SDDL: explicit and inherited ACEs digested apart
    need = wintypes.DWORD()
    info = 0x1 | 0x2 | 0x4  # OWNER, GROUP, DACL
    advapi.GetFileSecurityW(ctypes.c_wchar_p(vpath), info, None, 0, ctypes.byref(need))
    if need.value:
        sd = ctypes.create_string_buffer(need.value)
        if advapi.GetFileSecurityW(ctypes.c_wchar_p(vpath), info, sd, need, ctypes.byref(need)):
            out = ctypes.c_wchar_p()
            if advapi.ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd, 1, info, ctypes.byref(out), None
            ):
                sddl = out.value or ""
                k32.LocalFree(out)
                m = re.search(r"O:([^:]+?)(?=G:|D:|S:|$)", sddl)
                p.owner = m.group(1) if m else ""
                p.owner_key = _h64(p.owner) if p.owner else 0
                d = sddl[sddl.find("D:") :] if "D:" in sddl else ""
                aces = re.findall(r"\(([^)]*)\)", d)
                inherited = sorted(a for a in aces if "ID" in [*a.split(";"), "", ""][1])
                explicit = sorted(a for a in aces if "ID" not in [*a.split(";"), "", ""][1])
                protected = d.startswith("D:P")
                p.acl_explicit = _h64("|".join(explicit) + ("P" if protected else ""))
                p.acl_inherited = _h64("|".join(inherited))
                if explicit:
                    p.bits |= F_ACL
                p.owner = _win_account(p.owner)
    # streams: names and sizes, the mark of the web apart

    class STREAM(ctypes.Structure):
        _fields_ = [("StreamSize", ctypes.c_longlong), ("cStreamName", ctypes.c_wchar * 296)]

    data = STREAM()
    k32.FindFirstStreamW.restype = wintypes.HANDLE
    k32.FindFirstStreamW.argtypes = [wintypes.LPCWSTR, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    k32.FindNextStreamW.argtypes = [wintypes.HANDLE, ctypes.c_void_p]
    k32.FindClose.argtypes = [wintypes.HANDLE]
    h = k32.FindFirstStreamW(ctypes.c_wchar_p(vpath), 0, ctypes.byref(data), 0)
    if h and h != wintypes.HANDLE(-1).value:
        try:
            while True:
                name = data.cStreamName
                if name not in ("::$DATA",):
                    nm = name.split(":")[1] if name.count(":") >= 2 else name
                    if nm.lower() == "zone.identifier":
                        p.bits |= F_MOTW
                    else:
                        p.streams.append((nm, int(data.StreamSize)))
                        p.streams_bytes += int(data.StreamSize)
                if not k32.FindNextStreamW(wintypes.HANDLE(h), ctypes.byref(data)):
                    break
        finally:
            k32.FindClose(wintypes.HANDLE(h))
    p.ok = True
    return p


def _win_account(sid: str) -> str:  # pragma: no cover - Windows
    """A SID as DOMAIN\\name where Windows knows it (the SID otherwise)."""
    import ctypes
    from ctypes import wintypes

    advapi = ctypes.WinDLL("advapi32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    psid = ctypes.c_void_p()
    if len(sid) <= 2 or not advapi.ConvertStringSidToSidW(ctypes.c_wchar_p(sid), ctypes.byref(psid)):
        return sid
    try:
        name = ctypes.create_unicode_buffer(256)
        dom = ctypes.create_unicode_buffer(256)
        n, d, use = wintypes.DWORD(256), wintypes.DWORD(256), wintypes.DWORD()
        if advapi.LookupAccountSidW(
            None, psid, name, ctypes.byref(n), dom, ctypes.byref(d), ctypes.byref(use)
        ):
            return f"{dom.value}\\{name.value}" if dom.value else name.value
        return sid
    finally:
        ctypes.WinDLL("kernel32").LocalFree(psid)  # type: ignore[attr-defined,unused-ignore]


def probe(path: str) -> Probe:
    """One metadata-only look at a file (never its data). A failure gives ``ok=False``."""
    try:
        return _probe_windows(path) if sys.platform == "win32" else _probe_posix(path)
    except Exception as exc:  # I7: unknown facts count as a risk, they never stop the plan
        return Probe(ok=False, error=str(exc) or type(exc).__name__)


def probe_many(paths: Sequence[str]) -> list[Probe]:
    return [probe(p) for p in paths]

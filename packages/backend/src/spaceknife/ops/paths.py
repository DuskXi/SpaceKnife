"""Path forms for copying and the built-in list of protected paths (docs/manage.md §2.1).

Every frontend copies paths through :func:`path_forms`, so the three of them put the same text on
the clipboard. :func:`protected_reason` is asked when an item is added to the delete basket and
again right before it is deleted or moved to the trash.

The protection lists are per platform (:func:`platform_rules`); :func:`match_rules` applies one
to a path without touching the disk, so every platform's list is tested on every platform.
:func:`protected_reason` adds what needs the disk: mount points, and paths whose parent folders
lead through a link into a protected place.
"""

from __future__ import annotations

import ntpath
import os
import posixpath
import re
import stat
import sys
import time
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from functools import lru_cache
from typing import Literal
from urllib.parse import quote

from .. import paths as app_paths
from ..i18n import Texts

System = Literal["windows", "linux", "darwin"]
IS_WINDOWS = sys.platform == "win32"

# Every user-facing text of this module (M7.5 extracts them for translation).
TEXT = Texts(
    {
        "root": "卷的根目录不能删除",
        "mount": "这里挂载着另一个卷，不能删除",
        "system": "系统目录，删除会损坏操作系统",
        "boot": "启动与恢复所需的文件，删除后系统可能无法启动",
        "system_file": "系统的分页 / 休眠 / 交换文件，由系统管理",
        "program_root": "程序安装位置的根目录，只能删除其中的单个程序",
        "data_root": "系统级应用数据的根目录，只能删除其中的单项",
        "shared_root": "系统共享目录本身，只能删除其中的内容",
        "users_root": "存放所有用户主目录的文件夹",
        "home": "用户主目录本身不能删除",
        "profile": "用户的应用配置与数据目录本身，只能删除其中的单项",
        "trash": "回收站目录，请用系统的「清空回收站」",
        "package_db": "软件包数据库，删除会损坏包管理器",
        "app": "SpaceKnife 自己的数据目录",
        "program": "SpaceKnife 正在运行的程序所在目录",
    }
)


# --------------------------------------------------------------------------- path forms


@dataclass(frozen=True, slots=True)
class PathForms:
    raw: str  # as the system writes it
    quoted: str  # Windows: in double quotes; POSIX: one shell word in single quotes
    posix: str  # forward slashes (C:/a, //host/share/a); POSIX paths unchanged
    uri: str  # RFC 8089 file URI, UTF-8 percent-encoded


def _strip_verbatim(path: str) -> str:
    """``\\\\?\\C:\\a`` → ``C:\\a`` and ``\\\\?\\UNC\\host\\share`` → ``\\\\host\\share``."""
    if path.startswith(("\\\\?\\UNC\\", "\\\\?\\unc\\")):
        return "\\\\" + path[8:]
    if path.startswith("\\\\?\\") and len(path) >= 6 and path[5] == ":":
        return path[4:]
    return path


def _quote_path(data: bytes) -> str:
    return quote(data, safe="/")


def path_forms(path: str, *, system: System | None = None) -> PathForms:
    """The four copy formats of ``path`` (an absolute path of ``system``, default: this one).

    Windows: ``raw`` ``C:\\a b``, ``quoted`` ``"C:\\a b"``, ``posix`` the WSL form ``/mnt/c/a b``
    (UI-W5-12: drive letter lower-cased under ``/mnt/``), ``uri`` ``file:///C:/a%20b`` (UNC
    ``\\\\host\\share\\x`` → ``//host/share/x`` and ``file://host/share/x``).
    POSIX: ``quoted`` is ``'…'`` with each ``'`` written as ``'\\''``; ``posix`` is the path.
    """
    if (system or _this_system()) == "windows":
        plain = _strip_verbatim(path)
        slashed = plain.replace("\\", "/")
        data = slashed.encode("utf-8", "surrogatepass")
        if len(slashed) >= 2 and slashed[1] == ":":  # drive path: file:///C:/…
            uri = "file:///" + slashed[:2] + _quote_path(data[2:])
        elif slashed.startswith("//"):  # UNC: the host is the URI authority
            uri = "file:" + _quote_path(data)
        else:
            uri = "file:///" + _quote_path(data.lstrip(b"/"))
        posix = (
            f"/mnt/{slashed[0].lower()}{slashed[2:]}" if len(slashed) >= 2 and slashed[1] == ":" else slashed
        )
        return PathForms(raw=path, quoted=f'"{path}"', posix=posix, uri=uri)
    data = path.encode("utf-8", "surrogateescape")
    quoted = "'" + path.replace("'", "'\\''") + "'"
    return PathForms(raw=path, quoted=quoted, posix=path, uri="file://" + _quote_path(data))


def _this_system() -> System:
    if IS_WINDOWS:
        return "windows"
    return "darwin" if sys.platform == "darwin" else "linux"


# --------------------------------------------------------------------------- protection rules


@dataclass(frozen=True, slots=True)
class Rules:
    """One platform's protected paths. Keys are normalised (:meth:`key`); values are TEXT keys."""

    system: System
    exact: Mapping[str, str] = field(default_factory=dict)  # the path itself
    tree: Mapping[str, str] = field(default_factory=dict)  # the path and everything below it
    allow: frozenset[str] = frozenset()  # below a tree rule: what is *inside* these may go
    children: Mapping[str, str] = field(default_factory=dict)  # each direct child (homes in /home)
    root_names: Mapping[str, str] = field(default_factory=dict)  # Windows: entries of any volume root
    root_trees: Mapping[str, str] = field(default_factory=dict)  # ... and everything below them
    names: tuple[tuple[re.Pattern[str], str], ...] = ()  # any component matching (and below it)

    @property
    def windows(self) -> bool:
        return self.system == "windows"

    def key(self, path: str) -> str:
        if self.windows:
            return ntpath.normcase(ntpath.normpath(_strip_verbatim(path)))
        return posixpath.normpath(path)

    def parent(self, key: str) -> str:
        return ntpath.dirname(key) if self.windows else posixpath.dirname(key)


def _keys(rules_system: System, paths: Iterable[str]) -> list[str]:
    probe = Rules(rules_system)
    return [probe.key(p) for p in paths if p]


_TRASH_NAMES = (
    (re.compile(r"^\.Trash(-\d+)?$"), "trash"),  # freedesktop $topdir/.Trash, .Trash-$uid; macOS ~/.Trash
    (re.compile(r"^\.Trashes$"), "trash"),  # macOS volumes
)


def platform_rules(
    system: System,
    *,
    home: str,
    env: Mapping[str, str] | None = None,
    app_dirs: Iterable[str] = (),
    program_dirs: Iterable[str] = (),
) -> Rules:
    """The built-in protected paths of ``system`` for a user with ``home``.

    ``env``: environment of a Windows system (``SystemRoot``, ``ProgramFiles`` …; defaults filled in)
    or of a POSIX one (``XDG_DATA_HOME``). ``app_dirs``: SpaceKnife's data, config and cache
    folders; ``program_dirs``: where the running program lives.
    """
    env = dict(env or {})
    exact: dict[str, str] = {}
    tree: dict[str, str] = {}
    allow: list[str] = []
    children: dict[str, str] = {}
    root_names: dict[str, str] = {}
    root_trees: dict[str, str] = {}
    names: tuple[tuple[re.Pattern[str], str], ...] = ()

    def put(table: dict[str, str], reason: str, *paths: str) -> None:
        for k in _keys(system, paths):
            table.setdefault(k, reason)

    if system == "windows":
        drive = env.get("SystemDrive", "C:")
        sysroot = env.get("SystemRoot") or env.get("windir") or drive + "\\Windows"
        put(tree, "system", sysroot)
        allow.extend(_keys(system, [sysroot + "\\Temp"]))
        put(
            exact,
            "program_root",
            env.get("ProgramFiles", drive + "\\Program Files"),
            env.get("ProgramFiles(x86)", drive + "\\Program Files (x86)"),
            env.get("ProgramW6432", ""),
        )
        put(exact, "data_root", env.get("ProgramData", drive + "\\ProgramData"))
        users = ntpath.dirname(home) if home else drive + "\\Users"
        put(exact, "users_root", users)
        put(children, "home", users)
        put(exact, "home", home)
        put(exact, "profile", *(ntpath.join(home, *p) for p in _WIN_PROFILE))
        put(tree, "boot", *(drive + "\\" + name for name in ("Recovery", "Boot", "EFI", "$WinREAgent")))
        put(exact, "boot", drive + "\\bootmgr", drive + "\\BOOTNXT")
        root_names.update(dict.fromkeys(("pagefile.sys", "hiberfil.sys", "swapfile.sys"), "system_file"))
        root_trees.update({"$recycle.bin": "trash", "system volume information": "system"})
    else:
        if system == "linux":
            put(tree, "system", *_LINUX_SYSTEM)
            put(tree, "package_db", "/var/lib/dpkg", "/var/lib/rpm", "/var/lib/dnf", "/var/lib/pacman")
            allow.extend(_keys(system, ["/run/media", "/run/user", "/run/mount"]))
            put(exact, "shared_root", *_LINUX_SHARED)
            put(exact, "users_root", "/home")
            put(children, "home", "/home")
            put(exact, "home", "/root")
        else:
            put(tree, "system", *_MAC_SYSTEM)
            put(exact, "program_root", "/Applications", "/System/Applications")
            put(exact, "shared_root", *_MAC_SHARED)
            put(exact, "users_root", "/Users")
            put(children, "home", "/Users")
            put(exact, "profile", *(posixpath.join(home, *p) for p in _MAC_PROFILE))
        put(exact, "home", home)
        put(exact, "profile", *(posixpath.join(home, *p) for p in _POSIX_PROFILE))
        xdg = env.get("XDG_DATA_HOME", "")
        data_home = xdg if xdg.startswith("/") else posixpath.join(home, ".local", "share")
        put(
            tree,
            "trash",
            posixpath.join(data_home, "Trash"),
            posixpath.join(home, ".local", "share", "Trash"),
        )
        names = _TRASH_NAMES
    put(tree, "app", *app_dirs)
    put(tree, "program", *program_dirs)
    return Rules(
        system=system,
        exact=exact,
        tree=tree,
        allow=frozenset(allow),
        children=children,
        root_names=root_names,
        root_trees=root_trees,
        names=names,
    )


_WIN_PROFILE = (("AppData",), ("AppData", "Local"), ("AppData", "LocalLow"), ("AppData", "Roaming"))
_POSIX_PROFILE = ((".config",), (".local",), (".local", "share"), (".local", "state"))
_MAC_PROFILE = (("Library",), ("Library", "Application Support"), ("Library", "Mobile Documents"))
_LINUX_SYSTEM = (
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/proc",
    "/run",
    "/sbin",
    "/sys",
    "/usr",
    "/efi",
)
_LINUX_SHARED = (
    "/var",
    "/var/lib",
    "/var/log",
    "/var/cache",
    "/var/tmp",
    "/tmp",
    "/opt",
    "/srv",
    "/mnt",
    "/media",
    "/snap",
    "/lost+found",
)
_MAC_SYSTEM = (
    "/System",
    "/bin",
    "/sbin",
    "/usr",
    "/dev",
    "/etc",
    "/private/etc",
    "/private/var/db",
    "/private/var/vm",
    "/Library/Apple",
    "/cores",
)
_MAC_SHARED = ("/Library", "/Volumes", "/private", "/private/var", "/private/tmp", "/var", "/tmp", "/opt")


def match_rules(path: str, rules: Rules) -> str | None:
    """Why ``path`` is protected by ``rules`` (a TEXT key), or None. Pure: never touches the disk."""
    key = rules.key(path)
    if rules.windows:
        _, rest = ntpath.splitdrive(key)
        if not rest.strip("\\"):
            return "root"
        parts = [p for p in rest.split("\\") if p]
    else:
        if key in ("/", "//"):
            return "root"
        parts = [p for p in key.split("/") if p]
    hit = rules.exact.get(key)
    if hit is not None:
        return hit
    parent = rules.parent(key)
    hit = rules.children.get(parent)
    if hit is not None:
        return hit
    if rules.windows:
        if len(parts) == 1 and parts[0] in rules.root_names:
            return rules.root_names[parts[0]]
        if parts[0] in rules.root_trees:
            return rules.root_trees[parts[0]]
    for pattern, reason in rules.names:
        if any(pattern.match(part) for part in parts):
            return reason
    # the nearest enclosing tree rule, unless an allowed folder lies between it and the path
    node = key
    allowed = False
    while True:
        if node != key and node in rules.allow:
            allowed = True
        hit = rules.tree.get(node)
        if hit is not None:
            return None if allowed else hit
        up = rules.parent(node)
        if up == node:
            return None
        node = up


# --------------------------------------------------------------------------- this system


@lru_cache(maxsize=8)
def _app_dirs(home_override: str | None) -> tuple[str, ...]:
    """SpaceKnife's own folders (they move only with ``SPACEKNIFE_HOME``, the cache key)."""
    return tuple(str(d) for d in (app_paths.data_dir(), app_paths.config_dir(), app_paths.cache_dir()))


@lru_cache(maxsize=1)
def _program_dirs() -> tuple[str, ...]:
    dirs = {sys.prefix, sys.base_prefix, os.path.dirname(os.path.abspath(sys.executable))}
    if getattr(sys, "frozen", False):
        # the installed app: <install>/resources/backend/spaceknife-backend → <install>
        dirs.add(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(sys.executable)))))
    return tuple(sorted(d for d in dirs if d))


_ENV_KEYS = (
    "SystemDrive",
    "SystemRoot",
    "windir",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    "XDG_DATA_HOME",
)


@lru_cache(maxsize=8)
def _rules_cached(
    home: str, env: tuple[tuple[str, str], ...], app: tuple[str, ...], program: tuple[str, ...]
) -> Rules:
    return platform_rules(_this_system(), home=home, env=dict(env), app_dirs=app, program_dirs=program)


def current_rules() -> Rules:
    """The rules of this system and user (rebuilt when the environment or app folders change)."""
    env = tuple((k, v) for k in _ENV_KEYS if (v := os.environ.get(k)))
    app = _app_dirs(os.environ.get("SPACEKNIFE_HOME"))
    return _rules_cached(os.path.expanduser("~"), env, app, _program_dirs())


_REAL_TTL = 2.0  # seconds a resolved parent folder is reused (one basket add / one execution)
_REAL_MAX = 1024
_real_parents: dict[str, tuple[float, str]] = {}


def _real_parent(parent: str) -> str:
    now = time.monotonic()
    hit = _real_parents.get(parent)
    if hit is not None and now - hit[0] < _REAL_TTL:
        return hit[1]
    real = os.path.realpath(parent)
    if len(_real_parents) >= _REAL_MAX:
        _real_parents.clear()
    _real_parents[parent] = (now, real)
    return real


def _resolved(path: str) -> str:
    """``path`` with its parent folders resolved (links followed), the last component kept."""
    parent, name = os.path.split(os.path.abspath(path))
    try:
        return os.path.join(_real_parent(parent), name) if name else os.path.realpath(parent)
    except (OSError, ValueError):
        return path


def _is_mount(path: str) -> bool:
    """A mount point (only folders can be: files skip the costly volume lookup)."""
    try:
        if not stat.S_ISDIR(os.lstat(path).st_mode):
            return False
        return os.path.ismount(path)
    except (OSError, ValueError):
        return False


def protected_reason(path: str) -> str | None:
    """Why ``path`` must not be deleted (for the user, in the current language), or None when it may be.

    Volume roots and mount points, system folders, program and data roots, user homes themselves,
    trash folders and SpaceKnife's own folders. Also checks the path with its parent folders
    resolved, so a link into ``/usr`` or ``C:\\Windows`` does not open a way around the list.
    """
    rules = current_rules()
    reason = match_rules(path, rules)
    if reason is None:
        real = _resolved(path)
        if rules.key(real) != rules.key(path):
            reason = match_rules(real, rules)
    if reason is None and _is_mount(path):
        reason = "mount"
    return TEXT[reason] if reason is not None else None


__all__ = [
    "TEXT",
    "PathForms",
    "Rules",
    "current_rules",
    "match_rules",
    "path_forms",
    "platform_rules",
    "protected_reason",
]

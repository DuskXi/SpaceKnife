"""Finding plugins, hashing their folders and remembering what the user confirmed.

A plugin is a folder named by its id with a ``plugin.toml``. The user confirms it once with its
permissions; the trust file records the folder's hash, and a plugin whose files changed is
``changed`` until the user confirms again. Built-in plugins are always on and not pinned (they
ship signed with the app); development links (developer mode) are not pinned either.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import threading
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from loguru import logger
from pydantic import BaseModel, ConfigDict, Field, ValidationError

from spaceknife_protocol.plugins import PluginState
from spaceknife_sdk.errors import Issue, ManifestError
from spaceknife_sdk.manifest import MANIFEST, PLUGIN_API, Manifest, load_manifest
from spaceknife_sdk.plugin import Description

from . import paths as ppaths

MAX_FILES = 2000
MAX_BYTES = 64 << 20
SKIP_DIRS = frozenset(
    {"__pycache__", ".git", ".hg", ".svn", "node_modules", ".venv", "dist", ".pytest_cache"}
)
SKIP_FILES = frozenset({".DS_Store", "Thumbs.db", "desktop.ini"})
SKIP_SUFFIXES = (".pyc", ".pyo")


class TooBig(ValueError):
    pass


def plugin_files(folder: Path) -> list[Path]:
    """The files that make the plugin (what the hash covers and installs copy), sorted."""
    out: list[Path] = []
    for root, dirs, files in os.walk(folder):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS and not d.startswith(".incoming"))
        for name in sorted(files):
            if name in SKIP_FILES or name.endswith(SKIP_SUFFIXES):
                continue
            p = Path(root) / name
            if p.is_symlink():
                raise ValueError(f"{p.relative_to(folder)}: symbolic links are not allowed in a plugin")
            out.append(p)
            if len(out) > MAX_FILES:
                raise TooBig(f"more than {MAX_FILES} files")
    return sorted(out, key=lambda p: p.relative_to(folder).as_posix())


def _file_hash(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        while chunk := fh.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def hash_folder(folder: Path, cache: dict[str, Any] | None = None) -> str:
    """sha256 over ``relative path NUL size NUL sha256(content)`` of every file, in path order.

    ``cache`` maps a relative path to ``[size, mtime_ns, sha256]`` and is updated in place, so only
    changed files are read again (the start of the app compares sizes and times first)."""
    total = 0
    outer = hashlib.sha256()
    seen: set[str] = set()
    for p in plugin_files(folder):
        rel = p.relative_to(folder).as_posix()
        st = p.stat()
        total += st.st_size
        if total > MAX_BYTES:
            raise TooBig(f"more than {MAX_BYTES >> 20} MiB")
        hit = cache.get(rel) if cache is not None else None
        if hit is not None and hit[0] == st.st_size and hit[1] == st.st_mtime_ns:
            digest = str(hit[2])
        else:
            digest = _file_hash(p)
            if cache is not None:
                cache[rel] = [st.st_size, st.st_mtime_ns, digest]
        seen.add(rel)
        outer.update(f"{rel}\0{st.st_size}\0{digest}\n".encode())
    if cache is not None:
        for gone in set(cache) - seen:
            cache.pop(gone, None)
    return "sha256:" + outer.hexdigest()


# --------------------------------------------------------------------------- trust


class TrustEntry(BaseModel):
    model_config = ConfigDict(extra="ignore")

    enabled: bool = False
    version: str = ""
    hash: str = ""
    granted: list[str] = Field(default_factory=list)
    net_hosts: list[str] = Field(default_factory=list)
    exec: list[str] = Field(default_factory=list)
    confirmed_at: str = ""


class TrustFile(BaseModel):
    model_config = ConfigDict(extra="ignore")

    version: int = 1
    plugins: dict[str, TrustEntry] = Field(default_factory=dict)
    dev: list[str] = Field(default_factory=list)


class TrustStore:
    """``config_dir()/plugins.json``: which plugins the user enabled, with what hash and grants."""

    def __init__(self, file: Path | None = None) -> None:
        self.file = file or ppaths.trust_file()
        self._lock = threading.Lock()
        self.doc = self._read()

    def _read(self) -> TrustFile:
        try:
            return TrustFile.model_validate(json.loads(self.file.read_text(encoding="utf-8")))
        except FileNotFoundError:
            return TrustFile()
        except (OSError, ValueError, ValidationError) as exc:
            logger.warning("plugin trust file {} is unreadable, starting empty: {}", self.file, exc)
            return TrustFile()

    def save(self) -> None:
        with self._lock:
            self.file.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.file.with_suffix(".tmp")
            tmp.write_text(
                json.dumps(self.doc.model_dump(mode="json"), ensure_ascii=False, indent=1), encoding="utf-8"
            )
            os.replace(tmp, self.file)

    def get(self, plugin: str) -> TrustEntry | None:
        return self.doc.plugins.get(plugin)

    def confirm(self, plugin: str, manifest: Manifest, hash: str, granted: list[str]) -> TrustEntry:
        entry = TrustEntry(
            enabled=True,
            version=manifest.version,
            hash=hash,
            granted=list(granted),
            net_hosts=list(manifest.net_hosts),
            exec=list(manifest.exec),
            confirmed_at=datetime.now(UTC).isoformat(timespec="seconds"),
        )
        self.doc.plugins[plugin] = entry
        self.save()
        return entry

    def set_enabled(self, plugin: str, enabled: bool) -> None:
        entry = self.doc.plugins.get(plugin)
        if entry is not None:
            entry.enabled = enabled
            self.save()

    def forget(self, plugin: str) -> None:
        if self.doc.plugins.pop(plugin, None) is not None:
            self.save()

    def link(self, folder: Path) -> None:
        p = str(folder.resolve())
        if p not in self.doc.dev:
            self.doc.dev.append(p)
            self.save()

    def unlink(self, folder: Path) -> None:
        p = str(folder.resolve())
        if p in self.doc.dev:
            self.doc.dev.remove(p)
            self.save()


# --------------------------------------------------------------------------- records


@dataclass(slots=True)
class PluginRecord:
    """One plugin the host found, whatever its state."""

    id: str
    folder: Path
    manifest: Manifest | None
    builtin: bool = False
    dev: bool = False
    hash: str = ""
    issues: list[Issue] = field(default_factory=list)
    state: PluginState = "needs_confirm"
    description: Description | None = None
    granted: frozenset[str] = frozenset()
    runtime: Any = None  # PluginRuntime once started
    states: dict[str, dict[str, Any]] = field(default_factory=dict)  # shared states' latest values
    crashes: list[float] = field(default_factory=list)  # times of recent crashes
    restarts: int = 0
    last_used: float = 0.0
    keys: list[str] = field(default_factory=list)

    @property
    def name(self) -> str:
        from ..i18n import language

        if self.manifest is None:
            return self.id
        return self.manifest.name.get(language())

    @property
    def requested(self) -> list[str]:
        return list(self.manifest.permissions) if self.manifest is not None else []

    @property
    def has_code(self) -> bool:
        return self.manifest is not None and self.manifest.entry is not None


def _hash_cache(plugin: str) -> tuple[Path, dict[str, Any]]:
    path = ppaths.cache_dir(plugin) / "hash.json"
    try:
        return path, dict(json.loads(path.read_text(encoding="utf-8")))
    except (OSError, ValueError):
        return path, {}


def read_record(
    folder: Path, *, builtin: bool = False, dev: bool = False, hash_it: bool = True
) -> PluginRecord:
    """A plugin folder as a record: its manifest (or what is wrong with it) and its hash."""
    try:
        manifest = load_manifest(folder)
    except ManifestError as exc:
        return PluginRecord(
            id=folder.name,
            folder=folder,
            manifest=None,
            builtin=builtin,
            dev=dev,
            issues=exc.issues,
            state="error",
        )
    rec = PluginRecord(id=manifest.id, folder=folder, manifest=manifest, builtin=builtin, dev=dev)
    if manifest.builtin != builtin:
        rec.issues.append(
            Issue("reserved_id", "builtin.* ids are for the plugins shipped with the app", file=MANIFEST)
        )
        rec.state = "error"
        return rec
    if not dev and not builtin and folder.name != manifest.id:
        rec.issues.append(
            Issue(
                "folder_name",
                f"the folder is named {folder.name!r}, the plugin id is {manifest.id!r}",
                file=MANIFEST,
            )
        )
        rec.state = "error"
        return rec
    if manifest.api != PLUGIN_API:
        rec.issues.append(
            Issue("api", f"written for plugin API {manifest.api}, this app has {PLUGIN_API}", file=MANIFEST)
        )
        rec.state = "incompatible"
        return rec
    if not _host_ok(manifest.requires.host):
        rec.issues.append(Issue("host", f"needs SpaceKnife {manifest.requires.host}", file=MANIFEST))
        rec.state = "incompatible"
        return rec
    if hash_it and not builtin and not dev:
        path, cache = _hash_cache(manifest.id)
        try:
            rec.hash = hash_folder(folder, cache)
        except (TooBig, ValueError, OSError) as exc:
            rec.issues.append(Issue("folder", str(exc)))
            rec.state = "error"
            return rec
        with contextlib.suppress(OSError):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(cache), encoding="utf-8")
    return rec


def _version_tuple(text: str) -> tuple[int, ...]:
    out = []
    for part in text.split("."):
        digits = "".join(ch for ch in part if ch.isdigit())
        out.append(int(digits) if digits else 0)
    return tuple(out)


def _host_ok(spec: str) -> bool:
    """``>=0.2``, ``<1``, ``>=0.2,<1`` against the app version; empty: any."""
    from .. import __version__

    have = _version_tuple(__version__)
    for clause in (c.strip() for c in spec.split(",") if c.strip()):
        for op in (">=", "<=", "==", ">", "<"):
            if clause.startswith(op):
                want = _version_tuple(clause[len(op) :].strip())
                n = max(len(have), len(want))
                a, b = have + (0,) * (n - len(have)), want + (0,) * (n - len(want))
                ok = {">=": a >= b, "<=": a <= b, "==": a == b, ">": a > b, "<": a < b}[op]
                if not ok:
                    return False
                break
        else:
            return False
    return True


def discover(
    trust: TrustStore, *, dev_mode: bool, extra_dirs: list[Path] | None = None
) -> list[PluginRecord]:
    """Every plugin: the built-in ones, the installed ones, and (developer mode) linked folders."""
    out: dict[str, PluginRecord] = {}
    bdir = ppaths.builtin_dir()
    if bdir.is_dir():
        for folder in sorted(p for p in bdir.iterdir() if (p / MANIFEST).is_file()):
            rec = read_record(folder, builtin=True)
            out.setdefault(rec.id, rec)
    for base in [ppaths.plugins_dir(), *(extra_dirs or [])]:
        if not base.is_dir():
            continue
        for folder in sorted(p for p in base.iterdir() if p.is_dir() and not p.name.startswith(".")):
            if not (folder / MANIFEST).is_file():
                continue
            rec = read_record(folder)
            if rec.id in out:
                rec.issues.append(Issue("duplicate", f"another plugin already has the id {rec.id!r}"))
                rec.state = "error"
                rec.id = f"{rec.id}#{folder.name}"
            out[rec.id] = rec
    if dev_mode:
        for link in trust.doc.dev:
            folder = Path(link)
            if not (folder / MANIFEST).is_file():
                continue
            rec = read_record(folder, dev=True)
            out[rec.id] = rec  # a development link wins over an installed copy
    return list(out.values())


__all__ = [
    "MAX_BYTES",
    "MAX_FILES",
    "PluginRecord",
    "TooBig",
    "TrustEntry",
    "TrustStore",
    "discover",
    "hash_folder",
    "plugin_files",
    "read_record",
]

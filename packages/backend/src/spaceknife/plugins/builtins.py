"""The plugins shipped with the app (``plugins/builtin/<id>/``), registered once per process.

A built-in plugin is a plugin folder like any other — ``plugin.toml``, rule files, templates, a
``plugin.py`` that declares with the SDK — loaded the way the host loads every plugin (the SDK's
loader and :meth:`~spaceknife_sdk.Plugin.describe`, then :func:`.adapters.build`), and registered
under its own id. It is part of the app, so its contributions exist as soon as anything needs them,
with or without a server (the command line exports without one): :func:`ensure` runs on first use.
Built-in plugins cannot be disabled, are not pinned by hash and run in this process.

What a built-in plugin may add: the declarative kinds, and the code kinds that run without a
session — export formats, called directly on the export's thread with a tree view of the store
(:mod:`.local`). The host refuses the others for built-in plugins (they would need a runtime).
"""

from __future__ import annotations

import threading
from dataclasses import dataclass
from pathlib import Path

from loguru import logger

from spaceknife_sdk.errors import Issue, ManifestError
from spaceknife_sdk.manifest import MANIFEST
from spaceknife_sdk.plugin import Description, Plugin

from . import paths as ppaths

# kinds a built-in plugin may contribute (no session, no runtime: see the module docstring)
STATIC_KINDS = frozenset(
    {
        "export_format",
        "export_template",
        "insight_rules",
        "file_class",
        "folder_rule",
        "filter_preset",
        "icon",
    }
)


@dataclass(slots=True)
class Loaded:
    id: str
    folder: Path
    plugin: Plugin
    description: Description


_lock = threading.RLock()
_loading = False
_loaded: dict[str, Loaded] | None = None
_failed: dict[str, list[Issue]] = {}


def folders() -> list[Path]:
    base = ppaths.builtin_dir()
    if not base.is_dir():
        return []
    return sorted(p for p in base.iterdir() if (p / MANIFEST).is_file())


def _load(folder: Path) -> Loaded:
    from spaceknife_sdk._runtime import load_plugin

    from ..extensions import EXTENSIONS, points
    from .adapters import AdapterError, build
    from .store import read_record

    points.install()
    rec = read_record(folder, builtin=True)
    if rec.manifest is None:
        raise ManifestError(rec.issues)
    if rec.issues:
        raise ManifestError(rec.issues)
    plugin = load_plugin(folder, rec.manifest)
    desc = plugin.describe()
    other = sorted({str(c.get("kind")) for c in desc.contributions} - STATIC_KINDS)
    if other:
        raise ManifestError(
            [Issue("builtin_kind", f"a built-in plugin cannot add {', '.join(other)} (no runtime)")]
        )
    rec.description = desc
    try:
        contributions = build(None, rec, desc, local=plugin)
    except AdapterError as exc:
        raise ManifestError(exc.issues) from None
    EXTENSIONS.register_many(contributions)
    return Loaded(rec.id, folder, plugin, desc)


def ensure() -> dict[str, Loaded]:
    """Register every built-in plugin (once; later calls return what was loaded)."""
    global _loaded, _loading
    if _loaded is not None:
        return _loaded
    with _lock:
        if _loaded is not None:
            return _loaded
        if _loading:  # re-entered while registering (a point reloading what it serves)
            return {}
        _loading = True
        try:
            out: dict[str, Loaded] = {}
            for folder in folders():
                try:
                    item = _load(folder)
                except (ManifestError, ValueError, OSError, ImportError) as exc:
                    issues = exc.issues if isinstance(exc, ManifestError) else [Issue("load", str(exc))]
                    _failed[folder.name] = list(issues)
                    logger.error(
                        "built-in plugin {} cannot be loaded: {}",
                        folder.name,
                        "; ".join(i.text() for i in issues),
                    )
                    continue
                out[item.id] = item
            _loaded = out
        finally:
            _loading = False
        return out


def loaded() -> bool:
    """The shipped plugins are registered (or failed for good)."""
    return _loaded is not None


def loading() -> bool:
    """True while :func:`ensure` registers (points that reload their subsystem wait for the end)."""
    return _loading


def get(plugin: str) -> Loaded | None:
    return ensure().get(plugin)


def failures() -> dict[str, list[Issue]]:
    ensure()
    return dict(_failed)


__all__ = ["STATIC_KINDS", "Loaded", "ensure", "failures", "folders", "get", "loaded", "loading"]

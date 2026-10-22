"""Where plugins live: ``data_dir()/plugins/<id>/`` (portable builds too), their data and caches."""

from __future__ import annotations

from pathlib import Path

from .. import paths


def plugins_dir() -> Path:
    return paths.data_dir() / "plugins"


def builtin_dir() -> Path:
    """The plugins shipped with the app (package data, same layout)."""
    return Path(__file__).resolve().parent / "builtin"


def data_dir(plugin: str) -> Path:
    """The plugin's own writable folder (``ctx.storage``)."""
    return paths.data_dir() / "plugin-data" / plugin


def cache_dir(plugin: str) -> Path:
    """What the host keeps about a plugin: its cached description, the hash cache (not hashed)."""
    return paths.cache_dir() / "plugin-cache" / plugin


def trust_file() -> Path:
    return paths.config_dir() / "plugins.json"


def runtime_dir() -> Path:
    """Images that fall back to files, sockets (0700 below the cache)."""
    return paths.cache_dir() / "plugin-run"


__all__ = ["builtin_dir", "cache_dir", "data_dir", "plugins_dir", "runtime_dir", "trust_file"]

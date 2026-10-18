"""Per-user application directories.

``SPACEKNIFE_HOME`` overrides everything (tests and isolated milestone demos use it so they never
touch the real user data).
"""

from __future__ import annotations

import os
from pathlib import Path

from platformdirs import PlatformDirs

APP_NAME = "SpaceKnife"


def _home_override() -> Path | None:
    value = os.environ.get("SPACEKNIFE_HOME")
    return Path(value).expanduser().resolve() if value else None


def data_dir() -> Path:
    override = _home_override()
    return override / "data" if override else Path(PlatformDirs(APP_NAME, appauthor=False).user_data_dir)


def config_dir() -> Path:
    override = _home_override()
    return override / "config" if override else Path(PlatformDirs(APP_NAME, appauthor=False).user_config_dir)


def cache_dir() -> Path:
    override = _home_override()
    return override / "cache" if override else Path(PlatformDirs(APP_NAME, appauthor=False).user_cache_dir)


def snapshots_dir() -> Path:
    return data_dir() / "snapshots"


def logs_dir() -> Path:
    return data_dir() / "logs"

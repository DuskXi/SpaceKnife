"""The visualizations this server offers. Built-ins register like plugins will: through :func:`register`."""

from __future__ import annotations

import re
import threading
from collections.abc import Iterable

from .api import ENGINE_API, Visualization

_ID = re.compile(r"[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*")  # protocol VisualId
_ID_MAX = 48
_LAYOUT = re.compile(r"[a-z][a-z0-9]*\.v\d+")

_lock = threading.Lock()
_visuals: dict[str, Visualization] = {}
_loaded = False


def register(v: Visualization) -> Visualization:
    """Add ``v``; ValueError for a malformed or taken id, another engine API or a malformed layout."""
    if len(v.id) > _ID_MAX or not _ID.fullmatch(v.id):
        raise ValueError(f"visualization id {v.id!r} is not a lowercase word list of up to {_ID_MAX} chars")
    if v.api != ENGINE_API:
        raise ValueError(f"visualization {v.id!r} needs engine API {v.api}, this server has {ENGINE_API}")
    if not _LAYOUT.fullmatch(v.layout):
        raise ValueError(f"visualization {v.id!r} has a malformed layout name {v.layout!r}")
    with _lock:
        if v.id in _visuals:
            raise ValueError(f"visualization {v.id!r} is already registered")
        _visuals[v.id] = v
    return v


def unregister(id: str) -> None:
    """Remove a visualization (tests; plugins being turned off). Unknown ids are ignored."""
    with _lock:
        _visuals.pop(id, None)


def ensure_loaded() -> None:
    """Register the built-in visualizations (once)."""
    global _loaded
    if not _loaded:
        from . import builtins  # noqa: F401  (registers on import)

        _loaded = True


def get(id: str) -> Visualization:
    """The visualization ``id``; KeyError when there is none."""
    ensure_loaded()
    return _visuals[id]


def all_visuals() -> list[Visualization]:
    """Every registered visualization, by order then id."""
    ensure_loaded()
    with _lock:
        found = list(_visuals.values())
    return sorted(found, key=lambda v: (v.order, v.id))


def is_available(v: Visualization, caps: Iterable[str]) -> bool:
    """Whether a session with capabilities ``caps`` can show ``v``."""
    return not v.capability or v.capability in set(caps)


def available(caps: Iterable[str]) -> list[Visualization]:
    """The visualizations a session with capabilities ``caps`` can show, by order then id."""
    have = set(caps)
    return [v for v in all_visuals() if not v.capability or v.capability in have]

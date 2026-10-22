"""The unified extension registry (docs/plugins.md): built-in features and plugins register the same way."""

from __future__ import annotations

from spaceknife_protocol import BASE_CAPABILITIES

from .registry import BUILTIN, EXTENSIONS, BasePoint, Contribution, ExtensionError, Extensions, Point


def capabilities() -> tuple[str, ...]:
    """What this server offers: the host's own capabilities and those of its visualizations' layouts
    (a plugin never gets a capability of its own)."""
    from ..layout import registry as visuals

    out = list(BASE_CAPABILITIES)
    for v in visuals.all_visuals():
        if v.capability and v.capability not in out:
            out.append(v.capability)
    return tuple(out)


__all__ = [
    "BUILTIN",
    "EXTENSIONS",
    "BasePoint",
    "Contribution",
    "ExtensionError",
    "Extensions",
    "Point",
    "capabilities",
]

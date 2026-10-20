"""The built-in visualizations: importing their modules registers them (see registry.ensure_loaded)."""

from __future__ import annotations

from . import engine, flame, flame_polar  # noqa: F401  (treemap, flame graph, polar flame graph)

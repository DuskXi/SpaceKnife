"""Views: per-tab / per-pane state over a shared scan (navigation, detail, filter, tags, colours)."""

from __future__ import annotations

from .state import Frame, View, ViewSettings, Zoom
from .tags import TagStore

__all__ = ["Frame", "TagStore", "View", "ViewSettings", "Zoom"]

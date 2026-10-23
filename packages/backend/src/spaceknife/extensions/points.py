"""The extension points that wire contributions into subsystems with registries of their own.

Kinds not listed here are read where they are used (``EXTENSIONS.of(kind)``): commands, keys,
settings, reports, fields, panels, colour modes, export formats and templates, node sets, presets,
cleanups, sources, watchers, annotators and hooks.
"""

from __future__ import annotations

import threading

from spaceknife_protocol.filter import FilterClassInfo

from ..layout import registry as visuals
from ..layout.api import ENGINE_API, Visualization
from .registry import EXTENSIONS, BasePoint, Contribution

_lock = threading.Lock()


class VisualizationPoint(BasePoint):
    api = ENGINE_API

    def __init__(self) -> None:
        super().__init__("visualization")

    def check(self, c: Contribution) -> list[str]:
        if not isinstance(c.impl, Visualization):
            return ["no engine"]
        try:
            visuals.get(c.impl.id)
        except KeyError:
            return []
        return [f"visualization {c.impl.id!r} exists already"]

    def add(self, c: Contribution) -> None:
        visuals.register(c.impl)

    def remove(self, c: Contribution) -> None:
        visuals.unregister(c.impl.id if isinstance(c.impl, Visualization) else c.id)


def _rule_sources(
    adding: Contribution | None = None, removing: Contribution | None = None
) -> list[tuple[str, str]]:
    """Rule files in override order: the shipped plugins' (the built-in rules), then by plugin id."""
    items = [c for c in EXTENSIONS.of("insight_rules") if removing is None or c.id != removing.id]
    if adding is not None:
        items.append(adding)
    ordered = sorted(items, key=lambda c: (not c.shipped, c.owner, c.id))
    return [c.impl for c in ordered if c.impl]


class InsightRulesPoint(BasePoint):
    """Rule files of plugins: the shipped ``builtin.insights`` → plugins (by plugin id) → the
    user's folder."""

    def __init__(self) -> None:
        super().__init__("insight_rules")

    def check(self, c: Contribution) -> list[str]:
        from ..insights.rules import _parse_file

        source, text = c.impl
        _rules, problems = _parse_file(text, source)
        return [p.message for p in problems if not p.rule]  # a broken file (a broken rule: logged, skipped)

    def _reload(self, adding: Contribution | None = None, removing: Contribution | None = None) -> None:
        from ..insights.rules import set_plugin_sources

        set_plugin_sources(_rule_sources(adding, removing))

    def add(self, c: Contribution) -> None:
        self._reload(adding=c)

    def remove(self, c: Contribution) -> None:
        self._reload(removing=c)


class FileClassPoint(BasePoint):
    def __init__(self) -> None:
        super().__init__("file_class")

    def _apply(self, extra: FilterClassInfo | None = None, drop: str | None = None) -> list[str]:
        from .. import classes

        items = [
            c.impl
            for c in EXTENSIONS.of("file_class")
            if isinstance(c.impl, FilterClassInfo) and c.id != drop
        ]
        if extra is not None:
            items.append(extra)
        return classes.set_plugin_classes(tuple(items))

    def check(self, c: Contribution) -> list[str]:
        from .. import classes

        if not isinstance(c.impl, FilterClassInfo):
            return ["not a class"]
        _table, skipped = classes.merged(classes.active(), (c.impl,))
        return (
            [f"class {c.impl.id!r} does not fit: its name is taken or the table is full"] if skipped else []
        )

    def add(self, c: Contribution) -> None:
        self._apply(extra=c.impl)

    def remove(self, c: Contribution) -> None:
        self._apply(drop=c.id)


class FolderRulePoint(BasePoint):
    def __init__(self) -> None:
        super().__init__("folder_rule")

    def _apply(self, extra: Contribution | None = None, drop: str | None = None) -> None:
        from .. import pathclass

        items = [c.impl for c in EXTENSIONS.of("folder_rule") if c.id != drop]
        if extra is not None:
            items.append(extra.impl)
        pathclass.set_plugin_rules(items)

    def add(self, c: Contribution) -> None:
        self._apply(extra=c)

    def remove(self, c: Contribution) -> None:
        self._apply(drop=c.id)


_installed = False


def install() -> None:
    """Register the points (once per process)."""
    global _installed
    with _lock:
        if _installed:
            return
        for p in (VisualizationPoint(), InsightRulesPoint(), FileClassPoint(), FolderRulePoint()):
            EXTENSIONS.point(p)
        _installed = True


__all__ = ["FileClassPoint", "FolderRulePoint", "InsightRulesPoint", "VisualizationPoint", "install"]

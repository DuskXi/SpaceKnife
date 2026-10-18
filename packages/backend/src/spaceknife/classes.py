"""File-class lookups over the scanned extension table (``:class:`` filter, treemap colours).

The class definitions themselves are shared data in :mod:`spaceknife_protocol.classes`.
"""

from __future__ import annotations

import numpy as np

from spaceknife_protocol.classes import BUILTIN_CLASSES, OTHER
from spaceknife_protocol.filter import FilterClassInfo


class ClassTable:
    """Classes in display order (``other`` last), with per-extension membership."""

    def __init__(self, classes: tuple[FilterClassInfo, ...] = BUILTIN_CLASSES) -> None:
        self.classes = classes
        self.by_id = {c.id: c for c in classes}
        if OTHER not in self.by_id:
            raise ValueError("the class table needs an 'other' class")
        self._ext_classes: dict[str, list[str]] = {}
        for c in classes:
            for ext in c.extensions:
                owners = self._ext_classes.setdefault(ext.lower(), [])
                if c.id not in owners:
                    owners.append(c.id)
        names: dict[str, str] = {}
        for c in classes:
            for name in (c.id, c.name, *c.aliases):
                if names.setdefault(name.casefold(), c.id) != c.id:
                    raise ValueError(f"class name {name!r} is used by two classes")

    def classes_of(self, ext: str) -> list[str]:
        return self._ext_classes.get(ext.lower(), [])

    def color_class(self, ext: str) -> str:
        owners = self._ext_classes.get(ext.lower())
        return owners[0] if owners else OTHER

    def ext_mask(self, class_id: str, ext_table: list[str]) -> np.ndarray:
        """Boolean array over extension-table ids: the extensions that belong to ``class_id``.

        For ``other`` these are the extensions (including "no extension") that are in no class.
        """
        if class_id == OTHER:
            return np.fromiter((not self._ext_classes.get(e) for e in ext_table), bool, len(ext_table))
        return np.fromiter(
            (class_id in self._ext_classes.get(e, ()) for e in ext_table), bool, len(ext_table)
        )


DEFAULT_CLASSES = ClassTable()

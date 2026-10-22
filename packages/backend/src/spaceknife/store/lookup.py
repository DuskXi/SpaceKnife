"""Path → node for change events: per-folder name maps, built when a folder is first asked about.

:meth:`NodeStore.find` compares names one by one at every level — fine for a single lookup, slow
for a folder of 100,000 entries that sees a change per second. Here each folder visited gets a
name map, valid while the folder keeps the same child range (a relist gives it a new one). At most
``MAX_FOLDERS`` maps are kept (least recently used out first).

A name is looked up in its exact case first, and only then case-insensitively (``normcase``), and
then only when one entry has it: a folder can hold names that differ only in case (WinBtrfs, a
case-sensitive NTFS folder), and a change in ``A`` must not land in ``a`` (docs/native.md 2.2).
"""

from __future__ import annotations

import os
from collections import OrderedDict

import numpy as np

from .nodes import Flag, Kind, NodeStore, split_below

MAX_FOLDERS = 512
AMBIGUOUS = -1  # several entries have this name under normcase: none of them is meant


class PathIndex:
    def __init__(self, store: NodeStore) -> None:
        self.store = store
        # folder -> (child range, {name: first child}, {normcase(name): child or AMBIGUOUS})
        self._maps: OrderedDict[int, tuple[tuple[int, int], dict[str, int], dict[str, int]]] = OrderedDict()

    def locate(self, path: str) -> tuple[int | None, int | None]:
        """``(node, deepest)``: the node at ``path`` (None if the tree has no such entry) and the
        deepest node on the way there that does exist (None when ``path`` is outside every root)."""
        store = self.store
        for root in store.roots():
            if store.cols["flags"][root] & np.uint32(Flag.GONE):
                continue
            parts = split_below(path, store.name(root))
            if parts is None:
                continue
            node = root
            for part in parts:
                if store.cols["kind"][node] != Kind.DIR:
                    return None, node
                child = self._child(node, part)
                if child is None:
                    return None, node
                node = child
            return node, node
        return None, None

    def _child(self, folder: int, name: str) -> int | None:
        span = self.store.child_range(folder)
        hit = self._maps.get(folder)
        if hit is None or hit[0] != span:
            store = self.store
            exact: dict[str, int] = {}
            folded: dict[str, int] = {}
            for i in range(*span):
                own = store.name(i)
                exact.setdefault(own, i)
                key = os.path.normcase(own)
                folded[key] = AMBIGUOUS if key in folded else i
            hit = (span, exact, folded)
            self._maps[folder] = hit
            while len(self._maps) > MAX_FOLDERS:
                self._maps.popitem(last=False)
        else:
            self._maps.move_to_end(folder)
        found = hit[1].get(name)
        if found is None:
            found = hit[2].get(os.path.normcase(name))
        return None if found is None or found == AMBIGUOUS else found

    def forget(self) -> None:
        """Drop every map (the store was renumbered)."""
        self._maps.clear()


__all__ = ["AMBIGUOUS", "MAX_FOLDERS", "PathIndex"]

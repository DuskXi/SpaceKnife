"""Path → node for change events: per-folder name maps, built when a folder is first asked about.

:meth:`NodeStore.find` compares names one by one at every level — fine for a single lookup, slow
for a folder of 100,000 entries that sees a change per second. Here each folder visited gets a
``{normalised name: child id}`` map, valid while the folder keeps the same child range (a relist
gives it a new one). At most ``MAX_FOLDERS`` maps are kept (least recently used out first).
"""

from __future__ import annotations

import os
from collections import OrderedDict

import numpy as np

from .nodes import Flag, Kind, NodeStore

MAX_FOLDERS = 512


class PathIndex:
    def __init__(self, store: NodeStore) -> None:
        self.store = store
        self._maps: OrderedDict[int, tuple[tuple[int, int], dict[str, int]]] = OrderedDict()

    def locate(self, path: str) -> tuple[int | None, int | None]:
        """``(node, deepest)``: the node at ``path`` (None if the tree has no such entry) and the
        deepest node on the way there that does exist (None when ``path`` is outside every root)."""
        store = self.store
        target = os.path.normcase(os.path.normpath(path))
        for root in store.roots():
            if store.cols["flags"][root] & np.uint32(Flag.GONE):
                continue
            root_path = os.path.normcase(os.path.normpath(store.name(root)))
            if target == root_path:
                return root, root
            prefix = root_path if root_path.endswith(os.sep) else root_path + os.sep
            if not target.startswith(prefix):
                continue
            node = root
            for part in target[len(prefix) :].split(os.sep):
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
            names = {os.path.normcase(store.name(i)): i for i in range(*span)}
            hit = (span, names)
            self._maps[folder] = hit
            while len(self._maps) > MAX_FOLDERS:
                self._maps.popitem(last=False)
        else:
            self._maps.move_to_end(folder)
        return hit[1].get(name)

    def forget(self) -> None:
        """Drop every map (the store was renumbered)."""
        self._maps.clear()


__all__ = ["MAX_FOLDERS", "PathIndex"]

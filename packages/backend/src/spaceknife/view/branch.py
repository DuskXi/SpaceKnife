"""Top-level branch colours (the ``branch`` column): one palette slot per child of the zoom root.

A first-level block keeps the slot it got when it was first drawn, so a branch keeps its hue while a
running scan reorders the blocks; every block below it takes its hue. A new branch takes the slot
used least by the branches in view that differs from its drawn neighbours' slots (ties: the lower
slot). First-level aggregates and free / unknown space are neutral (0). The slots belong to one zoom
root and are forgotten when it changes; at most ``LIMIT`` are remembered, the least recently drawn
going first.
"""

from __future__ import annotations

from collections import OrderedDict
from collections.abc import Callable

import numpy as np

from ..layout.rows import KEY_NODE_MASK, PENDING_RECT, DrawList

SLOTS = 8  # visual.branch.1 … visual.branch.8
LIMIT = 4096  # branches remembered per view


class BranchHues:
    """The branch slots of one view, stable across its frames."""

    __slots__ = ("_root", "_slots")

    def __init__(self) -> None:
        self._root: int | None = None  # zoom key the slots belong to
        self._slots: OrderedDict[int, int] = OrderedDict()  # first-level key -> slot, least recent first

    def clear(self) -> None:
        self._root = None
        self._slots.clear()

    def slot_of(self, key: int) -> int:
        """The slot a first-level block (its key) got in the frames so far; 0 when it has none."""
        return self._slots.get(key, 0)

    def hues(self, root: int, draw: DrawList) -> np.ndarray:
        """Slot per row of ``draw`` (u1, 0 neutral), laid out below the zoom root keyed ``root``."""
        if root != self._root:
            self._slots.clear()
            self._root = root
        out = np.zeros(len(draw), dtype=np.uint8)
        if not len(draw):
            return out
        parent, depth = draw.parent, draw.depth
        top = np.flatnonzero((parent < 0) & (draw.kind <= PENDING_RECT))
        known = self._slots
        keys = draw.key[top].tolist()
        slots = [0] * len(keys)
        for i, k in enumerate(keys):
            s = known.get(k)
            if s is not None:
                known.move_to_end(k)
                slots[i] = s
        used = [0] * (SLOTS + 1)
        for s in slots:
            used[s] += 1
        n = len(keys)
        for i, k in enumerate(keys):
            if slots[i]:
                continue
            # neighbours on both sides; the list is a ring (the polar view's first and last
            # sectors meet at 12 o'clock), so the ends are neighbours of each other too
            near = {slots[i - 1] if n > 2 or i else 0, slots[(i + 1) % n] if n > 2 or i + 1 < n else 0}
            s = min((s for s in range(1, SLOTS + 1) if s not in near), key=lambda s: (used[s], s))
            slots[i] = s
            used[s] += 1
            known[k] = s
        while len(known) > LIMIT:
            known.popitem(last=False)
        out[top] = slots
        for d in range(2, int(depth.max()) + 1):  # parents come first: one level after the other
            rows = np.flatnonzero(depth == d)
            out[rows] = out[parent[rows]]
        return out

    def remap(self, new_ids: Callable[[np.ndarray], np.ndarray]) -> None:
        """Node ids changed (see ``View.remap``): the slots follow their branches, gone ones go."""
        root = self._root
        if root is None:
            return
        tag, node = root & ~KEY_NODE_MASK, root & KEY_NODE_MASK
        moved = new_ids(np.array([node, *self._slots], dtype=np.int64)).tolist()
        if moved[0] < 0:
            self.clear()
            return
        self._root = tag | int(moved[0])
        self._slots = OrderedDict(
            (int(m), s) for m, s in zip(moved[1:], self._slots.values(), strict=True) if m >= 0
        )


__all__ = ["LIMIT", "SLOTS", "BranchHues"]

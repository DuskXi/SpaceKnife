"""What every layout of one store shares: the area basis, colour classes and safe child reads."""

from __future__ import annotations

import numpy as np

from ..classes import ClassTable, active
from ..store.nodes import Flag, Kind, NodeStore


class Basis:
    """Per-store inputs of the layout engines; reusable across calls (and across engines)."""

    def __init__(self, store: NodeStore, classes: ClassTable | None = None) -> None:
        self.store = store
        self._fixed = classes  # None: follow the active (user-edited) table
        self._ext_cls = np.zeros(0, dtype=np.uint8)
        self._ext_key: tuple[int, int] = (-1, 0)
        self._sizes: tuple[int, int, np.ndarray] | None = None  # (store version, node count, sizes)

    @property
    def classes(self) -> ClassTable:
        return self._fixed if self._fixed is not None else active()

    @property
    def class_ids(self) -> list[str]:
        return self.classes.ids

    @property
    def other(self) -> int:
        return self.classes.other_index

    def ext_classes(self) -> np.ndarray:
        """Colour class index per extension-table id (again when the class table changed)."""
        table = self.store.ext_table
        classes = self.classes
        key = (id(classes), len(table))
        if key != self._ext_key:
            index = {cid: i for i, cid in enumerate(classes.ids)}
            other = classes.other_index
            self._ext_cls = np.fromiter(
                (index[classes.color_class(e)] if e else other for e in table), np.uint8, len(table)
            )
            self._ext_key = key
        return self._ext_cls

    def cls_of(self, ids: np.ndarray) -> np.ndarray:
        """Colour class index (u1) of some nodes."""
        out: np.ndarray = self.ext_classes()[self.store.cols["ext"][ids]]
        return out

    @staticmethod
    def effective_sizes(store: NodeStore) -> np.ndarray:
        """Area basis per node: subtree allocation for folders, own allocation for files."""
        n = len(store)
        c = store.cols
        is_dir = c["kind"][:n] == Kind.DIR
        dup = (c["flags"][:n] & np.uint32(Flag.HARDLINK_DUP)) != 0
        out: np.ndarray = np.where(is_dir, c["tot_alloc"][:n], np.where(dup, 0, c["alloc"][:n])).astype(
            np.int64
        )
        return out

    @staticmethod
    def effective_sizes_of(store: NodeStore, ids: np.ndarray) -> np.ndarray:
        """:meth:`effective_sizes` for some nodes only."""
        c = store.cols
        is_dir = c["kind"][ids] == Kind.DIR
        dup = (c["flags"][ids] & np.uint32(Flag.HARDLINK_DUP)) != 0
        out: np.ndarray = np.where(is_dir, c["tot_alloc"][ids], np.where(dup, 0, c["alloc"][ids])).astype(
            np.int64
        )
        return out

    def sizes(self) -> np.ndarray:
        """:meth:`effective_sizes`, cached until the store changes."""
        store = self.store
        cached = self._sizes
        if cached is None or cached[0] != store.version or cached[1] != len(store):
            cached = (store.version, len(store), self.effective_sizes(store))
            self._sizes = cached
        return cached[2]

    def sizes_of(self, ids: np.ndarray) -> np.ndarray:
        """:meth:`effective_sizes` of some nodes, without the whole-store array."""
        return self.effective_sizes_of(self.store, ids)

    def children(self, node: int, n: int) -> np.ndarray:
        """Child ids (i8) of ``node`` below node count ``n``; empty while the folder is not listed."""
        # a scan may list this folder while we read: count before start, a range never starts at
        # node 0 (else: not listed yet, see NodeStore.child_range), and never past the node count
        c = self.store.cols
        count = int(c["child_count"][node])
        start = int(c["child_start"][node]) if count > 0 else 0
        if count <= 0 or start < 1:
            return np.empty(0, dtype=np.int64)
        stop = min(start + count, n)
        return np.arange(start, max(start, stop), dtype=np.int64)

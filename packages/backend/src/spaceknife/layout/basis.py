"""What every layout of one store shares: the area basis, colour classes and safe child reads."""

from __future__ import annotations

from typing import Any

import numpy as np

from .. import pathclass
from ..classes import ClassTable, active, looks_like_stream
from ..store.nodes import Flag, Kind, NodeStore

NO_CLASS = 255
"""``cls`` of a row in no one class: a folder no folder class swallows, an aggregate of mixed
classes, "+N items". It paints like the Other class but never counts as it (the class hover highlight)."""


class LazySizes:
    """:meth:`Basis.sizes` without the whole-store array (8 B a node, worked out again after every
    change): ``sizes[ids]`` and ``len(sizes)`` compute only the nodes asked for. For the palette;
    ``len`` is the node count when it was made, like an array taken then."""

    __slots__ = ("basis", "n")

    def __init__(self, basis: Basis) -> None:
        self.basis = basis
        self.n = len(basis.store)

    def __len__(self) -> int:
        return self.n

    def __getitem__(self, ids: Any) -> Any:
        if isinstance(ids, int | np.integer):
            return int(self.basis.sizes_of(np.array([ids], dtype=np.int64))[0])
        return self.basis.sizes_of(np.asarray(ids, dtype=np.int64))


class Basis:
    """Per-store inputs of the layout engines; reusable across calls (and across engines)."""

    def __init__(self, store: NodeStore, classes: ClassTable | None = None) -> None:
        self.store = store
        self._fixed = classes  # None: follow the active (user-edited) table
        self._ext_cls = np.zeros(0, dtype=np.uint8)
        self._ext_key: tuple[int, int] = (-1, 0)
        self._splits: list[tuple[int, int, int, int]] = []  # (ext id, packet, recording, other)
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
        """Colour class index per extension-table id (again when the class table changed).

        Files of a size-split extension (``ts``) get theirs from :meth:`cls_of`, which also looks
        at the size.
        """
        table = self.store.ext_table
        classes = self.classes
        key = (id(classes), len(table))
        if key != self._ext_key:
            index = {cid: i for i, cid in enumerate(classes.ids)}
            other = classes.other_index
            self._ext_cls = np.fromiter(
                (index[classes.color_class(e)] if e else other for e in table), np.uint8, len(table)
            )
            self._splits = [
                (s.ext_id, s.packet, index[s.stream], index[s.rest[0]] if s.rest else other)
                for s in classes.splits(table)
            ]
            self._ext_key = key
        return self._ext_cls

    def cls_of(self, ids: np.ndarray) -> np.ndarray:
        """Colour class index (u1) of some nodes: by extension, under the folder classes
        (a game install's files are 「游戏」 whatever their extension, see :mod:`..pathclass`).

        A folder has a class only when a folder class swallows it (everything in it is that
        class); any other folder gets :data:`NO_CLASS`.
        """
        out = self.ext_cls_of(ids)
        col = pathclass.column(self.store, self.classes)[ids]
        if col.any():
            out = pathclass.resolve(col, out, self.other)
        dirs = self.store.cols["kind"][ids] == Kind.DIR
        if dirs.any():
            swallowed = (col > 0) & (col < pathclass.FALLBACK_BASE)
            out = np.where(dirs & ~swallowed, NO_CLASS, out).astype(np.uint8, copy=False)
        return out

    def ext_cls_of(self, ids: np.ndarray) -> np.ndarray:
        """Class index (u1) of some nodes by their extension alone (and size, for ``ts``)."""
        ext = self.store.cols["ext"][ids]
        out: np.ndarray = self.ext_classes()[ext]
        for ext_id, packet, stream, plain in self._splits:
            rows = np.flatnonzero(ext == ext_id)
            if rows.size:
                size = self.store.cols["size"][ids[rows]]
                out[rows] = np.where(looks_like_stream(size, packet), stream, plain)
        return out

    @staticmethod
    def _measure(store: NodeStore) -> tuple[str, str]:
        """The columns areas come from, (own, subtree total), by the store's ``measure``."""
        if getattr(store, "measure", "alloc") == "size":
            return "size", "tot_size"
        return "alloc", "tot_alloc"

    @staticmethod
    def effective_sizes(store: NodeStore) -> np.ndarray:
        """Area basis per node: subtree allocation for folders, own allocation for files (logical
        sizes instead in a store that measures by size: a scan source with ``size = "size"``)."""
        n = len(store)
        c = store.cols
        own, tot = Basis._measure(store)
        is_dir = c["kind"][:n] == Kind.DIR
        dup = (c["flags"][:n] & np.uint32(Flag.HARDLINK_DUP)) != 0
        out: np.ndarray = np.where(is_dir, c[tot][:n], np.where(dup, 0, c[own][:n])).astype(np.int64)
        return out

    @staticmethod
    def effective_sizes_of(store: NodeStore, ids: np.ndarray) -> np.ndarray:
        """:meth:`effective_sizes` for some nodes only."""
        c = store.cols
        own, tot = Basis._measure(store)
        is_dir = c["kind"][ids] == Kind.DIR
        dup = (c["flags"][ids] & np.uint32(Flag.HARDLINK_DUP)) != 0
        out: np.ndarray = np.where(is_dir, c[tot][ids], np.where(dup, 0, c[own][ids])).astype(np.int64)
        return out

    def sizes(self) -> np.ndarray:
        """:meth:`effective_sizes`, cached until the store changes. 8 B a node, worked out again after
        every change: engines and the palette use :meth:`sizes_of` / :class:`LazySizes` instead."""
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

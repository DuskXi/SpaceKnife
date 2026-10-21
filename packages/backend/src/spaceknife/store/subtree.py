"""Vectorised subtree queries: every node below a node, largest files, per-class totals.

A folder's children are one contiguous id range, so a subtree is expanded level by level by
turning (start, count) ranges into ids with numpy — no Python loop over nodes.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass

import numpy as np

from .nodes import Flag, Kind, NodeStore


def ranges_to_ids(starts: np.ndarray, counts: np.ndarray) -> np.ndarray:
    """Concatenate ``arange(s, s + c)`` for every pair, vectorised."""
    counts = counts.astype(np.int64)
    total = int(counts.sum())
    if total == 0:
        return np.empty(0, dtype=np.int64)
    offsets = np.cumsum(counts) - counts
    return np.repeat(starts.astype(np.int64) - offsets, counts) + np.arange(total, dtype=np.int64)


def subtree_ids(store: NodeStore, node: int) -> np.ndarray:
    """Ids of every node strictly below ``node`` (unordered).

    Safe while a scan adds listings in another thread (see :meth:`NodeStore.child_range`): counts
    are read before starts, and a range that starts at node 0 or runs past ``n`` reads as empty.
    The walk never visits more than ``n`` nodes (it raises instead of running away).
    """
    n = len(store)
    c = store.cols
    count_col, start_col = c["child_count"], c["child_start"]
    is_dir = c["kind"][:n] == Kind.DIR
    found: list[np.ndarray] = []
    total = 0
    frontier = np.array([node], dtype=np.int64)
    while frontier.size:
        count = count_col[frontier].astype(np.int64)  # counts first, then starts
        start = start_col[frontier].astype(np.int64)
        ok = (count > 0) & (start >= 1) & (start + count <= n)
        ids = ranges_to_ids(start[ok], count[ok])
        if not ids.size:
            break
        total += ids.size
        if total > n:  # cannot happen with consistent ranges; never let a walk run away
            raise RuntimeError(f"subtree walk below node {node} revisited nodes")
        found.append(ids)
        frontier = ids[is_dir[ids]]
    return np.concatenate(found) if found else np.empty(0, dtype=np.int64)


@dataclass(frozen=True, slots=True)
class Summary:
    top_files: np.ndarray  # node ids, largest first
    class_bytes: np.ndarray  # allocated bytes per colour class index
    class_files: np.ndarray
    latest_mtime: int


def summarize(
    store: NodeStore, node: int, cls_of: Callable[[np.ndarray], np.ndarray], n_classes: int, top: int
) -> Summary:
    """Largest files, per-class totals and newest modification time below ``node``.

    ``cls_of`` gives the colour class index of file nodes (``layout.Basis.cls_of``: by extension,
    and by size for ``ts``). A file node summarises itself.
    """
    c = store.cols
    ids = subtree_ids(store, node) if c["kind"][node] == Kind.DIR else np.array([node], dtype=np.int64)
    files = ids[c["kind"][ids] != Kind.DIR]
    files = files[(c["flags"][files] & np.uint32(Flag.HARDLINK_DUP)) == 0]
    alloc = c["alloc"][files].astype(np.int64)
    part = np.argpartition(-alloc, top)[:top] if files.size > top else np.arange(files.size)
    order = part[np.argsort(-alloc[part], kind="stable")]
    cls = cls_of(files) if files.size else np.empty(0, dtype=np.int64)
    class_bytes = np.bincount(cls, weights=alloc, minlength=n_classes).astype(np.int64)
    class_files = np.bincount(cls, minlength=n_classes).astype(np.int64)
    mt = c["mtime"][ids] if ids.size else np.zeros(1, dtype=np.uint32)
    return Summary(
        top_files=files[order],
        class_bytes=class_bytes,
        class_files=class_files,
        latest_mtime=int(mt.max()) if mt.size else 0,
    )

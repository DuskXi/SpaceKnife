"""Vectorised subtree queries: every node below a node, largest files, per-class totals.

A folder's children are one contiguous id range, so a subtree is expanded level by level by
turning (start, count) ranges into ids with numpy — no Python loop over nodes.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator
from dataclasses import dataclass

import numpy as np

from .nodes import Flag, Kind, NodeStore

# ids per piece of a chunked subtree walk (subtree_chunks): a summary of a whole volume holds a few
# tens of MB of temporaries instead of ~100 B per node (docs/memory.md §5)
SUMMARY_CHUNK = 1 << 18


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


def _pieces(start: np.ndarray, count: np.ndarray, chunk: int) -> Iterator[tuple[np.ndarray, np.ndarray]]:
    """The ranges ``(start, count)`` in runs of at most ``2 * chunk`` ids; a longer range is cut."""
    if not count.size:
        return
    if int(count.max()) > chunk:
        reps = (count + chunk - 1) // chunk
        idx = np.repeat(np.arange(len(count)), reps)
        k = np.arange(len(idx), dtype=np.int64) - np.repeat(np.cumsum(reps) - reps, reps)
        start = start[idx] + k * chunk
        count = np.minimum(count[idx] - k * chunk, chunk)
    group = (np.cumsum(count) - count) // chunk  # ranges starting in the same chunk-sized stretch
    cuts = (np.flatnonzero(np.diff(group)) + 1).tolist()
    for a, b in zip([0, *cuts], [*cuts, len(count)], strict=True):
        yield start[a:b], count[a:b]


def subtree_chunks(store: NodeStore, node: int, chunk: int = SUMMARY_CHUNK) -> Iterator[np.ndarray]:
    """:func:`subtree_ids` in pieces of at most ``2 * chunk`` ids (unordered, every id once), so a
    walk over a whole volume never holds an array of every node. Same safety rules as
    :func:`subtree_ids`; the folders still to visit are the only thing that grows (8 B a folder)."""
    n = len(store)
    c = store.cols
    count_col, start_col, kind = c["child_count"], c["child_start"], c["kind"]
    total = 0
    todo = [np.array([node], dtype=np.int64)]  # folders whose children are still to visit
    while todo:
        frontier = todo.pop()
        count = count_col[frontier].astype(np.int64)  # counts first, then starts
        start = start_col[frontier].astype(np.int64)
        ok = (count > 0) & (start >= 1) & (start + count <= n)
        for s, k in _pieces(start[ok], count[ok], chunk):
            ids = ranges_to_ids(s, k)
            total += ids.size
            if total > n:  # cannot happen with consistent ranges; never let a walk run away
                raise RuntimeError(f"subtree walk below node {node} revisited nodes")
            yield ids
            dirs = ids[kind[ids] == Kind.DIR]
            if dirs.size:
                todo.append(dirs)


def largest(ids: np.ndarray, alloc: np.ndarray, k: int) -> tuple[np.ndarray, np.ndarray]:
    """The ``k`` largest by ``alloc``, largest first; of equal sizes the smaller id first."""
    if ids.size > k:
        if k <= 0:
            return ids[:0], alloc[:0]
        cut = alloc.size - k
        v = np.partition(alloc, cut)[cut]  # the k-th largest size
        above = np.flatnonzero(alloc > v)  # fewer than k
        tied = np.flatnonzero(alloc == v)
        need = k - above.size  # at least 1
        if tied.size > need:
            tied = tied[np.argpartition(ids[tied], need - 1)[:need]]
        keep = np.concatenate([above, tied])
        ids, alloc = ids[keep], alloc[keep]
    order = np.lexsort((ids, -alloc))
    return ids[order], alloc[order]


@dataclass(frozen=True, slots=True)
class Summary:
    top_files: np.ndarray  # node ids, largest first
    class_bytes: np.ndarray  # allocated bytes per colour class index
    class_files: np.ndarray
    latest_mtime: int
    # bytes per (class, class by extension alone) where a folder class took files over (or None)
    class_parts: np.ndarray | None = None


def summarize(
    store: NodeStore,
    node: int,
    cls_of: Callable[[np.ndarray], np.ndarray],
    n_classes: int,
    top: int,
    base_cls_of: Callable[[np.ndarray], np.ndarray] | None = None,
    chunk: int = SUMMARY_CHUNK,
) -> Summary:
    """Largest files, per-class totals and newest modification time below ``node``.

    ``cls_of`` gives the colour class index of file nodes (``layout.Basis.cls_of``: by extension,
    and by size for ``ts``, under the folder classes). With ``base_cls_of`` (the class by extension
    alone), ``class_parts`` says what the folder classes took over: 「游戏 · 其中视频 3 GB」.
    A file node summarises itself. Largest first; of equal sizes the smaller id first.

    Works through the subtree ``chunk`` ids at a time (:func:`subtree_chunks`), so the temporaries
    stay at a few tens of MB on a volume of millions of nodes; the result does not depend on it.
    """
    c = store.cols
    if c["kind"][node] == Kind.DIR:
        pieces: Iterator[np.ndarray] = subtree_chunks(store, node, chunk)
    else:
        pieces = iter([np.array([node], dtype=np.int64)])
    dup = np.uint32(Flag.HARDLINK_DUP)
    best = np.empty(0, dtype=np.int64)
    best_alloc = np.empty(0, dtype=np.int64)
    class_bytes = np.zeros(n_classes, dtype=np.int64)
    class_files = np.zeros(n_classes, dtype=np.int64)
    parts: np.ndarray | None = None
    latest = 0
    for ids in pieces:
        if not ids.size:
            continue
        latest = max(latest, int(c["mtime"][ids].max()))
        files = ids[c["kind"][ids] != Kind.DIR]
        files = files[(c["flags"][files] & dup) == 0]
        if not files.size:
            continue
        alloc = c["alloc"][files].astype(np.int64)
        best, best_alloc = largest(np.concatenate([best, files]), np.concatenate([best_alloc, alloc]), top)
        cls = cls_of(files)
        # float sums of whole byte counts stay exact below 2**53 (8 PiB) per piece and class
        class_bytes += np.bincount(cls, weights=alloc, minlength=n_classes).astype(np.int64)
        class_files += np.bincount(cls, minlength=n_classes)
        if base_cls_of is not None:
            base = base_cls_of(files).astype(np.int64)
            moved = base != cls
            if moved.any():
                pair = cls[moved].astype(np.int64) * n_classes + base[moved]
                flat = np.bincount(pair, weights=alloc[moved], minlength=n_classes * n_classes)
                flat = flat.astype(np.int64)
                parts = flat if parts is None else parts + flat
    return Summary(
        top_files=best,
        class_bytes=class_bytes,
        class_files=class_files,
        latest_mtime=latest,
        class_parts=parts.reshape(n_classes, n_classes) if parts is not None else None,
    )

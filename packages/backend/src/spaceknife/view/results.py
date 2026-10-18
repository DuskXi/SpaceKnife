"""Filter result rows of a view (W3 sidebar, UI-W3-31…33, 待定 13).

Rows follow the treemap: a matching file that is drawn on its own is one row; matches hidden in a
collapsed folder, a "N small items" / "+N items" block, or under a sliver too small to draw are
grouped into the visible block that holds them (the last kind: one "rest" row for the zoom root).
Every match below the zoom root lands in exactly one row, so the rows add up to the frame totals.
"""

from __future__ import annotations

import os
import weakref
from dataclasses import dataclass
from typing import Literal

import numpy as np

from ..filter.engine import _levels_of
from ..layout.engine import RectKind
from ..store.nodes import Kind
from .state import View

RowKind = Literal["file", "folder", "group", "rest"]
SortBy = Literal["size", "name", "count"]


@dataclass(frozen=True, slots=True)
class ResultRow:
    key: int  # draw-list key of the block to locate; the zoom key for "rest"
    node: int  # the file or folder; the container for "group" and "rest"
    kind: RowKind
    name: str  # file / folder name; for a one-file group the file's name, else ""
    count: int  # matching files in this row
    alloc: int
    size: int
    parent: int  # folder holding the block (path column)


def result_rows(view: View) -> list[ResultRow]:
    """All result rows for the view's current filter and last frame, unsorted."""
    res = view.filter.result if view.filter.active else None
    frame = view.last
    if res is None or frame is None:
        return []
    store = view.store
    c = store.cols
    draw = frame.draw
    n = res.n
    kind = draw.kind
    node = draw.node.astype(np.int64)
    rest = len(draw)  # pseudo row index
    owner = np.full(n, -1, dtype=np.int64)
    real = (kind <= RectKind.PENDING) & (node >= 0) & (node < n)
    owner[node[real]] = np.flatnonzero(real)
    for row in np.flatnonzero((kind == RectKind.SMALL) | (kind == RectKind.MORE)):
        members = draw.members.get(int(draw.key[row]))
        if members is not None:
            owner[members[members < n]] = row
    z = view.zoom
    tops = np.array([z.node], dtype=np.int64) if z.members is None else np.asarray(z.members, dtype=np.int64)
    tops = tops[(tops >= 0) & (tops < n)]
    owner[tops[owner[tops] < 0]] = rest
    # undrawn nodes belong to the nearest drawn ancestor; nodes outside the zoom root stay -1
    levels = _levels_of(store, n)
    parent = levels.parent
    for d in range(1, levels.max_depth + 1):
        idx = levels.level(d)
        unset = idx[owner[idx] < 0]
        if unset.size:
            owner[unset] = owner[parent[unset]]
    matched = np.flatnonzero(res.match[:n] & (owner >= 0))
    if not matched.size:
        return []
    rows_of = owner[matched]
    order = np.argsort(rows_of, kind="stable")
    rows_sorted = rows_of[order]
    starts = np.flatnonzero(np.r_[True, rows_sorted[1:] != rows_sorted[:-1]])
    uniq = rows_sorted[starts]
    counts = np.diff(np.r_[starts, len(rows_sorted)])
    allocs = np.add.reduceat(res.alloc[matched][order], starts)  # duplicates already carry 0 bytes
    sizes = np.add.reduceat(res.size[matched][order], starts)
    firsts = matched[order][starts]
    out: list[ResultRow] = []
    for row, cnt, al, sz, first in zip(
        uniq.tolist(), counts.tolist(), allocs.tolist(), sizes.tolist(), firsts.tolist(), strict=True
    ):
        if row == rest:
            out.append(ResultRow(z.key, z.node, "rest", "", cnt, al, sz, z.node))
            continue
        rk = int(kind[row])
        nd = int(node[row])
        key = int(draw.key[row])
        if rk in (RectKind.SMALL, RectKind.MORE):
            out.append(ResultRow(key, nd, "group", store.name(first) if cnt == 1 else "", cnt, al, sz, nd))
        elif rk == RectKind.FILE and c["kind"][nd] != Kind.DIR:
            out.append(ResultRow(key, nd, "file", store.name(nd), cnt, al, sz, int(c["parent"][nd])))
        else:
            out.append(ResultRow(key, nd, "folder", store.name(nd), cnt, al, sz, int(c["parent"][nd])))
    return out


def sort_rows(rows: list[ResultRow], by: SortBy) -> list[ResultRow]:
    if by == "name":
        return sorted(rows, key=lambda r: (r.name.casefold() or "￿", -r.alloc, r.key))
    if by == "count":
        return sorted(rows, key=lambda r: (-r.count, -r.alloc, r.key))
    return sorted(rows, key=lambda r: (-r.alloc, -r.count, r.key))


_CACHE: weakref.WeakKeyDictionary[View, tuple[object, object, str, list[ResultRow]]] = (
    weakref.WeakKeyDictionary()
)


def sorted_rows(view: View) -> list[ResultRow]:
    """The view's result rows in its current order, cached per (frame, filter result, order)."""
    frame, res, by = view.last, view.filter.result if view.filter.active else None, view.results_sort
    hit = _CACHE.get(view)
    if hit is not None and hit[0] is frame and hit[1] is res and hit[2] == by:
        return hit[3]
    rows = sort_rows(result_rows(view), by)
    _CACHE[view] = (frame, res, by, rows)
    return rows


def relative_path(view: View, node: int) -> str:
    """Path of a folder below its scan root (the root's own name first when a view shows several)."""
    store = view.store
    parent = store.cols["parent"]
    parts: list[str] = []
    cur = node
    while cur > 0 and int(parent[cur]) > 0:
        parts.append(store.name(cur))
        cur = int(parent[cur])
    if view.home == 0 and cur > 0:
        root = store.name(cur)  # roots are named by their full path
        parts.append(os.path.basename(root.rstrip("\\/")) or root)
    return os.sep.join(reversed(parts))


def effective_tag(view: View, node: int) -> tuple[str | None, bool]:
    """(tag colour, inherited) of a node: its own tag, else the nearest tagged ancestor's."""
    own = view.tags.of(view.store, node)
    if own is not None:
        return own, False
    for anc in view.store.ancestors(node):
        color = view.tags.of(view.store, anc)
        if color is not None:
            return color, True
    return None, False


__all__ = [
    "ResultRow",
    "RowKind",
    "SortBy",
    "effective_tag",
    "relative_path",
    "result_rows",
    "sort_rows",
    "sorted_rows",
]

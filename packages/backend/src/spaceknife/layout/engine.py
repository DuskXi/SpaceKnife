"""Nested treemap layout: the zoom root's subtree as a flat, pre-ordered list of pixel rectangles.

Geometry follows docs/UI-SPEC.md (T section, tokens ``treemap.geometry``): every block keeps a
2 px gap to its neighbours; an expanded folder has a 1 px border, 2 px padding and an 18 px
header; blocks under 2 px are not drawn and content under 4 px is not subdivided. All lengths
are scaled by the device pixel ratio and snapped to whole physical pixels, so seams are crisp at
125 % / 150 % and identical in every frontend.

Small items (G5): in each container, files whose share of the area is below the minimum element
size are merged into one "N small items" block (coloured by a class that holds ≥ 50 % of its
bytes, else "other"); siblings that are still too small afterwards — folders, files, that block —
are merged into "+N items", placed last. Merging needs at least two members (merge.py).

Aggregate zoom (UI-TM-13): the members of such a block fill the view; the largest are drawn one by
one and the rest form one "+N items" block, shrunk when that lets more members through, so every
zoom into an aggregate shows members of its own.

The treemap is the registered visualization ``treemap`` (:class:`TreemapEngine`, wire layout
``drawlist.v1``); :class:`Layout` keeps the older call with a plain size array.
"""

from __future__ import annotations

import time

import numpy as np

from spaceknife_protocol.view import DRAWLIST_LAYOUT

from ..store.nodes import Flag, Kind
from . import registry
from .api import ENGINE_API, LayoutParams, RootSpec, SizeSource, StoreSizes, Visualization
from .basis import Basis
from .merge import MergeCtx, merge_items, page_items
from .rows import (
    COLLAPSED,
    FILE,
    FOLDER,
    GEOMETRY_COLUMNS,
    KEY_FREE,
    KEY_MORE,
    KEY_NODE_MASK,
    KEY_SMALL,
    KEY_UNKNOWN,
    L_NAME,
    L_SIZE,
    PAGE_REST,
    PENDING_RECT,
    DrawList,
    Label,
    RectKind,
    label_box,
)
from .squarify import squarify

__all__ = [
    "KEY_FREE",
    "KEY_MORE",
    "KEY_NODE_MASK",
    "KEY_SMALL",
    "KEY_UNKNOWN",
    "PAGE_REST",
    "DrawList",
    "Label",
    "Layout",
    "LayoutParams",
    "RectKind",
    "RootSpec",
    "TreemapEngine",
]

DIR = int(Kind.DIR)
PENDING = int(Flag.PENDING)


class _Geom:
    """Lengths in physical pixels."""

    def __init__(self, p: LayoutParams) -> None:
        d = p.dpr
        self.dpr = d
        self.gap = max(1, round(p.gap * d))
        self.border = max(1, round(p.border * d))
        self.pad = round(p.padding * d)
        self.header = round(p.header * p.font_scale * d)
        self.label_unit = d * p.font_scale  # px per CSS px of label thresholds
        self.aspect = p.aspect
        self.min_draw = p.min_draw * d
        self.min_content = p.min_content * d
        self.expand_w = p.expand_min_w * d
        self.expand_h = p.expand_min_h * d
        self.threshold = (p.min_element * d) ** 2

    def label(self, w: int, h: int) -> Label:
        return Label(label_box(w / self.label_unit, h / self.label_unit))


class Layout(Basis):
    """Lays out one store as a treemap; reusable across calls."""

    def run(self, params: LayoutParams, root: RootSpec, sizes: np.ndarray | None = None) -> DrawList:
        """Lay out ``root`` in the viewport. ``sizes`` overrides the area basis (filter: hide mode)."""
        return _treemap(self, params, root, StoreSizes(self) if sizes is None else sizes)


class TreemapEngine:
    """The treemap behind the ``treemap`` visualization (:class:`.api.Engine`)."""

    __slots__ = ("basis",)

    def __init__(self, basis: Basis) -> None:
        self.basis = basis

    def run(self, params: LayoutParams, root: RootSpec, sizes: SizeSource) -> DrawList:
        return _treemap(self.basis, params, root, sizes)  # sizes of the laid-out nodes only

    def levels(self, params: LayoutParams) -> int:
        return params.detail


class _ArraySizes:
    """A plain size array as a :class:`.api.SizeSource` (``Layout.run`` with ``sizes``)."""

    __slots__ = ("arr", "n")

    def __init__(self, arr: np.ndarray) -> None:
        self.arr = arr
        self.n = len(arr)

    def of(self, ids: np.ndarray) -> np.ndarray:
        out: np.ndarray = self.arr[ids]
        return out

    def full(self) -> np.ndarray:
        return self.arr


def _treemap(basis: Basis, params: LayoutParams, root: RootSpec, sizes: SizeSource | np.ndarray) -> DrawList:
    t0 = time.perf_counter()
    run = _Run(basis, params, sizes)
    W, H = round(params.width * params.dpr), round(params.height * params.dpr)
    c = basis.store.cols
    if root.members is not None:
        run.ctx.avoid = -1 if root.key is None else int(root.key)
        members = np.asarray(root.members, dtype=np.int64)
        run.container(root.node, members, [], (0, 0, W, H), 1, -1, page=True)
    elif c["kind"][root.node] == Kind.DIR:
        extra: list[tuple[int, int]] = []
        if root.free:
            extra.append((KEY_FREE, int(root.free)))
        if root.unknown is not None and root.unknown > 0:
            extra.append((KEY_UNKNOWN, int(root.unknown)))
        run.container(root.node, run.children(root.node), extra, (0, 0, W, H), 1, -1)
    else:  # a file as the zoom root: its block fills the view
        run.container(-1, np.array([root.node], dtype=np.int64), [], (0, 0, W, H), 1, -1)
    return run.result(W, H, time.perf_counter() - t0)


class _Run:
    def __init__(self, basis: Basis, params: LayoutParams, sizes: SizeSource | np.ndarray) -> None:
        self.basis = basis
        self.store = basis.store
        self.p = params
        self.g = _Geom(params)
        self.sizes = _ArraySizes(sizes) if isinstance(sizes, np.ndarray) else sizes
        n = self.sizes.n
        self.n = n
        c = self.store.cols
        self.kind = c["kind"][:n]
        self.flags = c["flags"][:n]
        self.child_count = c["child_count"][:n]
        self.rows: list[tuple[int, ...]] = []
        self.names: list[str] = []
        self.ctx = MergeCtx(self.store, basis, params.sort_by_size, {})

    def children(self, node: int) -> np.ndarray:
        return self.basis.children(node, self.n)

    def container(
        self,
        container: int,
        ids: np.ndarray,
        extra: list[tuple[int, int]],
        rect: tuple[int, int, int, int],
        level: int,
        parent_row: int,
        page: bool = False,
    ) -> None:
        g = self.g
        x0, y0, x1, y1 = rect
        # cells cover the content rectangle extended by one gap; each block keeps its gap right/bottom
        ex1, ey1 = x1 + g.gap, y1 + g.gap
        area = float((ex1 - x0) * (ey1 - y0))
        s = self.sizes.of(ids)
        if page:
            need = g.threshold / area if area > 0 else float("inf")  # share of the view per member
            items = page_items(self.ctx, container, ids, s, need)
        else:
            threshold = g.threshold  # bytes below threshold · total / area → too small

            def thr_of(total: float) -> float:
                return threshold * total / area if area > 0 else float("inf")

            items = merge_items(self.ctx, container, ids, s, extra, thr_of)
        if not items:
            return
        weights = [it.weight or float(it.size) for it in items]
        if page and len(items) > 1 and items[-1].kind == RectKind.MORE:
            # the leftover gets a strip of its own: no member shares a squarify row with a far larger block
            body, rest = squarify([sum(weights[:-1]), weights[-1]], x0, y0, ex1, ey1, g.aspect)
            cells = [*squarify(weights[:-1], *body, g.aspect), rest]
        else:
            cells = squarify(weights, x0, y0, ex1, ey1, g.aspect)
        rows, names = self.rows, self.names
        gap, min_draw, lu = g.gap, g.min_draw, g.label_unit
        expandable = level < self.p.detail
        depth = min(level, 255)
        kind, flags, child_count, store = self.kind, self.flags, self.child_count, self.store
        for it, (fx0, fy0, fx1, fy1) in zip(items, cells, strict=True):
            bx0, by0 = round(fx0), round(fy0)
            bw, bh = round(fx1) - gap - bx0, round(fy1) - gap - by0
            if bw < min_draw or bh < min_draw:
                continue
            rk = int(it.kind)
            node = it.node
            if rk == FILE and kind[node] == DIR:
                if flags[node] & PENDING:
                    rk = PENDING_RECT
                elif expandable and bw >= g.expand_w and bh >= g.expand_h and child_count[node] > 0:
                    rk = FOLDER
                else:
                    rk = COLLAPSED
            lbl = L_NAME | L_SIZE if rk == FOLDER else label_box(bw / lu, bh / lu)  # UI-TM-14..16
            row = len(rows)
            rows.append(
                (it.key, node, parent_row, rk, depth, bx0, by0, bw, bh, it.cls, lbl, it.size, it.count)
            )
            names.append(store.name(node) if lbl and rk <= PENDING_RECT else "")
            if rk == FOLDER:
                cx0, cy0 = bx0 + g.border + g.pad, by0 + g.border + g.header
                cx1, cy1 = bx0 + bw - g.border - g.pad, by0 + bh - g.border - g.pad
                if cx1 - cx0 >= g.min_content and cy1 - cy0 >= g.min_content:
                    self.container(node, self.children(node), [], (cx0, cy0, cx1, cy1), level + 1, row)

    def result(self, width: int, height: int, elapsed: float) -> DrawList:
        table = np.array(self.rows, dtype=np.int64).reshape(-1, len(_COLUMNS))
        arrays = {name: table[:, i].astype(dt) for i, (name, dt) in enumerate(_COLUMNS)}
        geometry = {name: arrays.pop(name) for name in GEOMETRY_COLUMNS[DRAWLIST_LAYOUT]}
        return DrawList(
            **arrays,
            names=self.names,
            geometry=geometry,
            members=self.ctx.members,
            width=width,
            height=height,
            elapsed_s=elapsed,
            layout=DRAWLIST_LAYOUT,
            visual="treemap",
            meta=None,
            levels=self.p.detail,
        )


_COLUMNS: tuple[tuple[str, type[np.generic]], ...] = (  # the order of the row tuples above
    ("key", np.uint32),
    ("node", np.int32),
    ("parent", np.int32),
    ("kind", np.uint8),
    ("depth", np.uint8),
    ("x", np.int32),
    ("y", np.int32),
    ("w", np.int32),
    ("h", np.int32),
    ("cls", np.uint8),
    ("label", np.uint8),
    ("size", np.int64),
    ("count", np.uint32),
)

registry.register(
    Visualization(
        id="treemap",
        api=ENGINE_API,
        layout=DRAWLIST_LAYOUT,
        capability="",  # the baseline: every session can show it
        title_id="树图",
        icon="vizTreemap",
        order=0,
        engine=TreemapEngine,
        default_color="class",
        aliases=("visualization treemap", "treemap"),
    )
)

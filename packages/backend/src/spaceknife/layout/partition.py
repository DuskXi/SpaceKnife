"""Partition layouts: the zoom root's subtree as nested intervals, one level per band or ring.

Shared by the flame graph (flame.py, ``flame.v1``) and the polar flame graph (flame_polar.py,
``polar.v1``). Every row owns an interval [u0, u1) of the zoom root's whole width or whole turn and a
level (1 = the zoom root's children); a :class:`Projection` snaps intervals to pixels and says what
is visible, what can show its children on the next level and which label bits a row gets. The zoom
root itself is not a row: frontends draw it as the root bar or the centre disc (``FrameInfo.root``).

A container shares its interval out by bytes against max(its children's bytes, its own size): a
folder's own allocation stays blank after its children, and totals that do not add up yet during a
scan never push a child past its parent. Siblings share their boundaries, so they meet exactly, and a
child always lies within its parent.

Small items (G5) merge as in the treemap (merge.py), with a length instead of an area: an item that
would span less than the minimum element size along its level joins "N small items" / "+N items".
Since a length is proportional to bytes, the byte threshold is one constant per container; in the
polar view the length is the arc at the ring's middle radius, so the threshold shrinks ring by ring
and deep small folders show as thin spikes on the outer rings. An aggregate zoom pages through the
members as in the treemap, the leftover "+N items" being the last block of the first level.

Frames hold at most ``MAX_ROWS`` rows: a folder whose children would not fit any more is drawn
collapsed (its children are counted before it is emitted, siblings still to come are reserved).
"""

from __future__ import annotations

import time
from collections.abc import Callable
from typing import Protocol

import numpy as np

from spaceknife_protocol import view as pv

from ..store.nodes import Flag, Kind
from .api import MAX_ROWS, LayoutParams, RootSpec, SizeSource
from .basis import Basis
from .merge import Item, MergeCtx, merge_items, page_items
from .rows import (
    COLLAPSED,
    COMMON_COLUMNS,
    FILE,
    FOLDER,
    KEY_FREE,
    KEY_MORE,
    KEY_NODE_MASK,
    KEY_SMALL,
    KEY_UNKNOWN,
    PENDING_RECT,
    DrawList,
)

DIR = int(Kind.DIR)
PENDING = int(Flag.PENDING)

Cell = tuple[int, ...]  # a row's geometry, in the order of Projection.columns


class Projection(Protocol):
    """Where the intervals of one viewport go: built per run from the layout parameters."""

    @property
    def levels(self) -> int:
        """Levels below the zoom root that fit (at most ``LayoutParams.detail``)."""
        ...

    @property
    def columns(self) -> tuple[str, ...]:
        """The geometry columns of a cell, in order (``GEOMETRY_COLUMNS`` of the layout)."""
        ...

    def unit_px(self, level: int) -> float:
        """Pixels the whole interval [0, 1) spans along ``level``: the length scale of the merges."""
        ...

    def cell(self, a: float, b: float, level: int) -> Cell | None:
        """Snapped geometry of [a, b) on ``level``; None when it is too small to draw."""
        ...

    def expandable(self, cell: Cell, level: int) -> bool:
        """Whether a folder drawn as ``cell`` is large enough to show its children on the next level."""
        ...

    def label(self, cell: Cell, kind: int) -> int:
        """Label bits of a row of ``kind`` drawn as ``cell``."""
        ...

    def meta(self) -> pv.FlameGeometry | pv.PolarGeometry | pv.ShapesGeometry:
        """``FrameInfo.geometry`` of the frame."""
        ...


class PartitionEngine:
    """Lays out one store as a partition for one projection; reusable across calls (an Engine)."""

    def __init__(
        self,
        basis: Basis,
        make_projection: Callable[[LayoutParams], Projection],
        layout: str,
        visual: str,
    ) -> None:
        self.basis = basis
        self.make_projection = make_projection
        self.layout = layout  # wire layout of the rows ("flame.v1", "polar.v1")
        self.visual = visual
        self.max_rows = MAX_ROWS  # rows per frame (tests lower it)

    def levels(self, params: LayoutParams) -> int:
        return self.make_projection(params).levels

    def run(self, params: LayoutParams, root: RootSpec, sizes: SizeSource) -> DrawList:
        """Lay out ``root`` in the viewport, sized by ``sizes`` (read per container, never in full)."""
        t0 = time.perf_counter()
        proj = self.make_projection(params)
        run = _Run(self.basis, proj, params, sizes, self.max_rows)
        if proj.levels > 0:
            if root.members is not None:
                run.ctx.avoid = -1 if root.key is None else int(root.key)
                run.top(root.node, np.asarray(root.members, dtype=np.int64), [], page=True)
            elif self.basis.store.cols["kind"][root.node] == DIR:
                extra: list[tuple[int, int]] = []
                if root.free:
                    extra.append((KEY_FREE, int(root.free)))
                if root.unknown is not None and root.unknown > 0:
                    extra.append((KEY_UNKNOWN, int(root.unknown)))
                run.top(root.node, self.basis.children(root.node, sizes.n), extra)
            # a file as the zoom root: no rows, the root bar / centre disc is the file
        W, H = round(params.width * params.dpr), round(params.height * params.dpr)
        return run.result(W, H, time.perf_counter() - t0, self.layout, self.visual)


class _Run:
    def __init__(
        self, basis: Basis, proj: Projection, params: LayoutParams, sizes: SizeSource, max_rows: int
    ) -> None:
        self.basis = basis
        self.store = basis.store
        self.proj = proj
        self.sizes = sizes
        n = sizes.n  # node count taken once: a scan may add nodes meanwhile
        self.n = n
        c = self.store.cols
        self.kind = c["kind"][:n]
        self.flags = c["flags"][:n]
        self.child_count = c["child_count"][:n]
        self.min_px = params.min_element * params.dpr
        self.max_rows = max_rows
        self.reserved = 0  # items of the containers being emitted that are still to come
        self.rows: list[tuple[int, ...]] = []
        self.names: list[str] = []
        self.ctx = MergeCtx(self.store, basis, params.sort_by_size, {})

    def items(
        self,
        container: int,
        own: int,
        ids: np.ndarray,
        extra: list[tuple[int, int]],
        span: float,
        level: int,
        page: bool = False,
    ) -> tuple[list[Item], float]:
        """Blocks of a container of ``own`` bytes whose interval is ``span`` long, and the bytes the
        interval stands for."""
        s = self.sizes.of(ids)
        length = self.proj.unit_px(level) * span  # pixels of the container's interval on ``level``
        if length <= 0:
            return [], 0.0
        if page:
            items = page_items(self.ctx, container, ids, s, self.min_px / length)
            return items, float(sum(it.weight or it.size for it in items))
        total = max(float(s[s > 0].sum()) + sum(v for _, v in extra), float(own))
        threshold = self.min_px * total / length  # bytes below → narrower than the minimum element
        return merge_items(self.ctx, container, ids, s, extra, lambda _: threshold), total

    def top(self, container: int, ids: np.ndarray, extra: list[tuple[int, int]], page: bool = False) -> None:
        own = 0 if page else int(self.sizes.of(np.array([container], dtype=np.int64))[0])
        items, total = self.items(container, own, ids, extra, 1.0, 1, page)
        self.reserved = len(items)
        self.container(items, total, 0.0, 1.0, 1, -1)

    def container(
        self, items: list[Item], total: float, u0: float, u1: float, level: int, parent_row: int
    ) -> None:
        """Emit ``items`` over [u0, u1) of ``level``, ``total`` bytes filling the interval."""
        if not items or total <= 0:
            return
        proj, rows, names, store = self.proj, self.rows, self.names, self.store
        kind, flags, child_count = self.kind, self.flags, self.child_count
        span = u1 - u0
        deeper = level < proj.levels
        depth = min(level, 255)
        done = 0.0  # bytes placed so far: boundaries from running totals, the last one exactly u1
        b = u0
        for it in items:
            self.reserved -= 1
            done += it.weight or it.size
            a, b = b, (u1 if done >= total else min(u1, u0 + span * (done / total)))
            cell = proj.cell(a, b, level)
            if cell is None or len(rows) >= self.max_rows:
                continue  # too small to draw (or no room left: only when the first level alone overflows)
            rk = int(it.kind)
            node = it.node
            sub: list[Item] = []
            sub_total = 0.0
            if rk == FILE and kind[node] == DIR:
                if flags[node] & PENDING:
                    rk = PENDING_RECT
                else:
                    rk = COLLAPSED
                    if deeper and child_count[node] > 0 and proj.expandable(cell, level):
                        children = self.basis.children(node, self.n)
                        sub, sub_total = self.items(node, it.size, children, [], b - a, level + 1)
                        if len(rows) + 1 + self.reserved + len(sub) <= self.max_rows:
                            rk = FOLDER
                        else:  # its children would not fit in the frame any more
                            tag = node & KEY_NODE_MASK
                            self.ctx.members.pop(KEY_SMALL | tag, None)
                            self.ctx.members.pop(KEY_MORE | tag, None)
                            sub = []
            lbl = proj.label(cell, rk)
            row = len(rows)
            rows.append((it.key, node, parent_row, rk, depth, it.cls, lbl, it.size, it.count, *cell))
            names.append(store.name(node) if lbl and rk <= PENDING_RECT else "")
            if sub:
                self.reserved += len(sub)
                self.container(sub, sub_total, a, b, level + 1, row)

    def result(self, width: int, height: int, elapsed: float, layout: str, visual: str) -> DrawList:
        geo = self.proj.columns
        k = len(COMMON_COLUMNS)
        table = np.array(self.rows, dtype=np.int64).reshape(-1, k + len(geo))
        arrays = {name: table[:, i].astype(dt) for i, (name, dt) in enumerate(COMMON_COLUMNS)}
        geometry = {name: table[:, k + i].astype(np.int32) for i, name in enumerate(geo)}
        return DrawList(
            **arrays,
            names=self.names,
            geometry=geometry,
            members=self.ctx.members,
            width=width,
            height=height,
            elapsed_s=elapsed,
            layout=layout,
            visual=visual,
            meta=self.proj.meta(),
            levels=self.proj.levels,
        )

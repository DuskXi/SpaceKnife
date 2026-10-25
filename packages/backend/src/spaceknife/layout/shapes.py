"""Shapes (``shapes.v1``): a plugin's own layout. The host picks what to draw, the plugin where.

The host walks the zoom root's subtree as the partition layouts do (partition.py): the same
small-item merges, "+N items" pages of an aggregate zoom, pending folders, free and unknown space at
a whole-volume root and a row budget. "Too small" is judged by area: a block's share of the zoom
root times the canvas times ``fill`` (how much of its share a block really gets: about 0.5 for circle
packing, 1 for an icicle), against the minimum element size squared. The share is not shrunk again
level by level: a folder alone in its parent gets nearly all of it, and a deep chain of them would
otherwise vanish; what the estimate lets through and the layout finds too small it leaves out.

That :class:`Hierarchy` (at most ``max_rows`` rows, parents first) goes to the placer, the plugin's
layout function, which answers a shape per row (:class:`Placed`): a rounded rectangle (a circle
when the radius is half its sides), a convex polygon, or nothing. The host checks the shapes, drops
a row without one together with what lies below it, works out the label bits and returns a
``shapes.v1`` draw list.

The answer depends only on the hierarchy, the canvas and the options, so it is cached by their
digest and the plugin's own key (its settings, its cache epoch): moving the selection, a hover
repaint or a second view of the same place asks the plugin nothing. Notation as in flame.py:
lu = dpr · font scale, m = min_element · dpr.
"""

from __future__ import annotations

import hashlib
import math
import threading
import time
from collections import OrderedDict
from collections.abc import Callable
from dataclasses import dataclass

import numpy as np

from spaceknife_protocol import view as pv

from ..store.nodes import Kind
from .api import MAX_ROWS, LayoutParams, RootSpec, SizeSource
from .basis import Basis
from .partition import Cell, PartitionEngine
from .rows import (
    COLLAPSED,
    FOLDER,
    KEY_MORE,
    L_BIG,
    L_NAME,
    L_SHORT,
    L_SIZE,
    L_TOP,
    PENDING_RECT,
    DrawList,
    RectKind,
)

SPAN = 1 << 30  # interval resolution of the hierarchy (u0, u1 travel as shares of the root's [0, 1))
MAX_COORD = 1 << 24  # px: coordinates are clipped to ±this (i4 columns, no overflow downstream)
DEFAULT_ROWS = 5000  # rows a plugin layout gets by default (it runs in Python every new picture)
CACHE = 8  # placed answers kept per engine (one per place and canvas recently drawn)

# shape kinds a placer answers
NONE, BOX, POLYGON = 0, 1, 2
# label placement a placer may ask for (0: the host decides)
AUTO, NO_LABEL, CENTRE, TOP = 0, 1, 2, 3

# label geometry (docs/plugin-dev.md 「自带布局的可视化」, frontend visual/shapes/geometry.ts)
TWO_LINES = 34  # lu: the band two lines of a centred label need
ONE_LINE = 18  # lu: one line
TOP_INSET = 4  # lu: a top label's band starts this far below the shape's top
TOP_BAND = 15  # lu: its height
POLY_INSET = 0.9  # a polygon's text band: this share of its ellipse's chord


class ShapesProjection:
    """Areas of one viewport (a :class:`.partition.Projection` whose lengths are areas / m)."""

    columns = ("u0", "u1", "area")

    def __init__(self, p: LayoutParams, fill: float) -> None:
        d = p.dpr
        W, H = round(p.width * d), round(p.height * d)
        self.m = max(1.0, p.min_element * d)
        self.area = float(W * H)
        self.fill = min(1.0, max(0.05, fill))
        self.levels = max(0, min(p.detail, 15))
        self.expand = max(1.0, p.expand_min_w * p.expand_min_h * d * d)

    def _area(self, level: int) -> float:
        return self.area * self.fill

    def unit_px(self, level: int) -> float:
        # an item below m · total / (span · unit) bytes covers less than m² of the canvas
        return self._area(level) / self.m

    def cell(self, a: float, b: float, level: int) -> Cell | None:
        area = (b - a) * self._area(level)
        if area < 1.0:
            return None
        return (round(a * SPAN), round(b * SPAN), round(area))

    def expandable(self, cell: Cell, level: int) -> bool:
        return cell[2] >= self.expand

    def label(self, cell: Cell, kind: int) -> int:
        return 0  # worked out from the shapes the plugin gives (:func:`assemble`)

    def meta(self) -> pv.ShapesGeometry:
        return pv.ShapesGeometry()


@dataclass(frozen=True, slots=True)
class Hierarchy:
    """What the placer gets: one row per block to place, parents first (docs/plugin-dev.md)."""

    node: np.ndarray  # i4 node id (the container for "N small items" / "+N items", -1 free / unknown)
    parent: np.ndarray  # i4 row of the parent, -1: a child of the zoom root
    kind: np.ndarray  # u1 RectKind
    depth: np.ndarray  # u1, 1 = a child of the zoom root
    size: np.ndarray  # i8 area basis (bytes, or what the visualization counts)
    count: np.ndarray  # u4 members of an aggregate
    u0: np.ndarray  # f8 interval of the zoom root's [0, 1): siblings in order, sized by size
    u1: np.ndarray
    root_node: int
    root_kind: int
    root_size: int
    width: int  # canvas, physical px
    height: int
    dpr: float
    font_scale: float
    min_px: float  # the minimum element size, physical px
    levels: int
    options: tuple[tuple[str, object], ...] = ()

    def __len__(self) -> int:
        return len(self.node)

    def digest(self) -> bytes:
        """Equal for two hierarchies the placer would answer alike."""
        h = hashlib.blake2b(digest_size=16)
        for col in (self.node, self.parent, self.kind, self.size, self.count):
            h.update(np.ascontiguousarray(col).tobytes())
        h.update(
            repr(
                (
                    self.root_node,
                    self.root_kind,
                    self.root_size,
                    self.width,
                    self.height,
                    self.dpr,
                    self.font_scale,
                    self.min_px,
                    self.levels,
                    self.options,
                )
            ).encode()
        )
        return h.digest()

    def params(self) -> dict[str, object]:
        """The JSON half of a plugin call (the columns travel as a binary frame)."""
        return {
            "width": self.width,
            "height": self.height,
            "dpr": self.dpr,
            "font_scale": self.font_scale,
            "min_element": self.min_px,
            "levels": self.levels,
            "root": {"node": self.root_node, "kind": self.root_kind, "size": self.root_size},
        }

    def columns(self) -> dict[str, np.ndarray]:
        return {
            "node": self.node,
            "parent": self.parent,
            "kind": self.kind,
            "depth": self.depth,
            "size": self.size,
            "count": self.count,
            "u0": self.u0,
            "u1": self.u1,
        }


@dataclass(slots=True)
class Placed:
    """A placer's answer, row for row (f8 arrays of ``len(hierarchy)``; a polygon's points in ``points``)."""

    shape: np.ndarray  # u1 NONE / BOX / POLYGON
    x: np.ndarray  # bounding box (BOX: the rounded rectangle; POLYGON: worked out from its points)
    y: np.ndarray
    w: np.ndarray
    h: np.ndarray
    r: np.ndarray  # BOX: corner radius (half the shorter side: a circle or a pill)
    poly: np.ndarray  # u1 points of the row's polygon (0 for the rest)
    points: np.ndarray  # f8 x, y pairs of the polygons, in row order
    label: np.ndarray  # u1 AUTO / NO_LABEL / CENTRE / TOP
    root: tuple[float, float, float, float, float, tuple[float, ...]] | None = None  # x y w h r points
    message: str = ""

    @classmethod
    def empty(cls, n: int, message: str = "") -> Placed:
        z = np.zeros(n, dtype=np.float64)
        u = np.zeros(n, dtype=np.uint8)
        return cls(u, z, z, z, z, z, u, np.zeros(0, dtype=np.float64), u, None, message)


Placer = Callable[[Hierarchy], Placed]


class PlaceError(Exception):
    """The placer failed: the frame says ``message`` instead of a picture."""


def hierarchy(
    draw: DrawList, root: RootSpec, sizes: SizeSource, params: LayoutParams, store_kind: np.ndarray
) -> Hierarchy:
    """The placer's input from a draw list of :class:`ShapesProjection` rows."""
    g = draw.geometry
    if root.members is not None:
        members = np.asarray(root.members, dtype=np.int64)
        tag = int(root.key or 0) & (3 << 30)
        rkind = int(RectKind.MORE if tag == KEY_MORE else RectKind.SMALL)
        rsize = int(np.asarray(sizes.of(members)).sum()) if len(members) else 0
    else:
        node = root.node
        is_dir = 0 <= node < len(store_kind) and int(store_kind[node]) == int(Kind.DIR)
        rkind = int(RectKind.FOLDER if is_dir else RectKind.FILE)
        rsize = int(np.asarray(sizes.of(np.array([node], dtype=np.int64)))[0]) if node < sizes.n else 0
    d = params.dpr
    return Hierarchy(
        node=draw.node.astype(np.int32),
        parent=draw.parent.astype(np.int32),
        kind=draw.kind.astype(np.uint8),
        depth=draw.depth.astype(np.uint8),
        size=draw.size.astype(np.int64),
        count=draw.count.astype(np.uint32),
        u0=g["u0"].astype(np.float64) / SPAN,
        u1=g["u1"].astype(np.float64) / SPAN,
        root_node=int(root.node),
        root_kind=rkind,
        root_size=rsize,
        width=round(params.width * d),
        height=round(params.height * d),
        dpr=float(d),
        font_scale=float(params.font_scale),
        min_px=float(params.min_element * d),
        levels=int(draw.levels),
        options=params.options,
    )


class ShapesEngine:
    """Lays out one store for one plugin visualization (an Engine): the hierarchy here, the shapes
    from ``place``; answers cached by digest."""

    def __init__(
        self,
        basis: Basis,
        visual: str,
        place: Placer,
        *,
        fill: float = 0.75,
        max_rows: int = DEFAULT_ROWS,
        nested: bool = True,
        key: Callable[[], object] = lambda: None,
    ) -> None:
        self.basis = basis
        self.visual = visual
        self.place = place
        self.nested = nested
        self.key = key  # what else the answer depends on (the plugin's settings, its cache epoch)
        self.parts = PartitionEngine(basis, lambda p: ShapesProjection(p, fill), pv.SHAPES_LAYOUT, visual)
        self.parts.max_rows = max(1, min(int(max_rows), MAX_ROWS))
        self._cache: OrderedDict[tuple[bytes, object], Placed] = OrderedDict()
        self._lock = threading.Lock()

    def levels(self, params: LayoutParams) -> int:
        return self.parts.levels(params)

    def run(self, params: LayoutParams, root: RootSpec, sizes: SizeSource) -> DrawList:
        t0 = time.perf_counter()
        draw = self.parts.run(params, root, sizes)
        h = hierarchy(draw, root, sizes, params, self.basis.store.cols["kind"])
        ck = (h.digest(), self.key())
        with self._lock:
            placed = self._cache.get(ck)
            if placed is not None:
                self._cache.move_to_end(ck)
        if placed is None:
            try:
                placed = self.place(h)
            except PlaceError as exc:
                placed = Placed.empty(len(h), str(exc))
            if not placed.message:  # a failure is asked again next time
                with self._lock:
                    self._cache[ck] = placed
                    while len(self._cache) > CACHE:
                        self._cache.popitem(last=False)
        out = assemble(draw, h, placed, self.basis, nested=self.nested)
        out.elapsed_s = time.perf_counter() - t0
        return out


def check_placed(placed: Placed, n: int) -> str | None:
    """Why ``placed`` does not fit a hierarchy of ``n`` rows (None: it does)."""
    for name in ("shape", "x", "y", "w", "h", "r", "poly", "label"):
        col = np.asarray(getattr(placed, name)).reshape(-1)
        if col.shape != (n,):
            return f"{name} has {col.shape[0]} values for {n} blocks"
    poly = np.asarray(placed.poly, dtype=np.int64)
    shape = np.asarray(placed.shape, dtype=np.int64)
    used = int(np.where(shape == POLYGON, poly, 0).sum())
    if np.asarray(placed.points).size != 2 * used:
        return f"{np.asarray(placed.points).size} point coordinates for {used} points"
    return None


def convex(pts: np.ndarray) -> bool:
    """A simple convex polygon (either orientation; collinear points allowed)."""
    k = len(pts)
    if k < 3:
        return False
    e = np.roll(pts, -1, axis=0) - pts
    cross = e[:, 0] * np.roll(e[:, 1], -1) - e[:, 1] * np.roll(e[:, 0], -1)
    if not (np.all(cross >= -1e-9) or np.all(cross <= 1e-9)):
        return False
    area = 0.5 * float(np.sum(pts[:, 0] * np.roll(pts[:, 1], -1) - np.roll(pts[:, 0], -1) * pts[:, 1]))
    if abs(area) < 0.5:
        return False
    # the edges turn once around: a star polygon has the same sign everywhere but turns twice
    ang = np.arctan2(e[:, 1], e[:, 0])
    turn = np.diff(np.concatenate([ang, ang[:1]]))
    turn = (turn + np.pi) % (2 * np.pi) - np.pi
    return abs(abs(float(turn.sum())) - 2 * np.pi) < 1e-3


def band_width(kind: str, w: float, h: float, r: float, top: float, bottom: float) -> float:
    """Width of the shape between ``top`` and ``bottom`` (px from the shape's top edge): the narrower
    end of that horizontal band. ``kind``: "box" (a rounded rectangle of radius r) or "poly" (the
    ellipse in the box, POLY_INSET of it). Mirrored by visual/shapes/geometry.ts bandWidth."""
    if top < 0 or bottom > h or bottom <= top:
        return 0.0
    if kind == "poly":
        half = h / 2
        dy = max(abs(top - half), abs(bottom - half))
        return POLY_INSET * w * math.sqrt(max(0.0, 1 - (dy / half) ** 2)) if half > 0 else 0.0
    r = max(0.0, min(r, w / 2, h / 2))
    worst = 0.0  # how far into a corner's height the band reaches
    if top < r:
        worst = max(worst, r - top)
    if bottom > h - r:
        worst = max(worst, bottom - (h - r))
    if worst <= 0:
        return w
    return w - 2 * (r - math.sqrt(max(0.0, r * r - worst * worst)))


def label_bits(kind: str, w: float, h: float, r: float, mode: int, lu: float) -> int:
    """Label bits of a shape (docs/plugin-dev.md 「标签」): a centred label of two lines (name and
    size; large type in a big shape), of one, or a top label (name only), wherever it fits."""
    if mode == NO_LABEL or w <= 0 or h <= 0:
        return 0
    if mode == TOP:
        t = TOP_INSET * lu
        cw = band_width(kind, w, h, r, t, t + TOP_BAND * lu)
        return L_NAME | L_TOP if cw >= 40 * lu and h >= 60 * lu else 0
    mid = h / 2
    two = TWO_LINES * lu
    cw = band_width(kind, w, h, r, mid - two / 2, mid + two / 2)
    if cw >= 42 * lu:
        big = 44 * lu
        cb = band_width(kind, w, h, r, mid - big / 2, mid + big / 2)
        return L_NAME | L_SIZE | (L_BIG if cb >= 120 * lu and cb * h > 52_000 * lu * lu else 0)
    one = ONE_LINE * lu
    cw = band_width(kind, w, h, r, mid - one / 2, mid + one / 2)
    return L_NAME | L_SHORT if cw >= 30 * lu and h >= 15 * lu else 0


def assemble(draw: DrawList, h: Hierarchy, placed: Placed, basis: Basis, *, nested: bool = True) -> DrawList:
    """The ``shapes.v1`` draw list: the hierarchy's rows that got a shape, with their geometry,
    label bits and names. A row without a shape takes its contents with it; a folder that keeps no
    row inside becomes a collapsed one."""
    n = len(draw)
    message = placed.message
    problem = check_placed(placed, n)
    if problem is not None:
        message = message or problem
        placed = Placed.empty(n, message)
    shape = np.asarray(placed.shape, dtype=np.int64).reshape(-1)
    x, y, w, hh, r = (
        np.nan_to_num(np.asarray(getattr(placed, k), dtype=np.float64).reshape(-1)).copy() for k in "xywhr"
    )
    x, y = np.clip(x, -MAX_COORD, MAX_COORD), np.clip(y, -MAX_COORD, MAX_COORD)
    w, hh = np.clip(w, 0, MAX_COORD), np.clip(hh, 0, MAX_COORD)
    poly_n = np.where(shape == POLYGON, np.asarray(placed.poly, dtype=np.int64).reshape(-1), 0)
    pts_all = np.nan_to_num(np.asarray(placed.points, dtype=np.float64).reshape(-1, 2))
    pts_all = np.clip(pts_all, -MAX_COORD, MAX_COORD)
    starts = np.concatenate([[0], np.cumsum(poly_n)]).astype(np.int64)
    keep = np.zeros(n, dtype=bool)
    keep[(shape == BOX) & (w >= 1) & (hh >= 1)] = True
    polys: dict[int, np.ndarray] = {}
    for i in np.flatnonzero(shape == POLYGON).tolist():
        k = int(poly_n[i])
        if not 3 <= k <= pv.MAX_POLY_POINTS:
            continue
        # whole pixels first: a polygon that stops being convex when rounded is not drawn either
        pts = np.floor(pts_all[starts[i] : starts[i] + k] + 0.5)
        if not convex(pts):
            continue
        x0, y0 = pts.min(axis=0)
        x1, y1 = pts.max(axis=0)
        if x1 - x0 < 1 or y1 - y0 < 1:
            continue
        x[i], y[i], w[i], hh[i], r[i] = x0, y0, x1 - x0, y1 - y0, 0.0
        polys[i] = pts
        keep[i] = True
    # a row without a shape takes what lies below it along (rows come before their children)
    parent = draw.parent.astype(np.int64)
    depth = draw.depth.astype(np.int64)
    for d in range(2, int(depth.max()) + 1 if n else 0):
        rows = np.flatnonzero(depth == d)
        keep[rows] &= keep[parent[rows]]
    rows = np.flatnonzero(keep)
    new = np.full(n, -1, dtype=np.int64)
    new[rows] = np.arange(len(rows))
    par = np.where(parent[rows] >= 0, new[np.maximum(parent[rows], 0)], -1)
    kind = draw.kind[rows].copy()
    has_child = np.zeros(len(rows), dtype=bool)
    has_child[par[par >= 0]] = True
    kind[(kind == FOLDER) & ~has_child] = COLLAPSED
    # whole pixels: the box's corners rounded, so neighbours that meet keep meeting
    xi = np.floor(x[rows] + 0.5)
    yi = np.floor(y[rows] + 0.5)
    wi = np.maximum(1, np.floor(x[rows] + w[rows] + 0.5) - xi)
    hi = np.maximum(1, np.floor(y[rows] + hh[rows] + 0.5) - yi)
    ri = np.clip(np.floor(r[rows] + 0.5), 0, np.floor(np.minimum(wi, hi) / 2))
    is_poly = np.array([int(i) in polys for i in rows.tolist()], dtype=bool)
    # a circle stays one: an even diameter around its rounded centre
    rf, wf, hf = r[rows], w[rows], hh[rows]
    circle = ~is_poly & (np.abs(wf - hf) < 0.5) & (2 * rf >= np.minimum(wf, hf) - 0.5)
    if circle.any():
        rad = np.maximum(1, np.floor(wf[circle] / 2 + 0.5))
        cx = np.floor(x[rows][circle] + wf[circle] / 2 + 0.5)
        cy = np.floor(y[rows][circle] + hf[circle] / 2 + 0.5)
        xi[circle], yi[circle] = cx - rad, cy - rad
        wi[circle] = hi[circle] = 2 * rad
        ri[circle] = rad
    pcount = np.where(is_poly, poly_n[rows], 0)
    points = (
        np.concatenate([polys[int(i)].reshape(-1) for i in rows[is_poly].tolist()])
        if is_poly.any()
        else np.zeros(0)
    ).astype(np.int32)
    # labels
    lu = h.dpr * h.font_scale
    mode = np.asarray(placed.label, dtype=np.int64).reshape(-1)[rows] if n else np.zeros(0, dtype=np.int64)
    label = np.zeros(len(rows), dtype=np.uint8)
    names: list[str] = []
    store = basis.store
    for j in range(len(rows)):
        k = int(kind[j])
        m = int(mode[j])
        if m == AUTO:
            # an open folder: in a nested layout a header-like name at the top of a rectangle (a
            # circle or a polygon is filled by its contents), elsewhere a label like any block
            boxy = not is_poly[j] and ri[j] * 2 < min(wi[j], hi[j]) - 1
            m = (TOP if boxy else NO_LABEL) if k == FOLDER and nested else CENTRE
        bits = label_bits("poly" if is_poly[j] else "box", float(wi[j]), float(hi[j]), float(ri[j]), m, lu)
        label[j] = bits
        node = int(draw.node[rows[j]])
        names.append(store.name(node) if bits and k <= PENDING_RECT and node >= 0 else "")
    members = {
        int(draw.key[i]): draw.members[int(draw.key[i])]
        for i in rows.tolist()
        if int(draw.key[i]) in draw.members
    }
    root_shape = None
    if placed.root is not None and message == "":
        rx, ry, rw, rh, rr, rp = placed.root
        root_shape = _root_shape(rx, ry, rw, rh, rr, rp)
    meta = pv.ShapesGeometry(root=root_shape, nested=nested, message=message[:300])
    out = DrawList(
        key=draw.key[rows].copy(),
        node=draw.node[rows].copy(),
        parent=par.astype(np.int32),
        kind=kind.astype(np.uint8),
        depth=draw.depth[rows].copy(),
        cls=draw.cls[rows].copy(),
        label=label,
        size=draw.size[rows].copy(),
        count=draw.count[rows].copy(),
        names=names,
        geometry={
            "x": xi.astype(np.int32),
            "y": yi.astype(np.int32),
            "w": wi.astype(np.int32),
            "h": hi.astype(np.int32),
            "r": ri.astype(np.int32),
            "poly": pcount.astype(np.int32),
        },
        members=members,
        width=draw.width,
        height=draw.height,
        layout=pv.SHAPES_LAYOUT,
        visual=draw.visual,
        meta=meta,
        levels=draw.levels,
        points=points,
    )
    return out


def _clip(v: float) -> int:
    return int(max(-MAX_COORD, min(MAX_COORD, math.floor(v + 0.5))))


def _root_shape(
    x: float, y: float, w: float, h: float, r: float, pts: tuple[float, ...]
) -> pv.PixelShape | None:
    vals = [x, y, w, h, r, *pts]
    if not all(math.isfinite(v) for v in vals):
        return None
    if pts:
        arr = np.floor(np.asarray(pts, dtype=np.float64).reshape(-1, 2) + 0.5)
        if not 3 <= len(arr) <= pv.MAX_POLY_POINTS or not convex(arr):
            return None
        x0, y0 = arr.min(axis=0)
        x1, y1 = arr.max(axis=0)
        return pv.PixelShape(
            x=_clip(x0),
            y=_clip(y0),
            w=max(1, _clip(x1 - x0)),
            h=max(1, _clip(y1 - y0)),
            points=[_clip(v) for v in pts],
        )
    if w < 1 or h < 1:
        return None
    xi, yi = _clip(x), _clip(y)
    wi, hi = max(1, _clip(x + w) - xi), max(1, _clip(y + h) - yi)
    return pv.PixelShape(x=xi, y=yi, w=wi, h=hi, r=max(0, min(_clip(r), min(wi, hi) // 2)))


__all__ = [
    "AUTO",
    "BOX",
    "CENTRE",
    "NONE",
    "NO_LABEL",
    "POLYGON",
    "TOP",
    "Hierarchy",
    "PlaceError",
    "Placed",
    "Placer",
    "ShapesEngine",
    "ShapesProjection",
    "assemble",
    "band_width",
    "check_placed",
    "convex",
    "hierarchy",
    "label_bits",
]

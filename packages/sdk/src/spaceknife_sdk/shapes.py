"""Visualizations with a layout of their own (``@plugin.layout``): circle packing, icicles, cells.

The host picks what to draw: the blocks of the zoom root's subtree that are large enough, small
items merged as in the built-in views, at most ``max_rows`` of them, parents first. Your function
says where each goes and answers a :class:`Shapes`; the host draws them with everything a
built-in visualization has (hover, selection, labels, colour modes, zoom, themes, the legend,
screenshots). Coordinates are physical pixels of a ``width`` × ``height`` canvas.

>>> @plugin.layout("pack", T("圆堆积", "Circle packing"), fill=0.5)
... def pack(v: LayoutInput) -> Shapes:
...     return shapes.pack(v)                     # the ready-made circle packing
>>> @plugin.layout("bars", T("冰柱图", "Icicle"), fill=1.0, nested=False)
... def bars(v: LayoutInput) -> Shapes:
...     out = v.shapes()
...     band = v.height / (v.levels + 1)
...     out.rect(-1, 0, 0, v.width, band - 2)     # -1: the zoom root itself
...     x, w = v.u0 * v.width, (v.u1 - v.u0) * v.width
...     out.rects(np.arange(len(v)), x, v.depth * band, w - 2, band - 2)
...     return out
"""

from __future__ import annotations

import math
import random
from collections.abc import Sequence
from dataclasses import dataclass, field
from enum import IntEnum
from typing import TYPE_CHECKING, Any, Literal

import numpy as np

from spaceknife_protocol.view import MAX_POLY_POINTS

if TYPE_CHECKING:
    from .context import Context
    from .tree import TreeView

ROOT = -1  # the zoom root, where a block index is asked for
LabelMode = Literal["auto", "none", "centre", "center", "top"]
_LABEL = {"auto": 0, "none": 1, "centre": 2, "center": 2, "top": 3}
NONE, BOX, POLYGON = 0, 1, 2


class BlockKind(IntEnum):
    """What a block is (the host's row kinds)."""

    FILE = 0
    FOLDER = 1  # a folder whose blocks follow (they have it as parent)
    COLLAPSED = 2  # a folder drawn as one block (too small, or the last level)
    PENDING = 3  # a folder not listed yet (a scan is running)
    SMALL = 4  # "N small items" of a folder (node: the folder)
    MORE = 5  # "+N items" of a folder
    FREE = 6  # free space of the volume (node -1)
    UNKNOWN = 7  # space the scan could not see


@dataclass(slots=True)
class LayoutInput:
    """What a layout function gets: the blocks to place and the canvas; it answers a :class:`Shapes`.

    Block ``i``: ``node[i]`` its node id (-1: free or unknown space; the folder for "N small items"
    and "+N items"), ``parent[i]`` the block it lies in (-1: the zoom root), ``kind[i]`` a
    :class:`BlockKind`, ``depth[i]`` its level (1: on the zoom root), ``size[i]`` what its area
    stands for (bytes on disk, or the visualization's ``size``), ``count[i]`` the members of an
    aggregate, ``u0[i]``…``u1[i]`` its interval of the zoom root's [0, 1): siblings in order, each
    as long as its share (an icicle's x). Parents come before their blocks.

    ``width`` × ``height``: the canvas in physical pixels (``dpr`` of them per CSS pixel);
    ``min_element``: the smallest block worth drawing, physical px; ``levels``: how deep the blocks
    go; ``root``: the zoom root's node (``root_kind``, ``root_size`` as for a block); ``options``:
    the values of the settings the visualization's ``options`` names; ``ctx``: the call's context
    (``ctx.check()``; ``ctx.tree`` with ``tree.read``)."""

    node: np.ndarray
    parent: np.ndarray
    kind: np.ndarray
    depth: np.ndarray
    size: np.ndarray
    count: np.ndarray
    u0: np.ndarray
    u1: np.ndarray
    width: int = 0
    height: int = 0
    dpr: float = 1.0
    font_scale: float = 1.0
    min_element: float = 14.0
    levels: int = 0
    root: int = 0
    root_kind: int = int(BlockKind.FOLDER)
    root_size: int = 0
    options: dict[str, Any] = field(default_factory=dict)
    ctx: Context | None = None
    _kids: tuple[np.ndarray, np.ndarray] | None = field(default=None, repr=False)

    def __len__(self) -> int:
        return len(self.node)

    @property
    def tree(self) -> TreeView:
        """The scan's tree (needs ``tree.read``): names, times, anything beyond the blocks."""
        if self.ctx is None:
            raise RuntimeError("this layout input has no context")
        return self.ctx.tree

    def children(self, i: int = ROOT) -> np.ndarray:
        """Indices of the blocks lying in block ``i`` (``ROOT``: on the zoom root), in order."""
        if self._kids is None:
            p = np.asarray(self.parent, dtype=np.int64) + 1  # the root's blocks first
            order = np.argsort(p, kind="stable")
            start = np.searchsorted(p[order], np.arange(len(p) + 2))
            self._kids = (order, start)
        order, start = self._kids
        k = int(i) + 1
        if not 0 <= k < len(start) - 1:
            return np.zeros(0, dtype=np.int64)
        return order[start[k] : start[k + 1]]

    def shapes(self) -> Shapes:
        """An empty answer for these blocks (a block left without a shape is not drawn)."""
        return Shapes(len(self))

    @classmethod
    def from_wire(
        cls, params: dict[str, Any], cols: dict[str, np.ndarray], ctx: Context | None
    ) -> LayoutInput:
        root = params.get("root") or {}

        def col(name: str, dt: Any) -> np.ndarray:
            v = cols.get(name)
            return np.zeros(0, dtype=dt) if v is None else np.asarray(v, dtype=dt)

        return cls(
            node=col("node", np.int64),
            parent=col("parent", np.int64),
            kind=col("kind", np.int64),
            depth=col("depth", np.int64),
            size=col("size", np.int64),
            count=col("count", np.int64),
            u0=col("u0", np.float64),
            u1=col("u1", np.float64),
            width=int(params.get("width", 0)),
            height=int(params.get("height", 0)),
            dpr=float(params.get("dpr", 1.0)),
            font_scale=float(params.get("font_scale", 1.0)),
            min_element=float(params.get("min_element", 14.0)),
            levels=int(params.get("levels", 0)),
            root=int(root.get("node", 0)),
            root_kind=int(root.get("kind", int(BlockKind.FOLDER))),
            root_size=int(root.get("size", 0)),
            options=dict(params.get("options") or {}),
            ctx=ctx,
        )


class Shapes:
    """A layout function's answer: one shape per block, or none (then neither the block nor what
    lies in it is drawn). Index ``ROOT`` (-1) is the zoom root itself: give it a shape and it is
    drawn under its blocks, named and clickable (a click zooms out). Labels: ``"auto"`` (the host
    decides: centred in a block, at the top of an open folder drawn as a rectangle), ``"centre"``,
    ``"top"`` or ``"none"``; the host draws a label only where it fits."""

    def __init__(self, n: int) -> None:
        self.n = n
        self.shape = np.zeros(n, dtype=np.uint8)
        self.x = np.zeros(n, dtype=np.float64)
        self.y = np.zeros(n, dtype=np.float64)
        self.w = np.zeros(n, dtype=np.float64)
        self.h = np.zeros(n, dtype=np.float64)
        self.r = np.zeros(n, dtype=np.float64)
        self.label = np.zeros(n, dtype=np.uint8)
        self._polys: dict[int, np.ndarray] = {}
        self.root: tuple[float, float, float, float, float, tuple[float, ...]] | None = None
        self.message = ""

    def __len__(self) -> int:
        return self.n

    def _at(self, i: int) -> int:
        i = int(i)
        if not 0 <= i < self.n:
            raise IndexError(f"block {i} of {self.n}")
        return i

    def rect(
        self,
        i: int,
        x: float,
        y: float,
        w: float,
        h: float,
        *,
        radius: float = 0.0,
        label: LabelMode = "auto",
    ) -> None:
        """Block ``i`` (``ROOT``: the zoom root) as a rectangle from (x, y), w × h, corners rounded
        by ``radius`` (half the shorter side: a pill, or a circle when w = h)."""
        if int(i) == ROOT:
            self.root = (float(x), float(y), float(w), float(h), float(radius), ())
            return
        i = self._at(i)
        self.shape[i] = BOX
        self.x[i], self.y[i], self.w[i], self.h[i] = x, y, w, h
        self.r[i] = max(0.0, min(float(radius), float(w) / 2, float(h) / 2))
        self.label[i] = _LABEL[label]
        self._polys.pop(i, None)

    def circle(self, i: int, cx: float, cy: float, r: float, *, label: LabelMode = "auto") -> None:
        """Block ``i`` (``ROOT``: the zoom root) as a circle around (cx, cy)."""
        self.rect(i, cx - r, cy - r, 2 * r, 2 * r, radius=r, label=label)

    def polygon(
        self, i: int, points: Sequence[tuple[float, float]] | np.ndarray, *, label: LabelMode = "auto"
    ) -> None:
        """Block ``i`` (``ROOT``: the zoom root) as a convex polygon of 3 to 12 points (either
        orientation; a Voronoi cell, a hexagon). A polygon that is not convex is not drawn."""
        pts = np.asarray(points, dtype=np.float64).reshape(-1, 2)
        if not 3 <= len(pts) <= MAX_POLY_POINTS:
            raise ValueError(f"a polygon has 3 to {MAX_POLY_POINTS} points (got {len(pts)})")
        if int(i) == ROOT:
            self.root = (0.0, 0.0, 0.0, 0.0, 0.0, tuple(pts.reshape(-1).tolist()))
            return
        i = self._at(i)
        x0, y0 = pts.min(axis=0)
        x1, y1 = pts.max(axis=0)
        self.shape[i] = POLYGON
        self.x[i], self.y[i], self.w[i], self.h[i], self.r[i] = x0, y0, x1 - x0, y1 - y0, 0.0
        self.label[i] = _LABEL[label]
        self._polys[i] = pts

    def rects(
        self, i: Any, x: Any, y: Any, w: Any, h: Any, radius: Any = 0.0, *, label: LabelMode = "auto"
    ) -> None:
        """Many blocks at once (arrays of the same length, or numbers)."""
        idx = np.asarray(i, dtype=np.int64).reshape(-1)
        if idx.size and (idx.min() < 0 or idx.max() >= self.n):
            raise IndexError("rects: block index out of range (the root takes rect(-1, …))")
        b = np.broadcast_arrays(*(np.asarray(v, dtype=np.float64) for v in (x, y, w, h, radius)), idx)
        xs, ys, ws, hs, rs = (a.reshape(-1) for a in b[:5])
        self.shape[idx] = BOX
        self.x[idx], self.y[idx], self.w[idx], self.h[idx] = xs, ys, ws, hs
        self.r[idx] = np.clip(rs, 0, np.minimum(ws, hs) / 2)
        self.label[idx] = _LABEL[label]
        for k in idx.tolist():
            self._polys.pop(k, None)

    def circles(self, i: Any, cx: Any, cy: Any, r: Any, *, label: LabelMode = "auto") -> None:
        """Many circles at once."""
        cx, cy, r = (np.asarray(v, dtype=np.float64) for v in (cx, cy, r))
        self.rects(i, cx - r, cy - r, 2 * r, 2 * r, r, label=label)

    def clear(self, i: int) -> None:
        """Block ``i`` is not drawn after all (nor what lies in it)."""
        i = self._at(i)
        self.shape[i] = NONE
        self._polys.pop(i, None)

    # ------------------------------------------------------------- wire

    def to_wire(self) -> tuple[dict[str, Any], dict[str, np.ndarray], np.ndarray]:
        """(JSON value, row columns, polygon points) as the runtime sends them."""
        poly = np.zeros(self.n, dtype=np.uint8)
        parts: list[np.ndarray] = []
        for i in np.flatnonzero(self.shape == POLYGON).tolist():
            pts = self._polys[i]
            poly[i] = len(pts)
            parts.append(pts.reshape(-1))
        points = np.concatenate(parts).astype(np.float64) if parts else np.zeros(0, dtype=np.float64)
        cols: dict[str, np.ndarray] = {
            "shape": self.shape,
            "x": self.x,
            "y": self.y,
            "w": self.w,
            "h": self.h,
            "r": self.r,
            "poly": poly,
            "label": self.label,
        }
        value: dict[str, Any] = {
            "root": [*self.root[:5], list(self.root[5])] if self.root else None,
            "message": self.message,
        }
        return value, cols, points


# --------------------------------------------------------------------------- circle packing


def pack(v: LayoutInput, *, padding: float = 2.0, margin: float = 8.0) -> Shapes:
    """Circle packing (as d3-hierarchy's ``pack``): every folder a circle holding its blocks' circles,
    each block's area by its size. ``padding``: CSS px between circles and inside a folder's edge;
    ``margin``: CSS px around the zoom root's circle. A circle under one pixel is left out."""
    out = v.shapes()
    pad = max(0.0, padding) * v.dpr
    R = max(1.0, min(v.width, v.height) / 2 - max(0.0, margin) * v.dpr)
    cx, cy = v.width / 2, v.height / 2
    out.circle(ROOT, cx, cy, R)
    pos = np.zeros((len(v), 3), dtype=np.float64)  # cx, cy, r of each block's circle
    have = np.zeros(len(v), dtype=bool)
    queue: list[tuple[int, float, float, float]] = [(ROOT, cx, cy, R)]
    while queue:
        c, x0, y0, r0 = queue.pop()
        kids = v.children(c)
        if not len(kids) or r0 - pad <= 1:
            continue
        if v.ctx is not None:
            v.ctx.check()
        radii = np.sqrt(np.maximum(v.size[kids].astype(np.float64), 0.0))
        if not (radii > 0).any():
            continue
        xs, ys, er = pack_siblings(radii.tolist())
        s = (r0 - pad) / er if er > 0 else 0.0
        for k, x, y, r in zip(kids.tolist(), xs, ys, radii.tolist(), strict=True):
            rr = r * s - pad / 2
            if rr < 1:
                continue
            pos[k] = (x0 + x * s, y0 + y * s, rr)
            have[k] = True
            if v.kind[k] == BlockKind.FOLDER:
                queue.append((k, x0 + x * s, y0 + y * s, rr))
    idx = np.flatnonzero(have)
    out.circles(idx, pos[idx, 0], pos[idx, 1], pos[idx, 2])
    return out


def _place(
    bx: float, by: float, br: float, ax: float, ay: float, ar: float, cr: float
) -> tuple[float, float]:
    """Centre of a circle of radius ``cr`` touching circles b and a (d3's ``place``)."""
    dx = bx - ax
    dy = by - ay
    d2 = dx * dx + dy * dy
    if d2:
        a2 = (ar + cr) ** 2
        b2 = (br + cr) ** 2
        if a2 > b2:
            x = (d2 + b2 - a2) / (2 * d2)
            y = math.sqrt(max(0.0, b2 / d2 - x * x))
            return bx - x * dx - y * dy, by - x * dy + y * dx
        x = (d2 + a2 - b2) / (2 * d2)
        y = math.sqrt(max(0.0, a2 / d2 - x * x))
        return ax + x * dx - y * dy, ay + x * dy + y * dx
    return ax + cr, ay


def pack_siblings(radii: Sequence[float]) -> tuple[list[float], list[float], float]:
    """Circles of these radii side by side without overlaps, each touching two placed ones (the
    front-chain algorithm of Wang et al., 2006, as d3 does it): their centres around the origin,
    which is the centre of the smallest circle around them all, and that circle's radius."""
    r = [float(v) for v in radii]
    n = len(r)
    xs = [0.0] * n
    ys = [0.0] * n
    if n == 0:
        return xs, ys, 0.0
    if n == 1:
        return xs, ys, r[0]
    xs[0] = -r[1]
    xs[1] = r[0]
    if n == 2:
        return xs, ys, r[0] + r[1]
    xs[2], ys[2] = _place(xs[1], ys[1], r[1], xs[0], ys[0], r[0], r[2])
    nxt = [0] * n
    prv = [0] * n
    a, b, c = 0, 1, 2
    nxt[a], prv[c], nxt[b], prv[a], nxt[c], prv[b] = b, b, c, c, a, a

    def intersects(p: int, q: int) -> bool:
        dr = r[p] + r[q] - 1e-6
        dx = xs[q] - xs[p]
        dy = ys[q] - ys[p]
        return dr > 0 and dr * dr > dx * dx + dy * dy

    def score(p: int) -> float:
        q = nxt[p]
        ab = r[p] + r[q]
        dx = (xs[p] * r[q] + xs[q] * r[p]) / ab
        dy = (ys[p] * r[q] + ys[q] * r[p]) / ab
        return dx * dx + dy * dy

    i = 3
    while i < n:
        xs[i], ys[i] = _place(xs[a], ys[a], r[a], xs[b], ys[b], r[b], r[i])
        j, k = nxt[b], prv[a]
        sj, sk = r[b], r[a]
        hit = False
        while True:  # the closest circle on the front chain the new one overlaps, if any
            if sj <= sk:
                if intersects(j, i):
                    b = j
                    nxt[a], prv[b] = b, a
                    hit = True
                    break
                sj += r[j]
                j = nxt[j]
            else:
                if intersects(k, i):
                    a = k
                    nxt[a], prv[b] = b, a
                    hit = True
                    break
                sk += r[k]
                k = prv[k]
            if j == nxt[k]:
                break
        if hit:
            continue
        prv[i], nxt[i], nxt[a], prv[b] = a, b, i, i
        b = i
        best = score(a)  # the next circle goes by the pair closest to the centroid
        c = nxt[b]
        while c != b:
            s = score(c)
            if s < best:
                a, best = c, s
            c = nxt[c]
        b = nxt[a]
        i += 1
    chain = [b]
    c = nxt[b]
    while c != b:
        chain.append(c)
        c = nxt[c]
    ex, ey, er = enclose([(xs[p], ys[p], r[p]) for p in chain])
    return [x - ex for x in xs], [y - ey for y in ys], er


Circle = tuple[float, float, float]


def _encloses_not(a: Circle, b: Circle) -> bool:
    dr = a[2] - b[2]
    dx, dy = b[0] - a[0], b[1] - a[1]
    return dr < 0 or dr * dr < dx * dx + dy * dy


def _encloses_weak(a: Circle, b: Circle) -> bool:
    dr = a[2] - b[2] + max(a[2], b[2], 1.0) * 1e-9
    dx, dy = b[0] - a[0], b[1] - a[1]
    return dr > 0 and dr * dr > dx * dx + dy * dy


def _basis2(a: Circle, b: Circle) -> Circle:
    x1, y1, r1 = a
    x2, y2, r2 = b
    x21, y21, r21 = x2 - x1, y2 - y1, r2 - r1
    ln = math.sqrt(x21 * x21 + y21 * y21)
    if ln == 0:
        return (x1, y1, max(r1, r2))
    return ((x1 + x2 + x21 / ln * r21) / 2, (y1 + y2 + y21 / ln * r21) / 2, (ln + r1 + r2) / 2)


def _basis3(a: Circle, b: Circle, c: Circle) -> Circle:
    x1, y1, r1 = a
    x2, y2, r2 = b
    x3, y3, r3 = c
    a2, a3, b2, b3 = x1 - x2, x1 - x3, y1 - y2, y1 - y3
    c2, c3 = r2 - r1, r3 - r1
    d1 = x1 * x1 + y1 * y1 - r1 * r1
    d2 = d1 - x2 * x2 - y2 * y2 + r2 * r2
    d3 = d1 - x3 * x3 - y3 * y3 + r3 * r3
    ab = a3 * b2 - a2 * b3
    if ab == 0:  # collinear centres: the two outermost decide
        return max((_basis2(a, b), _basis2(a, c), _basis2(b, c)), key=lambda e: e[2])
    xa = (b2 * d3 - b3 * d2) / (ab * 2) - x1
    xb = (b3 * c2 - b2 * c3) / ab
    ya = (a3 * d2 - a2 * d3) / (ab * 2) - y1
    yb = (a2 * c3 - a3 * c2) / ab
    qa = xb * xb + yb * yb - 1
    qb = 2 * (r1 + xa * xb + ya * yb)
    qc = xa * xa + ya * ya - r1 * r1
    rr = -(qb + math.sqrt(max(0.0, qb * qb - 4 * qa * qc))) / (2 * qa) if abs(qa) > 1e-6 else -qc / qb
    return (x1 + xa + xb * rr, y1 + ya + yb * rr, rr)


def _extend(basis: list[Circle], p: Circle) -> list[Circle]:
    if all(_encloses_weak(p, b) for b in basis):
        return [p]
    for b in basis:
        if _encloses_not(p, b) and all(_encloses_weak(_basis2(b, p), q) for q in basis):
            return [b, p]
    for i in range(len(basis) - 1):
        for j in range(i + 1, len(basis)):
            bi, bj = basis[i], basis[j]
            if (
                _encloses_not(_basis2(bi, bj), p)
                and _encloses_not(_basis2(bi, p), bj)
                and _encloses_not(_basis2(bj, p), bi)
                and all(_encloses_weak(_basis3(bi, bj, p), q) for q in basis)
            ):
                return [bi, bj, p]
    raise ArithmeticError("no enclosing basis")


def enclose(circles: Sequence[Circle]) -> Circle:
    """The smallest circle around all of these (Welzl's algorithm, as d3's ``packEnclose``)."""
    if not circles:
        return (0.0, 0.0, 0.0)
    items = list(circles)
    random.Random(0x5EED).shuffle(items)  # the same picture every time
    basis: list[Circle] = []
    e: Circle | None = None
    i = guard = 0
    try:
        while i < len(items):
            p = items[i]
            if e is not None and _encloses_weak(e, p):
                i += 1
                continue
            basis = _extend(basis, p)
            e = basis[0] if len(basis) == 1 else _basis2(*basis) if len(basis) == 2 else _basis3(*basis)
            i = 0
            guard += 1
            if guard > 50 * len(items) + 100:
                raise ArithmeticError("no progress")
    except (ArithmeticError, ValueError, ZeroDivisionError):
        # rounding defeated it: a circle around all of them (not the smallest)
        mx = sum(c[0] for c in items) / len(items)
        my = sum(c[1] for c in items) / len(items)
        return (mx, my, max(math.hypot(c[0] - mx, c[1] - my) + c[2] for c in items))
    assert e is not None
    return e


__all__ = ["ROOT", "BlockKind", "LayoutInput", "Shapes", "enclose", "pack", "pack_siblings"]

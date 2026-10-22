"""Polar flame graph (``polar.v1``): the flame graph in polar coordinates, one ring per level.

The zoom root is the centre disc (``PolarGeometry.hole``, drawn from ``FrameInfo.root``); level k is
the k-th ring around it. Angles run in ``ANGLE_UNIT`` = 2^20 units per turn from 12 o'clock,
clockwise on screen, so the largest block starts at the top. Notation: lu = dpr · font scale,
g = max(1, round(2·dpr)), m = min_element · dpr, W × H the canvas in physical pixels
(docs/visuals.md):

- centre (W // 2, H // 2), radius R = min(W, H) // 2 − round(8·dpr);
- centre disc: hole = min(max(round(0.24·R), round(36·lu)), round(0.4·R));
- rings: n = min(detail, (R − hole) // round(14·lu)), pitch ring_w = (R − hole) // n; ring k's cell
  is [hole + (k−1)·ring_w, hole + k·ring_w), drawn from r0 = its inner edge + ⌈g/2⌉ to
  r1 = its outer edge − ⌊g/2⌋;
- angles a0 = round(a·2^20), a1 = round(b·2^20); the gap between neighbouring sectors is not baked
  into them, frontends inset both radial edges (docs/protocol.md §4.4);
- lengths along a ring are arcs at its middle radius: a sector shorter than 2·dpr there is not drawn,
  a folder at least m long shows its children, and the merge threshold of a ring is m over its
  circumference, smaller on every ring outwards;
- labels: with T = r1 − r0, Δ the sector's angle and L = Δ·r_mid − g, a sector thicker than it is
  long names itself along the radius (``label_box(T / lu, (Δ·r0 − g) / lu)``: name, short, radial),
  any other along the ring within the chord that stays round(2·dpr) inside its outer edge
  (``label_box(min(L, chord) / lu, T / lu)`` without the large type).

Constants match the tokens ``visual.flamePolar.*`` (tests/test_visual_tokens.py).
"""

from __future__ import annotations

import math

from spaceknife_protocol import CAP_LAYOUT_POLAR
from spaceknife_protocol import view as pv

from .api import ENGINE_API, LayoutParams, Visualization
from .basis import Basis
from .partition import Cell, PartitionEngine
from .registry import register
from .rows import GEOMETRY_COLUMNS, L_NAME, L_RADIAL, L_SHORT, label_box

MARGIN = 8  # CSS px between the outer ring and the canvas edge (selection ring, shadow)
HOLE_RATIO = 0.24  # centre disc radius as a share of the radius ...
HOLE_MIN = 36  # ... at least this many CSS px × font scale (the zoom root's name and size) ...
HOLE_MAX_RATIO = 0.4  # ... and at most this share
RING_MIN = 14  # CSS px × font scale: the thinnest ring
GAP = 2  # CSS px between rings and between neighbouring sectors

UNIT = pv.ANGLE_UNIT
TURN = 2 * math.pi


class FlamePolarProjection:
    """Rings of one viewport (a :class:`.partition.Projection`)."""

    columns = GEOMETRY_COLUMNS[pv.POLAR_LAYOUT]

    def __init__(self, p: LayoutParams) -> None:
        d = p.dpr
        lu = d * p.font_scale
        W, H = round(p.width * d), round(p.height * d)
        g = max(1, round(GAP * d))
        R = max(0, min(W, H) // 2 - round(MARGIN * d))
        hole = max(0, min(max(round(HOLE_RATIO * R), round(HOLE_MIN * lu)), round(HOLE_MAX_RATIO * R)))
        ring_min = max(1, round(RING_MIN * lu))
        n = max(0, min(p.detail, (R - hole) // ring_min))
        ring_w = (R - hole) // n if n else 0
        self.levels = n
        self.cx, self.cy = W // 2, H // 2
        self.hole = hole
        self.ring_w = ring_w
        self.gap = g
        # per level (0: the centre disc): drawn radii and arc pixels per angle unit at the middle radius
        self.r0 = [0] + [hole + (k - 1) * ring_w + (g + 1) // 2 for k in range(1, n + 1)]
        self.r1 = [hole] + [hole + k * ring_w - g // 2 for k in range(1, n + 1)]
        self.mid = [(r0 + r1) / 2 for r0, r1 in zip(self.r0, self.r1, strict=True)]
        self.arc = [TURN * r / UNIT for r in self.mid]
        self.min_draw = p.min_draw * d
        self.min_expand = p.min_element * d
        self.label_unit = lu
        self.pad = round(2 * d)  # tangential labels keep this far inside the outer edge

    def unit_px(self, level: int) -> float:
        return TURN * self.mid[level]

    def cell(self, a: float, b: float, level: int) -> Cell | None:
        a0, a1 = round(a * UNIT), round(b * UNIT)
        if (a1 - a0) * self.arc[level] < self.min_draw:
            return None
        return (a0, a1, self.r0[level], self.r1[level])

    def expandable(self, cell: Cell, level: int) -> bool:
        return (cell[1] - cell[0]) * self.arc[level] >= self.min_expand

    def label(self, cell: Cell, kind: int) -> int:
        a0, a1, r0, r1 = cell
        lu, g = self.label_unit, self.gap
        angle = (a1 - a0) * TURN / UNIT
        thick = r1 - r0
        mid = (r0 + r1) / 2
        along = angle * mid - g
        if thick > along:  # along the radius: one line, as long as the ring is thick
            bits = label_box(thick / lu, (angle * r0 - g) / lu)
            return L_NAME | L_SHORT | L_RADIAL if bits & L_NAME else 0
        chord = 2 * math.sqrt(max(0.0, (r1 - self.pad) ** 2 - mid**2))
        return label_box(min(along, chord) / lu, thick / lu, big=False)

    def meta(self) -> pv.PolarGeometry:
        return pv.PolarGeometry(
            cx=self.cx,
            cy=self.cy,
            radius=self.hole + self.levels * self.ring_w,
            hole=self.hole,
            ring_w=self.ring_w,
            rings=self.levels,
            gap=self.gap,
            unit=UNIT,
        )


def flame_polar_engine(basis: Basis) -> PartitionEngine:
    return PartitionEngine(basis, FlamePolarProjection, pv.POLAR_LAYOUT, "flame_polar")


FLAME_POLAR = register(
    Visualization(
        id="flame_polar",
        api=ENGINE_API,
        layout=pv.POLAR_LAYOUT,
        capability=CAP_LAYOUT_POLAR,
        title_id="火焰-极坐标图",
        icon="vizFlamePolar",
        order=2,
        engine=flame_polar_engine,
        default_color="branch",
        aliases=("visualization polar", "polar flame graph", "radial", "rings"),
    )
)

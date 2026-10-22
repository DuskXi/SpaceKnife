"""Flame graph (``flame.v1``): the partition layout as stacked bands, one per level.

The zoom root's bar is band 0 (``FlameGeometry.root``, drawn from ``FrameInfo.root``); level k is
band k, growing away from the root's side of the canvas: up from the bottom (the classic flame graph,
the default) or down from the top (option ``origin``, setting ``treemap.flame_origin``). Notation:
lu = dpr · font scale, g = max(1, round(2·dpr)), m = min_element · dpr, W × H the canvas in physical
pixels (docs/visuals.md):

- bands: row_min = round(20·lu), row_max = round(48·lu); n = min(detail, H // row_min − 1) levels;
  the band pitch is row_h = min(H // (n + 1), row_max), a bar is row_h − g high;
- band k lies at y = k·row_h (root on top) or y = H − k·row_h − (row_h − g) (root at the bottom);
- along x the interval [a, b) covers the width plus one gap: x = round(a·(W + g)),
  w = round(b·(W + g)) − g − x, so the rightmost bar ends at W and a child's right edge meets its
  parent's;
- a bar narrower than round(2·dpr) is not drawn; a folder bar at least m wide shows its children;
- labels: ``label_box(w / lu, h / lu)`` without the large type.

Constants match the tokens ``visual.flame.*`` (tests/test_visual_tokens.py).
"""

from __future__ import annotations

from typing import Literal

from spaceknife_protocol import CAP_LAYOUT_FLAME
from spaceknife_protocol import view as pv

from .api import ENGINE_API, LayoutParams, Visualization
from .basis import Basis
from .partition import Cell, PartitionEngine
from .registry import register
from .rows import GEOMETRY_COLUMNS, label_box

ROW_MIN = 20  # CSS px × font scale: the lowest band still holds a line of names
ROW_MAX = 48  # CSS px × font scale: shallow trees do not stretch bars over tall canvases
GAP = 2  # CSS px between bands and between neighbouring bars


class FlameProjection:
    """Bands of one viewport (a :class:`.partition.Projection`)."""

    columns = GEOMETRY_COLUMNS[pv.FLAME_LAYOUT]

    def __init__(self, p: LayoutParams) -> None:
        d = p.dpr
        lu = d * p.font_scale
        W, H = round(p.width * d), round(p.height * d)
        g = max(1, round(GAP * d))
        row_min = max(1, round(ROW_MIN * lu))
        row_max = max(row_min, round(ROW_MAX * lu))
        n = max(0, min(p.detail, H // row_min - 1))
        row_h = min(H // (n + 1), row_max)
        self.origin: Literal["top", "bottom"] = "top" if p.opt("origin", "bottom") == "top" else "bottom"
        self.levels = n
        self.width = W
        self.gap = g
        self.row_h = row_h
        self.bar_h = max(0, row_h - g)
        self.span = float(W + g)  # pixels of [0, 1): the width plus the gap of the rightmost bar
        self.min_draw = round(p.min_draw * d)
        self.min_expand = p.min_element * d
        self.label_unit = lu
        if self.origin == "top":
            self.band = [k * row_h for k in range(n + 1)]
        else:
            self.band = [H - k * row_h - self.bar_h for k in range(n + 1)]

    def unit_px(self, level: int) -> float:
        return self.span

    def cell(self, a: float, b: float, level: int) -> Cell | None:
        x = round(a * self.span)
        w = round(b * self.span) - self.gap - x
        if w < self.min_draw:
            return None
        return (x, self.band[level], w, self.bar_h)

    def expandable(self, cell: Cell, level: int) -> bool:
        return cell[2] >= self.min_expand

    def label(self, cell: Cell, kind: int) -> int:
        lu = self.label_unit
        return label_box(cell[2] / lu, cell[3] / lu, big=False)

    def meta(self) -> pv.FlameGeometry:
        root = pv.PixelRect(x=0, y=self.band[0], w=self.width, h=self.bar_h)
        return pv.FlameGeometry(
            origin=self.origin, row_h=self.row_h, rows=self.levels, gap=self.gap, root=root
        )


def flame_engine(basis: Basis) -> PartitionEngine:
    return PartitionEngine(basis, FlameProjection, pv.FLAME_LAYOUT, "flame")


FLAME = register(
    Visualization(
        id="flame",
        api=ENGINE_API,
        layout=pv.FLAME_LAYOUT,
        capability=CAP_LAYOUT_FLAME,
        title_id="火焰图",
        icon="vizFlame",
        order=1,
        engine=flame_engine,
        default_color="branch",
        options=(("origin", "treemap.flame_origin"),),
        aliases=("visualization flame", "flame graph", "icicle"),
    )
)

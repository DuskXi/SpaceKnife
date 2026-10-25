"""Two visualizations with a layout of their own (docs/plugin-dev.md 「自带布局的可视化」).

A layout function gets the blocks the host picked to draw (``v``: parents first, each with its
node, kind, level and size) and the canvas in physical pixels; it answers a shape per block. The
host draws them like its own views, so neither function knows about colours, labels or the mouse.
"""

from __future__ import annotations

import math

import numpy as np

from spaceknife_sdk import BlockKind, LayoutInput, Plugin, Shapes, T, shapes

plugin = Plugin()


@plugin.layout(
    "pack",
    T("圆堆积", "Circle packing"),
    icon="icons/pack.svg",
    fill=0.5,  # circles in a circle cover about half of it: blocks are picked by that share
    options=["padding", "shape"],
    default_color="class",
)
def pack(v: LayoutInput) -> Shapes:
    """Every folder a circle holding the circles of its contents (the SDK's circle packing)."""
    out = shapes.pack(v, padding=float(v.options.get("padding", 2)))
    if v.options.get("shape") == "hexagon":
        hexagons(out)
    return out


def hexagons(out: Shapes) -> None:
    """The same packing drawn as hexagons: each circle becomes the hexagon inside it."""
    for i in np.flatnonzero(out.shape != 0).tolist():
        cx, cy, r = out.x[i] + out.w[i] / 2, out.y[i] + out.h[i] / 2, out.w[i] / 2
        out.polygon(i, [(cx + r * math.cos(a), cy + r * math.sin(a)) for a in np.arange(6) * math.pi / 3])


@plugin.layout(
    "icicle",
    T("冰柱图", "Icicle"),
    icon="vizFlame",
    fill=1.0,  # a level's blocks share its whole width
    nested=False,  # a folder's blocks lie below it, not inside
)
def icicle(v: LayoutInput) -> Shapes:
    """The zoom root as a bar across the top and every level a band below it, each block as wide
    as its share (``u0``…``u1``: the host already shares the width out by size)."""
    out = v.shapes()
    gap = 2 * v.dpr
    band = min(v.height / (v.levels + 1), 48 * v.dpr * v.font_scale)
    out.rect(-1, 0, 0, v.width, band - gap, radius=3 * v.dpr)  # -1: the zoom root
    x = v.u0 * (v.width + gap)
    w = (v.u1 - v.u0) * (v.width + gap) - gap
    shown = np.flatnonzero(w >= 2 * v.dpr)  # what is narrower is left out (and what lies below it)
    out.rects(shown, x[shown], v.depth[shown] * band, w[shown], band - gap, 3 * v.dpr)
    # free space is not a level of the tree: keep it out of the picture
    out.shape[v.kind == BlockKind.FREE] = 0
    return out

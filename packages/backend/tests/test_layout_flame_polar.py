"""Polar flame graph projection (polar.v1): rings, angles, thresholds per ring, labels, registration."""

from __future__ import annotations

import math
import random
from typing import Any

import numpy as np
import pytest

from spaceknife.layout import (
    Basis,
    DrawList,
    LayoutParams,
    RectKind,
    RootSpec,
    StoreSizes,
    label_box,
    registry,
)
from spaceknife.layout.contract import check_draw
from spaceknife.layout.engine import KEY_FREE, KEY_MORE, KEY_SMALL, KEY_UNKNOWN
from spaceknife.layout.flame_polar import (
    GAP,
    HOLE_MAX_RATIO,
    HOLE_MIN,
    HOLE_RATIO,
    MARGIN,
    RING_MIN,
    FlamePolarProjection,
    flame_polar_engine,
)
from spaceknife.store.nodes import NodeStore
from spaceknife_protocol import CAP_LAYOUT_POLAR
from spaceknife_protocol import view as pv

from .store_builder import build, find
from .test_layout import random_spec
from .test_layout_golden import tree

MB = 1 << 20
U = pv.ANGLE_UNIT


def run(store: NodeStore, root: RootSpec | None = None, **params: Any) -> DrawList:
    """Lay out ``root`` (default: the scan root) on 1920 × 1080 px; the draw list keeps the contract."""
    root = root or RootSpec(node=1)
    basis = Basis(store)
    dl = flame_polar_engine(basis).run(
        LayoutParams(**({"width": 1920, "height": 1080} | params)), root, StoreSizes(basis)
    )
    assert check_draw(dl, store, root) == []
    return dl


def meta(dl: DrawList) -> pv.PolarGeometry:
    assert isinstance(dl.meta, pv.PolarGeometry)
    return dl.meta


def arc(dl: DrawList, rows: np.ndarray | slice = slice(None)) -> np.ndarray:
    """Arc length at the middle radius, physical px."""
    a0, a1, r0, r1 = (dl.geometry[k][rows].astype(np.float64) for k in ("a0", "a1", "r0", "r1"))
    out: np.ndarray = (a1 - a0) / U * 2 * math.pi * (r0 + r1) / 2
    return out


def row_of(dl: DrawList, store: NodeStore, path: str) -> int:
    return int(np.flatnonzero(dl.node == find(store, path))[0])


# --------------------------------------------------------------------------- rings


@pytest.mark.parametrize(
    ("width", "height", "dpr", "font", "detail"),
    [
        (1920, 1080, 1.0, 1.0, 5),
        (3840, 2160, 1.0, 1.0, 15),
        (1280, 720, 1.25, 1.0, 8),
        (1280, 720, 1.5, 1.4, 15),
        (800, 600, 2.0, 0.8, 15),
        (300, 200, 1.0, 1.0, 15),
        (120, 90, 1.0, 1.0, 5),
        (10, 10, 1.0, 1.0, 5),
    ],
)
def test_rings_and_centre_disc_follow_the_formula(
    width: int, height: int, dpr: float, font: float, detail: int
) -> None:
    p = LayoutParams(width=width, height=height, dpr=dpr, font_scale=font, detail=detail)
    m = FlamePolarProjection(p).meta()
    lu = dpr * font
    W, H = round(width * dpr), round(height * dpr)
    R = max(0, min(W, H) // 2 - round(MARGIN * dpr))
    hole = min(max(round(HOLE_RATIO * R), round(HOLE_MIN * lu)), round(HOLE_MAX_RATIO * R))
    n = max(0, min(detail, (R - hole) // round(RING_MIN * lu)))
    assert (m.cx, m.cy, m.hole, m.rings, m.unit) == (W // 2, H // 2, hole, n, U)
    assert m.gap == max(1, round(GAP * dpr))
    if n:
        assert m.ring_w == (R - hole) // n >= round(RING_MIN * lu)
        assert m.radius == hole + n * m.ring_w <= R
    else:
        assert m.radius == hole
    assert flame_polar_engine(Basis(build({"a.bin": 1}))).levels(p) == n


@pytest.mark.parametrize("dpr", [1.0, 1.25, 1.5, 2.0])
def test_each_level_is_one_ring_with_the_gap_taken_off(dpr: float) -> None:
    store = build(random_spec(random.Random(1)), cluster=4096)
    dl = run(store, width=1280, height=720, dpr=dpr, detail=8, min_element=4)
    m = meta(dl)
    g = m.gap
    for d in np.unique(dl.depth).tolist():
        rows = dl.depth == d
        inner, outer = m.hole + (d - 1) * m.ring_w, m.hole + d * m.ring_w
        assert (dl.geometry["r0"][rows] == inner + (g + 1) // 2).all()
        assert (dl.geometry["r1"][rows] == outer - g // 2).all()


def test_a_canvas_too_small_for_a_ring_draws_nothing() -> None:
    basis = Basis(build({"a.bin": MB}))
    for w, h in ((10, 10), (60, 60)):
        dl = flame_polar_engine(basis).run(
            LayoutParams(width=w, height=h), RootSpec(node=1), StoreSizes(basis)
        )
        assert len(dl) == 0 and dl.levels == meta(dl).rings == 0


# --------------------------------------------------------------------------- angles


def test_sectors_start_at_twelve_o_clock_largest_first() -> None:
    store = build({"b.bin": MB, "C.bin": 3 * MB, "a.bin": 2 * MB, "d.bin": 4 * MB})
    dl = run(store, min_element=1)
    a0, a1 = dl.geometry["a0"], dl.geometry["a1"]
    assert a0[0] == 0 and a1[-1] == U  # the whole turn, clockwise from the top
    assert dl.names == ["d.bin", "C.bin", "a.bin", "b.bin"]
    assert np.array_equal(a0[1:], a1[:-1])  # neighbours share their boundary
    assert np.max(np.abs((a1 - a0) / U - dl.size / dl.size.sum())) * 360 < 0.1  # ±0.1°
    by_name = run(store, min_element=1, sort_by_size=False)
    assert by_name.names == ["a.bin", "b.bin", "C.bin", "d.bin"]  # A→Z clockwise


def test_plus_n_closes_the_turn() -> None:
    spec: dict[str, object] = {"big.bin": 100 * MB}
    spec.update({f"dir{i}": {"x.bin": 10_000} for i in range(50)})
    store = build(spec | {"mid.bin": 30 * MB})
    for by_size in (True, False):
        dl = run(store, sort_by_size=by_size)
        more = dl.row(KEY_MORE | 1)
        assert more is not None and more == np.flatnonzero(dl.parent == -1)[-1]
        assert dl.geometry["a1"][more] == U


def test_a_single_child_is_a_full_turn() -> None:
    store = build({"A": {"B": {"x.bin": 3 * MB, "y.bin": MB}}})
    dl = run(store, min_element=1)
    for name in ("A", "A/B"):
        row = row_of(dl, store, name)
        assert (dl.geometry["a0"][row], dl.geometry["a1"][row]) == (0, U)


@pytest.mark.parametrize("seed", range(6))
@pytest.mark.parametrize("dpr", [1.0, 1.25, 2.0])
def test_sectors_nest_within_their_parent_and_never_overlap(seed: int, dpr: float) -> None:
    store = build(random_spec(random.Random(seed)), cluster=4096)
    dl = run(store, width=1280, height=720, dpr=dpr, detail=8, min_element=4)
    assert len(dl) > 0
    a0, a1, r0, r1 = (dl.geometry[k].astype(np.int64) for k in ("a0", "a1", "r0", "r1"))
    assert ((a0 >= 0) & (a0 < a1) & (a1 <= U)).all()
    inner = dl.parent >= 0
    up = dl.parent[inner]
    assert (a0[inner] >= a0[up]).all() and (a1[inner] <= a1[up]).all() and (r0[inner] > r1[up]).all()
    for d in np.unique(dl.depth).tolist():
        rows = np.flatnonzero(dl.depth == d)
        assert (a0[rows[1:]] >= a1[rows[:-1]]).all()
    top = np.flatnonzero(dl.parent == -1)
    assert int((a1[top] - a0[top]).sum()) <= U
    assert (arc(dl) >= 2 * dpr).all()  # every sector at least min_draw long at its middle radius


def test_siblings_meet_exactly_and_the_last_child_meets_its_parent() -> None:
    spec = {"A": {"a1.bin": 300 * MB, "a2.bin": 100 * MB, "S": {"s.bin": 50 * MB, "t.bin": 50 * MB}}}
    store = build(spec | {"B": {"b1.bin": 200 * MB, "b2.bin": 200 * MB}, "c.bin": 100 * MB})
    dl = run(store, min_element=1, dpr=1.25)
    a0, a1 = dl.geometry["a0"], dl.geometry["a1"]
    for row in [-1, *np.flatnonzero(dl.kind == RectKind.FOLDER).tolist()]:
        kids = np.flatnonzero(dl.parent == row)
        start, end = (0, U) if row < 0 else (int(a0[row]), int(a1[row]))
        assert a0[kids[0]] == start and a1[kids[-1]] == end
        assert np.array_equal(a0[kids[1:]], a1[kids[:-1]])


def test_free_and_unknown_space_lie_in_the_first_ring() -> None:
    store = build({"Users": {"a.bin": 400 * MB}, "Windows": {"b.bin": 100 * MB}})
    dl = run(store, RootSpec(node=1, free=200 * MB, unknown=50 * MB))
    for key in (KEY_FREE, KEY_UNKNOWN):
        row = dl.row(key)
        assert row is not None and dl.depth[row] == 1 and dl.parent[row] == -1


# --------------------------------------------------------------------------- thresholds


def test_the_threshold_shrinks_ring_by_ring() -> None:
    # 140 equal files: at ring 3 (middle radius 328 px at 1080p) each spans 14.7 px, at ring 1
    # (168 px) only 7.5 px: drawn one by one far out, merged near the centre
    store = build({"A": {"B": {f"f{i}.bin": MB for i in range(140)}}})
    far = run(store, min_element=10)
    files = np.flatnonzero(far.kind == RectKind.FILE)
    assert len(files) == 140 and (far.depth[files] == 3).all()
    assert (arc(far, files) >= 10).all()
    near = run(store, RootSpec(node=find(store, "A/B")), min_element=10)
    small = near.row(KEY_SMALL | find(store, "A/B"))
    assert len(near) == 1 and small is not None and near.count[small] == 140


def test_expanding_needs_the_minimum_element_along_the_ring() -> None:
    store = build({"big.bin": 990 * MB, "tiny": {"a.bin": 5 * MB, "b.bin": 5 * MB}})
    for m, kind in ((14, RectKind.COLLAPSED), (8, RectKind.FOLDER)):
        dl = run(store, min_element=m)
        row = row_of(dl, store, "tiny")  # 1 % of the first ring: 10.6 px at its middle radius
        assert dl.kind[row] == kind


# --------------------------------------------------------------------------- labels


def want_label(a0: int, a1: int, r0: int, r1: int, dpr: float, font: float) -> int:
    """docs/protocol.md §4.4."""
    lu, g = dpr * font, max(1, round(2 * dpr))
    delta = (a1 - a0) / U * 2 * math.pi
    t, r_mid = r1 - r0, (r0 + r1) / 2
    length = delta * r_mid - g
    if t > length:
        return 1 | 8 | 16 if label_box(t / lu, (delta * r0 - g) / lu) & 1 else 0
    chord = 2 * math.sqrt(max(0.0, (r1 - round(2 * dpr)) ** 2 - r_mid**2))
    return label_box(min(length, chord) / lu, t / lu, big=False)


@pytest.mark.parametrize(("dpr", "font"), [(1.0, 1.0), (1.25, 1.0), (1.5, 1.4), (2.0, 0.8)])
def test_labels_run_along_the_ring_or_along_the_radius(dpr: float, font: float) -> None:
    store = tree(3000)  # a disk-like tree: wide top levels, deep thin branches
    dl = run(store, width=1280, height=900, dpr=dpr, font_scale=font, detail=6, min_element=4)
    g = dl.geometry
    got = dl.label.tolist()
    rows = zip(g["a0"].tolist(), g["a1"].tolist(), g["r0"].tolist(), g["r1"].tolist(), strict=True)
    assert got == [want_label(*cell, dpr, font) for cell in rows]
    assert any(b & 16 for b in got) and any(b & 1 and not b & 16 for b in got)  # both directions occur


def test_a_thin_spike_names_itself_along_the_radius() -> None:
    proj = FlamePolarProjection(LayoutParams(width=1920, height=1080, detail=5))
    cell = (0, U // 60, proj.r0[3], proj.r1[3])  # 6°: 34 px long at its middle radius, 78 px thick
    assert proj.label(cell, int(RectKind.FILE)) == 1 | 8 | 16
    wide = (0, U // 4, proj.r0[1], proj.r1[1])
    assert proj.label(wide, int(RectKind.FILE)) == 1 | 2  # along the ring, with the size line
    assert proj.label((0, U // 2000, proj.r0[1], proj.r1[1]), int(RectKind.FILE)) == 0  # too thin


# --------------------------------------------------------------------------- registration


def test_the_polar_flame_graph_is_registered() -> None:
    from spaceknife.layout import flame_polar

    v = registry.get("flame_polar")
    assert v is flame_polar.FLAME_POLAR
    assert (v.layout, v.capability, v.icon, v.order, v.default_color) == (
        pv.POLAR_LAYOUT,
        CAP_LAYOUT_POLAR,
        "vizFlamePolar",
        2,
        "branch",
    )
    assert v.title == "火焰-极坐标图"
    assert registry.is_available(v, [CAP_LAYOUT_POLAR]) and not registry.is_available(v, [])
    basis = Basis(build({"a.bin": MB}))
    dl = v.engine(basis).run(LayoutParams(width=300, height=200), RootSpec(node=1), StoreSizes(basis))
    assert (dl.layout, dl.visual) == (pv.POLAR_LAYOUT, "flame_polar")

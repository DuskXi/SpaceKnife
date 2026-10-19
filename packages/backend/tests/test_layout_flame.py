"""Flame graph projection (flame.v1): bands, bar widths, snapping, direction, labels, registration."""

from __future__ import annotations

import random
from typing import Any

import numpy as np
import pytest

from spaceknife.layout import (
    ArraySizes,
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
from spaceknife.layout.engine import KEY_FREE, KEY_SMALL, KEY_UNKNOWN
from spaceknife.layout.flame import GAP, ROW_MAX, ROW_MIN, FlameProjection, flame_engine
from spaceknife.store.nodes import NodeStore
from spaceknife_protocol import CAP_LAYOUT_FLAME
from spaceknife_protocol import view as pv

from .store_builder import build, find
from .test_layout import random_spec

MB = 1 << 20


def run(
    store: NodeStore, root: RootSpec | None = None, sizes: np.ndarray | None = None, **params: Any
) -> DrawList:
    """Lay out ``root`` (default: the scan root) on 1200 × 800 px; the draw list keeps the contract."""
    root = root or RootSpec(node=1)
    e = flame_engine(Basis(store))
    source = StoreSizes(e.basis) if sizes is None else ArraySizes(sizes)
    dl = e.run(LayoutParams(**({"width": 1200, "height": 800} | params)), root, source)
    assert check_draw(dl, store, root, sizes=None if sizes is None else source) == []
    assert isinstance(dl.meta, pv.FlameGeometry)
    return dl


def row_of(dl: DrawList, store: NodeStore, path: str) -> int:
    return int(np.flatnonzero(dl.node == find(store, path))[0])


def meta(dl: DrawList) -> pv.FlameGeometry:
    assert isinstance(dl.meta, pv.FlameGeometry)
    return dl.meta


def band_y(dl: DrawList, k: int) -> int:
    """docs/protocol.md §4.3: the y of band k (0: the root bar)."""
    m = meta(dl)
    return k * m.row_h if m.origin == "top" else dl.height - k * m.row_h - (m.row_h - m.gap)


# --------------------------------------------------------------------------- bands


@pytest.mark.parametrize(
    ("height", "dpr", "font", "detail"),
    [
        (800, 1.0, 1.0, 5),
        (800, 1.0, 1.0, 15),
        (2160, 1.0, 1.0, 15),
        (300, 1.0, 1.0, 15),
        (500, 1.5, 1.4, 15),
        (720, 1.25, 0.8, 8),
        (60, 1.0, 1.0, 5),
        (39, 1.0, 1.0, 5),
        (10, 2.0, 1.0, 5),
    ],
)
def test_levels_and_band_height_stay_within_their_bounds(
    height: int, dpr: float, font: float, detail: int
) -> None:
    p = LayoutParams(width=1000, height=height, dpr=dpr, font_scale=font, detail=detail)
    proj = FlameProjection(p)
    m = proj.meta()
    lu = dpr * font
    H = round(height * dpr)
    row_min, row_max = round(ROW_MIN * lu), round(ROW_MAX * lu)
    assert proj.levels == m.rows == max(0, min(detail, H // row_min - 1))
    assert m.gap == max(1, round(GAP * dpr))
    if m.rows:
        assert row_min <= m.row_h <= row_max
        assert (m.rows + 1) * m.row_h <= H  # every band on the canvas
        assert m.row_h == min(H // (m.rows + 1), row_max)
    assert flame_engine(Basis(build({"a.bin": 1}))).levels(p) == m.rows


def test_tall_canvases_keep_bars_at_their_largest_height() -> None:
    m = FlameProjection(LayoutParams(width=1920, height=1080, detail=5)).meta()
    assert m.row_h == ROW_MAX and m.rows == 5
    assert FlameProjection(LayoutParams(width=1920, height=1080, detail=15)).meta().row_h == ROW_MAX


def test_no_band_fits_no_rows() -> None:
    basis = Basis(build({"a.bin": MB}))
    dl = flame_engine(basis).run(LayoutParams(width=1200, height=30), RootSpec(node=1), StoreSizes(basis))
    assert len(dl) == 0 and dl.levels == meta(dl).rows == 0
    assert meta(dl).root.h == 30 - meta(dl).gap  # the root bar still fills its band


def test_the_root_bar_spans_the_canvas_in_band_zero() -> None:
    for origin in ("top", "bottom"):
        dl = run(build({"a.bin": MB}), options=(("origin", origin),))
        m = meta(dl)
        assert (m.root.x, m.root.y, m.root.w, m.root.h) == (0, band_y(dl, 0), 1200, m.row_h - m.gap)
    assert meta(run(build({"a.bin": MB}))).origin == "bottom"  # the classic flame graph by default


# --------------------------------------------------------------------------- widths


def test_bar_widths_follow_sizes() -> None:
    sizes = [400, 300, 120, 90, 50, 25, 15]
    store = build({f"f{i}.bin": s * MB for i, s in enumerate(sizes)})
    dl = run(store, min_element=1)
    g = meta(dl).gap
    want = dl.size / dl.size.sum() * (dl.width + g)  # each bar's cell includes the gap on its right
    assert np.max(np.abs(dl.w + g - want)) <= 1
    assert dl.x[0] == 0 and dl.x[-1] + dl.w[-1] == dl.width  # the rightmost bar ends at the canvas edge


@pytest.mark.parametrize("seed", range(6))
@pytest.mark.parametrize("dpr", [1.0, 1.25, 1.5, 2.0])
def test_children_sit_in_the_next_band_within_their_parent(seed: int, dpr: float) -> None:
    store = build(random_spec(random.Random(seed)), cluster=4096)
    dl = run(store, width=900, height=640, dpr=dpr, detail=8, min_element=4)
    assert len(dl) > 0
    g = meta(dl).gap
    for i in range(len(dl)):
        p = int(dl.parent[i])
        assert dl.y[i] == band_y(dl, int(dl.depth[i])) and dl.h[i] == meta(dl).row_h - g
        if p >= 0:
            assert dl.depth[i] == dl.depth[p] + 1
            assert dl.x[p] <= dl.x[i] < dl.x[p] + dl.w[p] + g
            assert dl.x[i] + dl.w[i] <= dl.x[p] + dl.w[p]
    assert (dl.w >= round(2 * dpr)).all()


@pytest.mark.parametrize("seed", range(4))
def test_bars_of_a_band_run_left_to_right_without_overlap(seed: int) -> None:
    store = build(random_spec(random.Random(seed)), cluster=4096)
    dl = run(store, detail=8, min_element=4)
    g = meta(dl).gap
    for d in np.unique(dl.depth).tolist():
        rows = np.flatnonzero(dl.depth == d)
        assert (dl.x[rows[1:]] >= dl.x[rows[:-1]] + dl.w[rows[:-1]] + g).all()
        assert (dl.x[rows] + dl.w[rows] <= dl.width).all()


def test_siblings_meet_at_one_gap_and_the_last_child_meets_its_parent() -> None:
    spec = {"A": {"a1.bin": 300 * MB, "a2.bin": 100 * MB, "S": {"s.bin": 50 * MB, "t.bin": 50 * MB}}}
    store = build(spec | {"B": {"b1.bin": 200 * MB, "b2.bin": 200 * MB}, "c.bin": 100 * MB})
    dl = run(store, min_element=1, dpr=1.5)
    g = meta(dl).gap
    for row in [-1, *np.flatnonzero(dl.kind == RectKind.FOLDER).tolist()]:
        kids = np.flatnonzero(dl.parent == row)
        x0, x1 = (0, dl.width) if row < 0 else (int(dl.x[row]), int(dl.x[row] + dl.w[row]))
        assert dl.x[kids[0]] == x0 and dl.x[kids[-1]] + dl.w[kids[-1]] == x1
        assert np.array_equal(dl.x[kids[1:]], dl.x[kids[:-1]] + dl.w[kids[:-1]] + g)


# --------------------------------------------------------------------------- merging and expanding


def test_the_merge_threshold_is_the_minimum_bar_width() -> None:
    # 1202 px share out per byte: 100 files span 12 px each, 300 files 4 px
    for n, merged in ((100, False), (300, True)):
        store = build({f"f{i}.bin": MB for i in range(n)})
        dl = run(store, min_element=10)
        assert (dl.row(KEY_SMALL | 1) is not None) == merged
        files = dl.kind == RectKind.FILE
        assert (dl.w[files] + meta(dl).gap >= 10 - 1).all()
    # the same length at every level: a folder's children merge below 10 px of their own bar
    store = build({"A": {f"f{i}.bin": MB for i in range(100)}, "B": {f"f{i}.bin": MB for i in range(100)}})
    dl = run(store, min_element=10)
    for name in ("A", "B"):
        row = row_of(dl, store, name)
        small = dl.row(KEY_SMALL | find(store, name))
        assert dl.w[row] == 599 and small is not None and dl.parent[small] == row  # 6 px per file


def test_detail_level_controls_nesting() -> None:
    spec = {"a": {"b": {"c": {"d": {"x.bin": 50 * MB, "y.bin": 40 * MB}}}}, "z.bin": 10 * MB}
    store = build(spec)
    for detail in range(1, 6):
        dl = run(store, detail=detail)
        folders = [n for n, k in zip(dl.names, dl.kind, strict=True) if k == RectKind.FOLDER]
        collapsed = [n for n, k in zip(dl.names, dl.kind, strict=True) if k == RectKind.COLLAPSED]
        assert folders == ["a", "b", "c", "d"][: detail - 1]
        assert collapsed == (["a", "b", "c", "d"][detail - 1 : detail] if detail <= 4 else [])
        assert int(dl.depth.max()) == detail


def test_folders_narrower_than_the_minimum_element_collapse() -> None:
    store = build({"big.bin": 990 * MB, "tiny": {"a.bin": 5 * MB, "b.bin": 5 * MB}})
    dl = run(store, min_element=14)
    row = row_of(dl, store, "tiny")
    assert dl.kind[row] == RectKind.COLLAPSED and dl.w[row] < 14
    wide = run(store, min_element=8)
    assert wide.kind[row_of(wide, store, "tiny")] == RectKind.FOLDER


def test_free_and_unknown_space_lie_in_the_first_band() -> None:
    store = build({"Users": {"a.bin": 400 * MB}, "Windows": {"b.bin": 100 * MB}})
    dl = run(store, RootSpec(node=1, free=200 * MB, unknown=50 * MB))
    for key in (KEY_FREE, KEY_UNKNOWN):
        row = dl.row(key)
        assert row is not None and dl.depth[row] == 1 and dl.y[row] == band_y(dl, 1)
    top = np.flatnonzero(dl.parent == -1)
    assert dl.size[top].tolist() == sorted(dl.size[top].tolist(), reverse=True)  # largest on the left


# --------------------------------------------------------------------------- direction and labels


@pytest.mark.parametrize("dpr", [1.0, 1.25, 1.5])
def test_root_on_top_and_root_at_the_bottom_mirror_each_other(dpr: float) -> None:
    store = build(random_spec(random.Random(5)), cluster=4096)
    top = run(store, dpr=dpr, detail=8, min_element=4, options=(("origin", "top"),))
    bottom = run(store, dpr=dpr, detail=8, min_element=4, options=(("origin", "bottom"),))
    for col in ("key", "kind", "depth", "label", "size", "x", "w", "h"):
        assert np.array_equal(getattr(top, col), getattr(bottom, col))
    assert np.array_equal(bottom.y, top.height - top.y - top.h)
    mt, mb = meta(top), meta(bottom)
    assert (mt.origin, mb.origin) == ("top", "bottom")
    assert mt.root.y == 0 and mb.root.y == top.height - mt.root.h
    assert np.all(np.diff(bottom.y[np.argsort(bottom.depth)]) <= 0)  # deeper levels higher up


@pytest.mark.parametrize(("dpr", "font"), [(1.0, 1.0), (1.25, 1.0), (2.0, 1.4), (1.0, 0.8)])
def test_labels_follow_label_box_without_the_large_type(dpr: float, font: float) -> None:
    store = build(random_spec(random.Random(7)), cluster=4096)
    dl = run(store, dpr=dpr, font_scale=font, detail=8, min_element=4)
    lu = dpr * font
    want = [label_box(w / lu, h / lu, big=False) for w, h in zip(dl.w.tolist(), dl.h.tolist(), strict=True)]
    assert dl.label.tolist() == want
    assert any(want) and not any(b & 4 for b in want)


def test_an_unexpected_origin_reads_as_the_default() -> None:
    p = LayoutParams(width=500, height=400, options=(("origin", 3),))
    assert FlameProjection(p).meta().origin == "bottom"


# --------------------------------------------------------------------------- registration


def test_the_flame_graph_is_registered() -> None:
    from spaceknife.layout import flame

    v = registry.get("flame")
    assert v is flame.FLAME
    assert (v.layout, v.capability, v.icon, v.order, v.default_color) == (
        pv.FLAME_LAYOUT,
        CAP_LAYOUT_FLAME,
        "vizFlame",
        1,
        "branch",
    )
    assert v.title == "火焰图"
    assert registry.is_available(v, [CAP_LAYOUT_FLAME]) and not registry.is_available(v, [])
    basis = Basis(build({"a.bin": MB}))
    dl = v.engine(basis).run(LayoutParams(width=300, height=200), RootSpec(node=1), StoreSizes(basis))
    assert (dl.layout, dl.visual) == (pv.FLAME_LAYOUT, "flame")

"""Treemap layout: tiling, proportions, pixel snapping, expansion rules, G5 merging, special blocks."""

from __future__ import annotations

import random

import numpy as np
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from spaceknife.layout import NO_CLASS, DrawList, Label, Layout, LayoutParams, RectKind, RootSpec
from spaceknife.layout.engine import KEY_FREE, KEY_MORE, KEY_SMALL, KEY_UNKNOWN, PAGE_REST
from spaceknife.layout.squarify import squarify

from .store_builder import build, find

MB = 1 << 20
GB = 1 << 30


# --------------------------------------------------------------------------- squarify


@settings(max_examples=300, deadline=None)
@given(
    st.lists(st.floats(min_value=0.01, max_value=1e6), min_size=1, max_size=60),
    st.floats(min_value=1, max_value=4000),
    st.floats(min_value=1, max_value=4000),
)
def test_squarify_tiles_the_rectangle_exactly(sizes: list[float], w: float, h: float) -> None:
    sizes = sorted(sizes, reverse=True)
    cells = squarify(sizes, 0.0, 0.0, w, h)
    areas = [(x1 - x0) * (y1 - y0) for x0, y0, x1, y1 in cells]
    assert sum(areas) == pytest.approx(w * h, rel=1e-9)
    total = sum(sizes)
    for a, s in zip(areas, sizes, strict=True):
        assert a == pytest.approx(s / total * w * h, rel=1e-6, abs=1e-6)
    for x0, y0, x1, y1 in cells:
        assert -1e-9 <= x0 <= x1 <= w + 1e-9 and -1e-9 <= y0 <= y1 <= h + 1e-9
    # cells never overlap
    for i, a in enumerate(cells):
        for b in cells[i + 1 :]:
            ox = min(a[2], b[2]) - max(a[0], b[0])
            oy = min(a[3], b[3]) - max(a[1], b[1])
            assert ox <= 1e-6 or oy <= 1e-6


def test_squarify_prefers_square_cells() -> None:
    cells = squarify([6, 6, 4, 3, 2, 2, 1], 0, 0, 6, 4)  # the example of Bruls et al.
    ratios = [max((x1 - x0) / (y1 - y0), (y1 - y0) / (x1 - x0)) for x0, y0, x1, y1 in cells]
    assert max(ratios) < 3


# --------------------------------------------------------------------------- helpers


def random_spec(rng: random.Random, depth: int = 0) -> dict[str, object]:
    spec: dict[str, object] = {}
    for i in range(rng.randint(2, 9 if depth < 3 else 4)):
        if depth < 4 and rng.random() < 0.3:
            spec[f"d{depth}_{i}"] = random_spec(rng, depth + 1)
        else:
            ext = rng.choice([".mp4", ".jpg", ".txt", ".zip", ".exe", ".bin", ""])
            spec[f"f{depth}_{i}{ext}"] = rng.choice([1, 100, 4096, 50_000, MB, 20 * MB, 700 * MB])
    return spec


def rects(dl: DrawList) -> list[tuple[int, int, int, int]]:
    return list(zip(dl.x.tolist(), dl.y.tolist(), dl.w.tolist(), dl.h.tolist(), strict=True))


def content_box(dl: DrawList, row: int, dpr: float) -> tuple[int, int, int, int]:
    b, pad, header = max(1, round(dpr)), round(2 * dpr), round(18 * dpr)
    x, y, w, h = rects(dl)[row]
    return x + b + pad, y + b + header, x + w - b - pad, y + h - b - pad


def check_geometry(dl: DrawList, dpr: float) -> None:
    W, H = dl.width, dl.height
    gap = max(1, round(2 * dpr))
    rs = rects(dl)
    by_parent: dict[int, list[int]] = {}
    for i, p in enumerate(dl.parent.tolist()):
        by_parent.setdefault(p, []).append(i)
        assert p < i, "parents come first"
        x, y, w, h = rs[i]
        assert w >= 2 * dpr and h >= 2 * dpr
        if p < 0:
            bx0, by0, bx1, by1 = 0, 0, W, H
        else:
            assert dl.kind[p] == RectKind.FOLDER
            bx0, by0, bx1, by1 = content_box(dl, p, dpr)
        assert bx0 <= x and x + w <= bx1 and by0 <= y and y + h <= by1, (i, rs[i], (bx0, by0, bx1, by1))
    for rows in by_parent.values():
        for i, a in enumerate(rows):
            ax, ay, aw, ah = rs[a]
            for b in rows[i + 1 :]:
                bx, by, bw, bh = rs[b]
                # siblings are separated by at least one gap
                sep_x = max(bx - (ax + aw), ax - (bx + bw))
                sep_y = max(by - (ay + ah), ay - (by + bh))
                assert max(sep_x, sep_y) >= gap, (rs[a], rs[b])


# --------------------------------------------------------------------------- layout


@pytest.mark.parametrize("seed", range(6))
@pytest.mark.parametrize("dpr", [1.0, 1.25, 1.5, 2.0])
def test_random_trees_tile_without_overlap(seed: int, dpr: float) -> None:
    store = build(random_spec(random.Random(seed)), cluster=4096)
    dl = Layout(store).run(LayoutParams(width=900, height=640, dpr=dpr, detail=6), RootSpec(node=1))
    assert len(dl) > 0
    check_geometry(dl, dpr)
    for col in (dl.x, dl.y, dl.w, dl.h):
        assert col.dtype == np.int32  # whole physical pixels


def test_areas_follow_sizes() -> None:
    sizes = [400, 300, 120, 90, 50, 25, 15]
    store = build({f"f{i}.bin": s * MB for i, s in enumerate(sizes)})
    dl = Layout(store).run(LayoutParams(width=1200, height=800, min_element=1), RootSpec(node=1))
    gap = 2
    cells = (dl.w + gap).astype(float) * (dl.h + gap)  # each block's cell includes its gap
    share = cells / ((1200 + gap) * (800 + gap))
    want = dl.size / dl.size.sum()
    assert np.max(np.abs(share - want)) < 0.01


def test_largest_first_in_the_top_left() -> None:
    store = build({"small.bin": MB, "big.bin": 9 * MB, "mid.bin": 3 * MB})
    dl = Layout(store).run(LayoutParams(width=600, height=400), RootSpec(node=1))
    assert dl.names[0] == "big.bin" and (dl.x[0], dl.y[0]) == (0, 0)
    by_name = Layout(store).run(LayoutParams(width=600, height=400, sort_by_size=False), RootSpec(node=1))
    assert by_name.names == ["big.bin", "mid.bin", "small.bin"]


def test_detail_level_controls_nesting() -> None:
    spec = {"a": {"b": {"c": {"d": {"x.bin": 50 * MB, "y.bin": 40 * MB}}}}, "z.bin": 10 * MB}
    store = build(spec)
    lay = Layout(store)
    for detail in range(1, 6):
        dl = lay.run(LayoutParams(width=1000, height=800, detail=detail), RootSpec(node=1))
        folders = [n for n, k in zip(dl.names, dl.kind, strict=True) if k == RectKind.FOLDER]
        collapsed = [n for n, k in zip(dl.names, dl.kind, strict=True) if k == RectKind.COLLAPSED]
        assert folders == ["a", "b", "c", "d"][: detail - 1]
        assert collapsed == (["a", "b", "c", "d"][detail - 1 : detail] if detail <= 4 else [])
        assert int(dl.depth.max()) == detail


def test_folders_too_small_to_hold_content_collapse() -> None:
    store = build({"big.bin": 100 * MB, "tiny": {"a.bin": 3 * MB, "b.bin": 3 * MB}})
    dl = Layout(store).run(LayoutParams(width=400, height=300, min_element=1), RootSpec(node=1))
    row = int(np.nonzero(dl.node == find(store, "tiny"))[0][0])
    assert dl.kind[row] == RectKind.COLLAPSED
    assert dl.w[row] < 48 or dl.h[row] < 40


def test_small_files_merge_into_one_block_with_the_dominant_class() -> None:
    spec: dict[str, object] = {"movie.mp4": 900 * MB}
    spec.update({f"p{i}.jpg": 60_000 for i in range(300)})
    spec.update({f"t{i}.txt": 10_000 for i in range(100)})
    store = build(spec)
    lay = Layout(store)
    dl = lay.run(LayoutParams(width=800, height=600), RootSpec(node=1))
    row = dl.row(KEY_SMALL | 1)
    assert row is not None
    assert dl.kind[row] == RectKind.SMALL
    assert dl.count[row] == 400
    assert dl.size[row] == 300 * 60_000 + 100 * 10_000
    assert lay.class_ids[dl.cls[row]] == "image"  # 95 % of its bytes are pictures
    members = dl.members[KEY_SMALL | 1]
    assert len(members) == 400 and dl.size[row] == Layout.effective_sizes(store)[members].sum()
    # mixed bytes without a majority: in no one class (painted like "other", never counted as it)
    mixed: dict[str, object] = {"movie.mp4": 900 * MB}
    mixed.update({f"p{i}.jpg": 30_000 for i in range(100)})
    mixed.update({f"z{i}.zip": 30_000 for i in range(100)})
    mixed.update({f"t{i}.txt": 30_000 for i in range(100)})
    m = Layout(build(mixed))
    dm = m.run(LayoutParams(width=800, height=600), RootSpec(node=1))
    r = dm.row(KEY_SMALL | 1)
    assert r is not None and dm.cls[r] == NO_CLASS


def test_tiny_siblings_merge_into_plus_n() -> None:
    spec: dict[str, object] = {"big.bin": 100 * MB}
    spec.update({f"dir{i}": {"x.bin": 10_000} for i in range(50)})
    spec.update({f"f{i}.bin": 10_000 for i in range(30)})
    store = build(spec)
    dl = Layout(store).run(LayoutParams(width=800, height=600), RootSpec(node=1))
    more = dl.row(KEY_MORE | 1)
    assert more is not None and dl.kind[more] == RectKind.MORE
    assert more == len(dl) - 1  # placed last
    assert dl.count[more] == 50 and dl.size[more] == 50 * 10_000
    small = dl.row(KEY_SMALL | 1)  # the 30 small files together are big enough to be drawn
    assert small is not None and dl.count[small] == 30


def test_a_too_small_small_items_block_joins_plus_n() -> None:
    spec: dict[str, object] = {"big.bin": 100 * MB}
    spec.update({f"dir{i}": {"x.bin": 10_000} for i in range(50)})
    spec.update({f"f{i}.bin": 500 for i in range(30)})
    store = build(spec)
    dl = Layout(store).run(LayoutParams(width=800, height=600), RootSpec(node=1))
    more = dl.row(KEY_MORE | 1)
    assert more is not None
    assert dl.row(KEY_SMALL | 1) is None
    assert dl.count[more] == 80  # 50 folders + 30 files of the merged small-items block
    assert len(dl.members[KEY_MORE | 1]) == 80
    assert dl.size[more] == 50 * 10_000 + 30 * 500


def test_aggregate_block_can_be_the_zoom_root() -> None:
    spec: dict[str, object] = {"big.bin": 10_000 * MB}
    spec.update({f"f{i}.bin": (i + 1) * 1000 for i in range(200)})
    store = build(spec)
    lay = Layout(store)
    dl = lay.run(LayoutParams(width=800, height=600), RootSpec(node=1))
    members = dl.members[KEY_SMALL | 1]
    zoomed = lay.run(LayoutParams(width=800, height=600, min_element=1), RootSpec(node=1, members=members))
    assert set(zoomed.node.tolist()) <= set(members.tolist())
    assert len(zoomed) > 150  # the members are spread out individually now


@pytest.mark.parametrize("dpr", [1.0, 1.25, 2.0])
def test_zooming_into_a_huge_small_items_block_pages_through_its_members(dpr: float) -> None:
    # 20,000 files of 4 KB: alone each would still get less than the minimum element size
    store = build({"Big": {"big.iso": 4 * GB} | {f"f{i}.bin": 4096 for i in range(20_000)}})
    big = find(store, "Big")
    lay = Layout(store)
    p = LayoutParams(width=1200, height=800, dpr=dpr)
    key = KEY_SMALL | big
    everything = members = lay.run(p, RootSpec(node=1)).members[key]
    assert len(members) == 20_000
    thr, gap = (14 * dpr) ** 2, max(1, round(2 * dpr))
    seen: list[int] = []
    keys = [key]
    while True:
        dl = lay.run(p, RootSpec(node=big, members=members, key=key))
        files = dl.kind == RectKind.FILE
        assert files.sum() > 1000  # every zoom shows members of its own, at the minimum element size
        assert ((dl.w[files] + gap) * (dl.h[files] + gap) >= 0.85 * thr).all()  # up to pixel snapping
        seen += dl.node[files].tolist()
        rest = np.flatnonzero(dl.kind != RectKind.FILE)
        if not rest.size:
            break
        (row,) = rest
        key = int(dl.key[row])
        assert dl.kind[row] == RectKind.MORE and row == len(dl) - 1
        assert key >> 30 == KEY_MORE >> 30 and key not in keys  # never reused: zooming in again is progress
        share = (dl.w[row] + gap) * (dl.h[row] + gap) / ((dl.width + gap) * (dl.height + gap))
        assert share == pytest.approx(PAGE_REST, abs=0.01)  # shrunk to leave room for the members
        rx, ry, rw, rh = (int(c[row]) for c in (dl.x, dl.y, dl.w, dl.h))
        fx, fy, fw, fh = (c[files] for c in (dl.x, dl.y, dl.w, dl.h))
        apart = np.maximum(
            np.maximum(rx - (fx + fw), fx - (rx + rw)), np.maximum(ry - (fy + fh), fy - (ry + rh))
        )
        assert (apart >= gap).all()  # its own strip, clear of every member
        inner = dl.members[key]
        assert dl.count[row] == len(inner) == len(members) - files.sum()
        assert dl.size[row] == 4096 * len(inner)  # the real bytes, not the shrunk area
        again = lay.run(p, RootSpec(node=big, members=members[::-1].copy(), key=keys[-1]))
        assert int(again.key[-1]) == key  # keyed by its largest member: stable, whatever the order
        keys.append(key)
        members = inner
    assert len(keys) == 5
    assert sorted(seen) == sorted(everything.tolist())  # each member on exactly one page


def test_the_leftover_never_takes_the_key_of_the_block_zoomed_into() -> None:
    store = build({"Big": {f"f{i}.bin": 4096 for i in range(20_000)}})
    big = find(store, "Big")
    lay = Layout(store)
    p = LayoutParams(width=1200, height=800)
    members = np.asarray(store.children(big), dtype=np.int64)
    first = lay.run(p, RootSpec(node=big, members=members))
    taken = int(first.key[-1])
    clash = lay.run(p, RootSpec(node=big, members=members, key=taken))  # e.g. sizes changed meanwhile
    other = int(clash.key[-1])
    assert other != taken and other >> 30 == KEY_MORE >> 30
    assert np.array_equal(clash.members[other], first.members[taken])


def test_the_leftover_of_an_aggregate_zoom_stays_proportional_when_shrinking_shows_nothing_more() -> None:
    store = build({"M": {f"d{i}.bin": MB for i in range(20)} | {f"s{i}.txt": 1024 for i in range(10_000)}})
    m = find(store, "M")
    members = np.asarray(store.children(m), dtype=np.int64)
    dl = Layout(store).run(LayoutParams(width=1200, height=800), RootSpec(node=m, members=members))
    assert (dl.kind == RectKind.FILE).sum() == 20
    row = len(dl) - 1
    assert dl.kind[row] == RectKind.MORE and dl.count[row] == 10_000
    share = (dl.w[row] + 2) * (dl.h[row] + 2) / (1202 * 802)
    assert share == pytest.approx(dl.size[row] / dl.size.sum(), abs=0.01)


def test_free_and_unknown_space_sort_with_the_top_level() -> None:
    store = build({"Users": {"a.bin": 400 * MB}, "Windows": {"b.bin": 100 * MB}})
    dl = Layout(store).run(
        LayoutParams(width=900, height=600), RootSpec(node=1, free=200 * MB, unknown=50 * MB)
    )
    top = [(int(k), int(s)) for k, s, p in zip(dl.key, dl.size, dl.parent, strict=True) if p == -1]
    assert [s for _, s in top] == sorted((s for _, s in top), reverse=True)
    kinds = {int(k): RectKind(int(dl.kind[i])) for i, k in enumerate(dl.key) if dl.parent[i] == -1}
    assert kinds[KEY_FREE] == RectKind.FREE and kinds[KEY_UNKNOWN] == RectKind.UNKNOWN
    none = Layout(store).run(LayoutParams(width=900, height=600), RootSpec(node=1, free=200 * MB, unknown=-5))
    assert none.row(KEY_UNKNOWN) is None  # nothing unaccounted: no block


def test_pending_folders_are_not_expanded() -> None:
    store = build({"done": {"a.bin": 300 * MB}, "todo": {"b.bin": 1}}, pending=("todo",))
    node = find(store, "todo")
    sizes = Layout.effective_sizes(store)
    sizes[node] = 200 * MB  # e.g. the size remembered from the last snapshot
    dl = Layout(store).run(LayoutParams(width=800, height=600), RootSpec(node=1), sizes=sizes)
    row = dl.names.index("todo")
    assert dl.kind[row] == RectKind.PENDING


def test_a_file_can_be_the_zoom_root() -> None:
    store = build({"one.iso": 5 * MB})
    dl = Layout(store).run(LayoutParams(width=500, height=300), RootSpec(node=2))
    # top-level blocks touch the view's edges: gaps only separate neighbours
    assert len(dl) == 1 and (dl.x[0], dl.y[0], dl.w[0], dl.h[0]) == (0, 0, 500, 300)


def test_empty_folder_draws_nothing() -> None:
    store = build({"empty": {}, "zero.bin": 0})
    assert len(Layout(store).run(LayoutParams(width=500, height=300), RootSpec(node=1))) == 0


def test_labels_follow_block_size() -> None:
    g = LayoutParams(width=10, height=10)
    from spaceknife.layout.engine import _Geom

    geom = _Geom(g)
    assert geom.label(24, 22) == Label.NONE
    assert geom.label(48, 22) == Label.NAME
    assert geom.label(66, 17) == Label.NAME | Label.SHORT
    assert geom.label(72, 46) == Label.NAME | Label.SIZE
    assert geom.label(303, 548) == Label.NAME | Label.SIZE | Label.BIG
    hi = _Geom(LayoutParams(width=10, height=10, dpr=2.0))
    assert hi.label(48, 44) == Label.NONE  # 24×22 CSS px


def test_gaps_scale_with_the_device_pixel_ratio() -> None:
    store = build({"a.bin": 3 * MB, "b.bin": 2 * MB})
    dl = Layout(store).run(LayoutParams(width=400, height=300, dpr=1.5), RootSpec(node=1))
    (ax, ay, aw, ah), (bx, by, _, _) = rects(dl)
    assert (dl.width, dl.height) == (600, 450)
    assert max(bx - (ax + aw), by - (ay + ah)) == 3  # 2 CSS px at 150 %


def test_layout_is_deterministic() -> None:
    store = build(random_spec(random.Random(42)), cluster=4096)
    lay = Layout(store)
    p = LayoutParams(width=1280, height=720, dpr=1.25)
    a, b = lay.run(p, RootSpec(node=1)), Layout(store).run(p, RootSpec(node=1))
    for col in ("key", "x", "y", "w", "h", "kind", "label", "size", "count"):
        assert np.array_equal(getattr(a, col), getattr(b, col))
    assert a.names == b.names

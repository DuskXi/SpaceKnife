"""Partition engine (flame graph, polar flame graph): merges, aggregate zoom, scans, row budget.

Uses a bare test projection — one band of 24 px per level, no gaps — so the expected geometry is
plain arithmetic; flame.py and flame_polar.py have their own tests.
"""

from __future__ import annotations

import random
from collections.abc import Callable
from functools import cache
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
)
from spaceknife.layout.contract import check_draw
from spaceknife.layout.engine import KEY_FREE, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, KEY_UNKNOWN
from spaceknife.layout.flame import flame_engine
from spaceknife.layout.flame_polar import flame_polar_engine
from spaceknife.layout.partition import Cell, PartitionEngine
from spaceknife.store.nodes import NodeStore
from spaceknife_protocol import view as pv

from .store_builder import build, find
from .test_layout import random_spec
from .test_layout_golden import hide_sizes, tree

MB = 1 << 20
GB = 1 << 30
BAND = 24


class Line:
    """Level k is the band [k·24, k·24 + 24); [a, b) is [round(a·W), round(b·W)) along x."""

    columns = ("x", "y", "w", "h")

    def __init__(self, p: LayoutParams) -> None:
        self.width = round(p.width * p.dpr)
        self.levels = max(0, min(p.detail, round(p.height * p.dpr) // BAND - 1))
        self.min_expand = p.min_element * p.dpr

    def unit_px(self, level: int) -> float:
        return float(self.width)

    def cell(self, a: float, b: float, level: int) -> Cell | None:
        x = round(a * self.width)
        w = round(b * self.width) - x
        return (x, level * BAND, w, BAND) if w >= 1 else None

    def expandable(self, cell: Cell, level: int) -> bool:
        return cell[2] >= self.min_expand

    def label(self, cell: Cell, kind: int) -> int:
        return label_box(cell[2], cell[3], big=False)

    def meta(self) -> pv.FlameGeometry:
        root = pv.PixelRect(x=0, y=0, w=self.width, h=BAND)
        return pv.FlameGeometry(origin="top", row_h=BAND, rows=self.levels, gap=0, root=root)


def engine(store: NodeStore) -> PartitionEngine:
    return PartitionEngine(Basis(store), Line, pv.FLAME_LAYOUT, "test.line")


def run(
    store: NodeStore, root: RootSpec | None = None, sizes: np.ndarray | None = None, **params: Any
) -> DrawList:
    """Lay out ``root`` (default: the scan root) on 1000 × 240 px; the draw list keeps the contract."""
    root = root or RootSpec(node=1)
    e = engine(store)
    source = StoreSizes(e.basis) if sizes is None else ArraySizes(sizes)
    dl = e.run(LayoutParams(**({"width": 1000, "height": 240} | params)), root, source)
    assert check_draw(dl, store, root, sizes=None if sizes is None else source) == []
    return dl


def top(dl: DrawList) -> np.ndarray:
    return np.flatnonzero(dl.parent == -1)


def under(dl: DrawList, row: int) -> np.ndarray:
    return np.flatnonzero(dl.parent == row)


# --------------------------------------------------------------------------- merges (G5)


def test_small_files_merge_into_one_block() -> None:
    spec: dict[str, object] = {"movie.mp4": 900 * MB}
    spec.update({f"p{i}.jpg": 60_000 for i in range(300)})
    store = build(spec)
    dl = run(store)
    row = dl.row(KEY_SMALL | 1)
    assert row is not None and dl.kind[row] == RectKind.SMALL
    assert dl.count[row] == 300 and dl.size[row] == 300 * 60_000
    assert len(dl.members[KEY_SMALL | 1]) == 300
    assert dl.names[row] == ""  # the frontend writes "N small items"


def test_a_single_small_file_is_not_a_block_of_its_own_kind() -> None:
    store = build({"movie.mp4": 900 * MB, "note.txt": 10_000, "pic.jpg": 20 * MB})
    dl = run(store)
    assert dl.row(KEY_SMALL | 1) is None and dl.row(KEY_MORE | 1) is None  # merging needs two members
    assert set(dl.kind.tolist()) == {RectKind.FILE}


def test_tiny_siblings_merge_into_plus_n_placed_last() -> None:
    spec: dict[str, object] = {"big.bin": 100 * MB}
    spec.update({f"dir{i}": {"x.bin": 10_000} for i in range(50)})
    spec.update({f"f{i}.bin": 5 * MB for i in range(3)})
    store = build(spec)
    for by_size in (True, False):
        dl = run(store, sort_by_size=by_size)
        more = dl.row(KEY_MORE | 1)
        assert more is not None and dl.kind[more] == RectKind.MORE
        assert more == top(dl)[-1] and dl.x[more] == dl.x[top(dl)].max()  # last, at the right end
        assert dl.count[more] == 50 and dl.size[more] == 50 * 10_000


def test_sorting_by_name_runs_left_to_right() -> None:
    store = build({"b.bin": MB, "C.bin": 3 * MB, "a.bin": 2 * MB})
    by_name = run(store, sort_by_size=False)
    assert by_name.names == ["a.bin", "b.bin", "C.bin"]
    assert np.all(np.diff(by_name.x) > 0)
    by_size = run(store)
    assert by_size.names == ["C.bin", "a.bin", "b.bin"]


def test_free_and_unknown_space_join_the_first_level() -> None:
    store = build({"Users": {"a.bin": 400 * MB}, "Windows": {"b.bin": 100 * MB}})
    root = RootSpec(node=1, free=200 * MB, unknown=50 * MB)
    dl = run(store, root)
    free, unknown = dl.row(KEY_FREE), dl.row(KEY_UNKNOWN)
    assert free is not None and unknown is not None
    assert dl.depth[free] == dl.depth[unknown] == 1 and dl.parent[free] == dl.parent[unknown] == -1
    assert dl.kind[free] == RectKind.FREE and dl.kind[unknown] == RectKind.UNKNOWN
    rows = top(dl)
    assert dl.size[rows].tolist() == sorted(dl.size[rows].tolist(), reverse=True)
    assert run(store, RootSpec(node=1, free=200 * MB, unknown=-5)).row(KEY_UNKNOWN) is None


# --------------------------------------------------------------------------- zoom roots


def test_an_aggregate_zoom_always_shows_members_of_its_own() -> None:
    # 20,000 files of 4 KB: alone each spans 0.05 px of 1000
    store = build({"Big": {"big.iso": 4 * GB} | {f"f{i}.bin": 4096 for i in range(20_000)}})
    big = find(store, "Big")
    key = KEY_SMALL | big
    members = run(store).members[key]
    assert len(members) == 20_000
    seen: list[int] = []
    for _ in range(3):
        root = RootSpec(node=big, members=members, key=key)
        dl = run(store, root)
        files = np.flatnonzero(dl.kind == RectKind.FILE)
        assert len(files) >= 1 and set(dl.node[files].tolist()) <= set(members.tolist())
        assert ((dl.w[files]) >= 14 - 1).all()  # each at the minimum element size, up to snapping
        seen += dl.node[files].tolist()
        more = int(top(dl)[-1])
        assert dl.kind[more] == RectKind.MORE and more == len(dl) - 1
        key = int(dl.key[more])
        members = dl.members[key]
        assert dl.count[more] == len(members) and dl.size[more] == 4096 * len(members)
    assert len(set(seen)) == len(seen)  # every page shows new members


def test_a_file_as_the_zoom_root_has_no_rows() -> None:
    store = build({"one.iso": 5 * MB})
    dl = engine(store).run(LayoutParams(width=500, height=300), RootSpec(node=2), StoreSizes(Basis(store)))
    assert len(dl) == 0 and dl.names == []
    assert isinstance(dl.meta, pv.FlameGeometry) and dl.levels == 5
    for col in (*dl.geometry.values(), dl.key, dl.size):
        assert col.shape == (0,)


def test_no_room_for_a_level_draws_nothing() -> None:
    store = build({"a.bin": MB})
    dl = engine(store).run(LayoutParams(width=500, height=30), RootSpec(node=1), StoreSizes(Basis(store)))
    assert len(dl) == 0 and dl.levels == 0 and isinstance(dl.meta, pv.FlameGeometry)


def test_zero_sized_children_are_left_out() -> None:
    store = build({"empty": {}, "zero.bin": 0, "a.bin": MB})
    dl = run(store)
    assert dl.names == ["a.bin"]
    assert len(run(build({"empty": {}, "zero.bin": 0}))) == 0


# --------------------------------------------------------------------------- scans


def test_pending_folders_are_not_expanded() -> None:
    store = build({"done": {"a.bin": 300 * MB}, "todo": {"b.bin": 1}}, pending=("todo",))
    node = find(store, "todo")
    sizes = Basis.effective_sizes(store)
    sizes[node] = 200 * MB  # e.g. the size remembered from the last snapshot
    dl = run(store, sizes=sizes)
    row = dl.names.index("todo")
    assert dl.kind[row] == RectKind.PENDING and not under(dl, row).size
    assert dl.kind[dl.names.index("done")] == RectKind.FOLDER


def test_a_folder_being_listed_reads_as_empty() -> None:
    # ingest publishes the count after the start: a reader may see the count with the old start 0
    store = build({"A": {"x.bin": 300 * MB, "y.bin": 100 * MB}, "b.bin": 50 * MB})
    a = find(store, "A")
    store.cols["child_start"][a] = 0
    dl = run(store)
    row = dl.names.index("A")
    assert not under(dl, row).size  # not the top of the tree again
    assert sorted(dl.names) == ["A", "b.bin"]


def test_nodes_added_after_the_run_started_are_not_read() -> None:
    store = build({"A": {"x.bin": 300 * MB, "y.bin": 100 * MB}, "b.bin": 50 * MB})
    sizes = Basis.effective_sizes(store)
    n = len(store) - 1  # the last listing ends past the node count this run took
    dl = engine(store).run(LayoutParams(width=1000, height=240), RootSpec(node=1), ArraySizes(sizes[:n]))
    assert (dl.node < n).all()
    assert check_draw(dl, store, RootSpec(node=1), sizes=ArraySizes(sizes[:n])) == []


# --------------------------------------------------------------------------- frame budget


def test_the_row_budget_collapses_folders_instead_of_dropping_rows() -> None:
    files = {f"f{i}.bin": (10 + i) * MB for i in range(6)} | {f"s{i}.txt": 1000 for i in range(10)}
    store = build({f"d{i}": {f"e{j}": dict(files) for j in range(6)} for i in range(6)})
    p = LayoutParams(width=1000, height=240, detail=8, min_element=3)
    full = engine(store).run(p, RootSpec(node=1), StoreSizes(Basis(store)))
    assert len(full) == 6 + 36 + 216  # the small files of every folder merge into a block too small to draw
    assert sum(1 for k in full.members if k & ~KEY_NODE_MASK == KEY_SMALL) == 36
    small = engine(store)
    small.max_rows = 40
    dl = small.run(p, RootSpec(node=1), StoreSizes(small.basis))
    assert len(dl) <= 40
    assert check_draw(dl, store, RootSpec(node=1)) == []
    assert set(dl.key.tolist()) <= set(full.key.tolist())  # the same rows, fewer expanded
    cut = [
        int(dl.node[i])
        for i in np.flatnonzero(dl.kind == RectKind.COLLAPSED).tolist()
        if full.kind[full.node == dl.node[i]][0] == RectKind.FOLDER
    ]
    assert cut
    assert set(dl.members) <= set(full.members)
    for node in cut:  # no member blocks of folders that were not expanded after all
        assert KEY_SMALL | node not in dl.members and KEY_MORE | node not in dl.members


# --------------------------------------------------------------------------- invariants


@pytest.mark.parametrize("seed", range(6))
@pytest.mark.parametrize("dpr", [1.0, 1.25, 2.0])
def test_random_trees_keep_the_contract(seed: int, dpr: float) -> None:
    store = build(random_spec(random.Random(seed)), cluster=4096)
    dl = run(store, width=900, height=240 / dpr, dpr=dpr, detail=8, min_element=4)
    assert len(dl) > 0
    for col in dl.geometry.values():
        assert col.dtype == np.int32
    assert dl.layout == pv.FLAME_LAYOUT and dl.visual == "test.line"


def test_children_share_out_their_parent_exactly() -> None:
    # nothing too small to draw, folders without bytes of their own: every pixel and byte accounted
    spec: dict[str, Any] = {
        "A": {"a1.bin": 300 * MB, "a2.bin": 100 * MB, "S": {"s.bin": 50 * MB, "t.bin": 50 * MB}}
    }
    store = build(spec | {"B": {"b1.bin": 200 * MB, "b2.bin": 200 * MB}, "c.bin": 100 * MB})
    dl = run(store, min_element=1)
    containers = [-1, *np.flatnonzero(dl.kind == RectKind.FOLDER).tolist()]
    for row in containers:
        kids = under(dl, row)
        x0, x1 = (0, dl.width) if row < 0 else (int(dl.x[row]), int(dl.x[row] + dl.w[row]))
        size = int(store.cols["tot_alloc"][1]) if row < 0 else int(dl.size[row])
        assert int(dl.size[kids].sum()) == size  # bytes kept
        assert dl.x[kids[0]] == x0 and dl.x[kids[-1]] + dl.w[kids[-1]] == x1  # pixels kept
        assert np.array_equal(dl.x[kids[1:]], dl.x[kids[:-1]] + dl.w[kids[:-1]])  # siblings meet


def test_a_folder_with_bytes_of_its_own_leaves_them_blank() -> None:
    store = build({"A": {"x.bin": 100 * MB, "y.bin": 100 * MB}, "b.bin": 200 * MB})
    a = find(store, "A")
    store.cols["tot_alloc"][a] += 200 * MB  # e.g. the folder's own allocation
    store.cols["tot_alloc"][1] += 200 * MB
    dl = run(store, min_element=1)
    row = dl.names.index("A")
    kids = under(dl, row)
    assert dl.w[row] == 667  # 400 of 600 MB
    assert dl.x[kids[-1]] + dl.w[kids[-1]] == dl.x[row] + 333  # half of it: its children's 200 of 400 MB


def test_layout_is_deterministic() -> None:
    store = build(random_spec(random.Random(42)), cluster=4096)
    p = LayoutParams(width=1280, height=240, dpr=1.25, detail=8, min_element=4)
    a = engine(store).run(p, RootSpec(node=1), StoreSizes(Basis(store)))
    b = engine(store).run(p, RootSpec(node=1), StoreSizes(Basis(store)))
    for col in ("key", "node", "parent", "kind", "depth", "cls", "label", "size", "count"):
        assert np.array_equal(getattr(a, col), getattr(b, col))
    assert all(np.array_equal(a.geometry[k], b.geometry[k]) for k in a.geometry)
    assert a.names == b.names
    assert a.members.keys() == b.members.keys()
    assert all(np.array_equal(a.members[k], b.members[k]) for k in a.members)


def test_levels_follow_the_projection() -> None:
    e = engine(build({"a.bin": 1}))
    assert e.levels(LayoutParams(width=100, height=240, detail=5)) == 5
    assert e.levels(LayoutParams(width=100, height=80, detail=5)) == 2


@cache
def disk(pending_every: int = 0) -> NodeStore:
    return tree(20_000, pending_every=pending_every)


@pytest.mark.parametrize("make", [flame_engine, flame_polar_engine], ids=["flame", "flame_polar"])
@pytest.mark.parametrize(
    ("width", "height", "dpr", "detail"),
    [(1920, 1080, 1.0, 5), (3840, 2160, 1.0, 15), (1280, 720, 1.25, 8), (1280, 720, 1.5, 8)],
)
def test_both_projections_keep_the_contract_on_a_disk_like_tree(
    make: Callable[[Basis], PartitionEngine], width: int, height: int, dpr: float, detail: int
) -> None:
    store = disk()
    basis = Basis(store)
    e = make(basis)
    p = LayoutParams(width=width, height=height, dpr=dpr, detail=detail)
    used = int(store.cols["tot_alloc"][1])
    roots = [RootSpec(node=1), RootSpec(node=1, free=used // 3, unknown=used // 50)]
    first = e.run(p, roots[0], StoreSizes(basis))
    drawn = [k for k in first.members if first.row(k) is not None]
    assert drawn
    roots += [RootSpec(node=k & KEY_NODE_MASK, members=first.members[k], key=k) for k in drawn[:4]]
    for root in roots:
        for by_size in (True, False):
            q = LayoutParams(width=width, height=height, dpr=dpr, detail=detail, sort_by_size=by_size)
            dl = e.run(q, root, StoreSizes(basis))
            assert len(dl) > 0 and check_draw(dl, store, root) == []
    hide = ArraySizes(hide_sizes(store))
    dl = e.run(p, roots[0], hide)
    assert len(dl) > 0 and check_draw(dl, store, roots[0], sizes=hide) == []
    scanning = disk(pending_every=3)
    dl = make(Basis(scanning)).run(p, roots[0], StoreSizes(Basis(scanning)))
    assert (dl.kind == RectKind.PENDING).any() and check_draw(dl, scanning, roots[0]) == []

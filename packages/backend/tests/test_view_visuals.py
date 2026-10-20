"""Views and visualizations: switching keeps the view's state, each visualization its colour mode;
scanning marks, flashes, the zoom root row, selection fallbacks, reveal depth and branch colours.

The flame graphs register in their own modules; these tests use test-only visualizations built on
the treemap engine, one of them handing its rows out as a partition layout (``flame.v1``).
"""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import replace
from itertools import pairwise
from pathlib import Path

import numpy as np
import pytest

from spaceknife.diff import compare as diff_trees
from spaceknife.layout import registry
from spaceknife.layout.api import ENGINE_API, LayoutParams, RootSpec, SizeSource, Visualization
from spaceknife.layout.basis import Basis
from spaceknife.layout.engine import KEY_FREE, KEY_MORE, KEY_SMALL, KEY_UNKNOWN, TreemapEngine
from spaceknife.layout.rows import ROW_SCANS, DrawList, RectKind
from spaceknife.server.compares import ChangeTree
from spaceknife.store.nodes import Flag
from spaceknife.view import TagStore, View
from spaceknife.view.branch import LIMIT, SLOTS, BranchHues
from spaceknife.view.state import F_DIM, F_MATCH, F_SCANNING, F_TAGGED, Frame, RootInfoLike, ViewSettings
from spaceknife_protocol import view as pv

from .store_builder import build, find
from .test_view import GB, MB, SPEC


class AsFlame:
    """The treemap's rows handed out as ``flame.v1`` (the zoom root is drawn, flashes may land on it)."""

    def __init__(self, basis: Basis) -> None:
        self.tree = TreemapEngine(basis)

    def run(self, params: LayoutParams, root: RootSpec, sizes: SizeSource) -> DrawList:
        draw = self.tree.run(params, root, sizes)
        draw.layout = pv.FLAME_LAYOUT
        draw.meta = pv.FlameGeometry(
            origin="bottom",
            row_h=20,
            rows=params.detail,
            gap=2,
            root=pv.PixelRect(x=0, y=0, w=draw.width, h=18),
        )
        return draw

    def levels(self, params: LayoutParams) -> int:
        return params.detail


class Shallow:
    """A treemap that fits one level whatever the detail level asks for."""

    def __init__(self, basis: Basis) -> None:
        self.tree = TreemapEngine(basis)

    def run(self, params: LayoutParams, root: RootSpec, sizes: SizeSource) -> DrawList:
        return self.tree.run(replace(params, detail=1), root, sizes)

    def levels(self, params: LayoutParams) -> int:
        return 1


def fake(id: str, engine: object, layout: str = pv.DRAWLIST_LAYOUT, **kw: object) -> Visualization:
    return Visualization(
        id=id,
        api=ENGINE_API,
        layout=layout,
        capability="",
        title_id="树图",
        icon="vizTreemap",
        order=90,
        engine=engine,  # type: ignore[arg-type]
        **kw,  # type: ignore[arg-type]
    )


FAKES = (
    fake("test.fake", TreemapEngine, default_color="flat"),
    fake("test.rooted", AsFlame, pv.FLAME_LAYOUT, default_color="branch"),
    fake("test.shallow", Shallow),
)


@pytest.fixture(autouse=True)
def fakes() -> Iterator[None]:
    for v in FAKES:
        registry.register(v)
    try:
        yield
    finally:
        for v in FAKES:
            registry.unregister(v.id)


@pytest.fixture
def view(tmp_path: Path) -> View:
    store = build(SPEC, root="C:\\")
    roots = [RootInfoLike(node=1, whole_volume=True, free=500 * MB, used=2200 * MB)]
    v = View("v1", store, roots, tags=TagStore(tmp_path / "tags.json"))
    v.set_viewport(1200, 800, 1.0)
    return v


def plain(spec: dict[str, object], tmp_path: Path, **kw: object) -> View:
    store = build(spec, **kw)  # type: ignore[arg-type]
    v = View("v", store, [RootInfoLike(node=1, whole_volume=False)], tags=TagStore(tmp_path / "t.json"))
    v.set_viewport(1200, 800, 1.0)
    return v


# ------------------------------------------------------------------ switching


def test_switching_keeps_zoom_history_selection_and_filter(view: View) -> None:
    store = view.store
    users, lenny = find(store, "Users"), find(store, "Users/lenny")
    view.render()
    assert view.zoom_into(users)
    view.render()
    assert view.zoom_into(lenny)
    view.select(find(store, "Users/lenny/Videos"))
    view.set_filter("*.mp4")
    view.set_detail(3)
    before = (view.zoom, list(view.back), list(view.forward), view.selected, view.filter.normal, view.detail)
    first = view.render()
    assert view.set_visual("test.fake")
    assert not view.set_visual("test.fake")
    with pytest.raises(KeyError):
        view.set_visual("pie")
    assert view.visual == "test.fake"
    assert (view.zoom, view.back, view.forward, view.selected, view.filter.normal, view.detail) == before
    frame = view.render()
    assert frame.draw.visual == "test.fake" and frame.draw.layout == pv.DRAWLIST_LAYOUT
    assert np.array_equal(frame.draw.key, first.draw.key)  # the same engine: the same rows
    assert frame.selected_row == first.selected_row and frame.filter_totals == first.filter_totals
    assert view.go_back() and view.zoom.node == users and view.visual == "test.fake"  # no history step
    view.set_visual("treemap")
    assert view.render().draw.visual == "treemap"


def test_each_visualization_remembers_its_colour_mode(view: View) -> None:
    assert view.color_mode == "class"
    view.color_mode = "time"
    view.set_visual("test.fake")
    assert view.color_mode == "flat"  # never set on it: its default
    view.color_mode = "branch"
    view.set_visual("test.rooted")
    assert view.color_mode == "branch"
    view.set_visual("treemap")
    assert view.color_mode == "time"
    view.set_visual("test.fake")
    assert view.color_mode == "branch"
    # the settings a view starts with: one colour mode for its first visualization, others listed
    s = ViewSettings(
        visual="test.fake", color_mode="class", colors=(("treemap", "flat"), ("test.fake", "time"))
    )
    v = View("v2", view.store, view.roots, tags=view.tags, layout=view.layout, settings=s)
    assert v.visual == "test.fake" and v.color_mode == "class"
    v.set_visual("treemap")
    assert v.color_mode == "flat"
    with pytest.raises(KeyError):
        View("v3", view.store, view.roots, settings=ViewSettings(visual="pie"))


def test_free_space_only_where_the_visualization_draws_it(view: View) -> None:
    keys = view.render().draw.key.tolist()
    assert KEY_FREE in keys and KEY_UNKNOWN in keys
    registry.register(fake("test.nofree", TreemapEngine, free_space=False))
    try:
        view.set_visual("test.nofree")
        keys = view.render().draw.key.tolist()
        assert KEY_FREE not in keys and KEY_UNKNOWN not in keys
    finally:
        view.set_visual("treemap")
        registry.unregister("test.nofree")


def test_compare_views_switch_too(tmp_path: Path) -> None:
    before = build({"a": {"one.bin": 1000, "two.bin": 5000}, "keep.bin": 70})
    after = build({"a": {"one.bin": 10_000}, "new": {"big.iso": 300_000}, "keep.bin": 70})
    tree = ChangeTree.of(diff_trees(before, after))
    roots = [RootInfoLike(node=r, whole_volume=False) for r in tree.store.roots()]
    v = View("c", tree.store, roots, tags=TagStore(None))
    v.compare = tree
    v.set_viewport(800, 600, 1.0)
    first = v.render()
    assert first.delta is not None
    for visual in ("test.rooted", "test.fake"):
        v.direction = "all"
        v.set_visual(visual)
        frame = v.render()
        assert frame.delta is not None and frame.dstat is not None
        assert np.array_equal(frame.draw.key, first.draw.key)
        assert frame.root.delta == int(tree.delta[v.zoom.node])
        v.direction = "grow"
        grown = v.render()
        assert grown.root.size == int(tree.grow[v.zoom.node])  # the frame's basis: the growth only


# ------------------------------------------------------------------ the zoom root


def test_the_zoom_root_as_a_row(view: View) -> None:
    store = view.store
    frame = view.render()
    root = frame.root
    assert (root.key, root.node, root.kind) == (1, 1, RectKind.FOLDER)
    assert root.size == int(store.cols["tot_alloc"][1]) and root.flags == 0 and root.branch == 0
    view.set_filter("*.mp4")
    assert view.render().root.flags & F_MATCH
    view.filter_mode = "hide"
    hidden = view.render()
    assert hidden.root.size == 900 * MB  # matched bytes only
    view.set_filter("*.nothing")
    assert view.render().root.flags & F_DIM
    view.set_filter("")
    view.filter_mode = "dim"
    # tagged, a file as the root, an aggregate zoom
    videos = find(store, "Users/lenny/Videos")
    view.select(videos)
    assert view.tag_selected("red")
    view.zoom_to_node(videos)
    root = view.render().root
    assert root.tag == 1 and root.flags & F_TAGGED and root.size == 1500 * MB
    temp = find(store, "Users/lenny/Temp")
    view.zoom_to_node(temp)
    view.render()
    assert view.zoom_into(KEY_SMALL | temp)
    root = view.render().root
    assert (root.key, root.node, root.kind, root.count) == (KEY_SMALL | temp, temp, RectKind.SMALL, 400)
    assert root.size == 400 * 5000


# ------------------------------------------------------------------ scanning and flashes


def test_switching_while_scanning_keeps_the_scanning_marks(tmp_path: Path) -> None:
    spec: dict[str, object] = {
        "a": {"b": {"c": {"d": {"e": {"f.bin": 1}, "h.bin": 20 * MB}, "g.bin": 30 * MB}}},
        "done": {"x.bin": 50 * MB},
    }
    v = plain(spec, tmp_path, pending=("a/b/c/d/e",))
    store = v.store

    def marked() -> tuple[set[str], bool]:
        frame = v.render()
        rows = np.flatnonzero(frame.flags & F_SCANNING)
        return {store.name(int(frame.draw.node[r])) for r in rows}, bool(frame.root.flags & F_SCANNING)

    assert marked() == (set(), False)  # the scan is not running
    v.scanning = True
    assert marked() == ({"a", "b", "c"}, True)
    for visual in ("test.rooted", "test.fake"):
        v.set_visual(visual)
        assert marked() == ({"a", "b", "c"}, True)
    v.zoom_to_node(find(store, "done"))
    assert marked() == (set(), False)
    v.zoom_to_node(find(store, "a/b/c/d/e"))  # not listed itself
    frame = v.render()
    assert frame.root.kind == RectKind.PENDING and frame.root.flags & F_SCANNING
    store.clear(find(store, "a/b/c/d/e"), Flag.PENDING)
    v.zoom_to_node(1)
    assert marked() == (set(), False)


def test_flashes_land_on_the_zoom_root_in_partition_layouts(tmp_path: Path) -> None:
    v = plain({"big.bin": 40 * GB, "tiny.txt": 1, "Other": {"o.bin": 10 * GB}}, tmp_path)
    store = v.store
    tiny, other, o_bin = find(store, "tiny.txt"), find(store, "Other"), find(store, "Other/o.bin")
    v.render()
    v.add_flash({tiny: 4096})
    assert v.render().flash == []  # the treemap does not draw its root: a block too small is dropped
    v.add_flash({tiny: 4096, o_bin: 10})
    v.set_visual("test.rooted")  # a switch between the change and the frame
    frame = v.render()
    row = frame.draw.row(o_bin)
    assert row is not None and frame.flash == [(-1, 4096), (row, 10)]
    v.zoom_to_node(other)
    v.render()
    v.add_flash({tiny: 5, other: 7})
    assert v.render().flash == [(-1, 7)]  # outside the zoom root: nothing; the root itself: -1


# ------------------------------------------------------------------ selection and reveal


def test_a_selected_aggregate_falls_back_to_its_node(view: View) -> None:
    store = view.store
    lenny, temp = find(store, "Users/lenny"), find(store, "Users/lenny/Temp")
    view.zoom_to_node(lenny)
    frame = view.render()
    small = KEY_SMALL | temp
    assert frame.draw.row(small) is not None
    view.select(small)
    assert view.render().selected_row == frame.draw.row(small)
    view.set_detail(1)  # Temp is collapsed: its small items are not a block of their own
    frame = view.render()
    assert frame.draw.row(small) is None and frame.selected_row == frame.draw.row(temp)
    view.set_detail(5)
    view.zoom_to_node(temp)
    view.select(small)
    view.min_element = 1  # every file drawn on its own
    frame = view.render()
    assert frame.draw.row(small) is None and frame.selected_row is None  # the zoom root is no row
    view.min_element = 14
    view.zoom_to_node(lenny)
    view.select(KEY_MORE | temp)
    assert view.render().selected_row == view.render().draw.row(temp)
    view.zoom_to_node(find(store, "Users"))
    view.select(KEY_FREE)  # free space has no node to fall back to
    assert view.render().selected_row is None


def test_reveal_uses_the_levels_the_visualization_draws(tmp_path: Path) -> None:
    deep: dict[str, object] = {"f.bin": 10 * GB}
    for name in ("e", "d", "c", "b", "a"):
        deep = {name: deep}
    v = plain(deep, tmp_path)
    store = v.store
    f = find(store, "a/b/c/d/e/f.bin")
    v.render()
    assert v.reveal(f) and v.zoom.node == find(store, "a")  # six levels down, five drawn
    assert v.render().draw.row(f) is not None
    v.go_home()
    v.set_visual("test.shallow")
    v.render()
    assert v.reveal(f) and v.zoom.node == find(store, "a/b/c/d/e")  # it draws one level only
    assert v.render().draw.row(f) is not None


# ------------------------------------------------------------------ branch colours


def top_slots(frame: Frame) -> dict[int, int]:
    top = np.flatnonzero(frame.draw.parent < 0)
    return {int(frame.draw.key[r]): int(frame.branch[r]) for r in top}


def test_branch_slots_are_stable_and_inherited(view: View) -> None:
    frame = view.render()
    slots = top_slots(frame)
    real = {k: s for k, s in slots.items() if k < KEY_SMALL}
    assert real and all(1 <= s <= SLOTS for s in real.values())
    assert slots[KEY_FREE] == 0 and slots[KEY_UNKNOWN] == 0
    order = [s for k, s in slots.items() if k < KEY_SMALL]
    assert all(a != b for a, b in pairwise(order))  # neighbours differ
    parent = frame.draw.parent
    inner = parent >= 0
    assert np.array_equal(frame.branch[inner], frame.branch[parent[inner]])  # down from the parent
    # another viewport, another visualization: the same branch keeps its slot
    view.set_viewport(700, 900, 1.5)
    view.set_visual("test.rooted")
    again = view.render()
    assert {k: s for k, s in top_slots(again).items() if k in real} == real


def test_branch_slots_reset_with_the_zoom_root_and_follow_remap(view: View) -> None:
    store = view.store
    users = find(store, "Users")
    first = top_slots(view.render())
    view.zoom_to_node(users)
    assert top_slots(view.render()) == {find(store, "Users/lenny"): 1}  # fresh slots
    view.go_back()
    assert top_slots(view.render()) == first
    # node ids move by one (a listing came in before them): the slots move along
    hues = view.hues
    before = dict(hues._slots)
    view.remap(lambda ids: np.where(ids >= 2, ids + 1, ids), 1)
    assert dict(hues._slots) == {k + 1: s for k, s in before.items()}
    view.remap(lambda ids: np.full(len(ids), -1, dtype=np.int64), 1)  # the zoom root went
    assert not hues._slots


def make_draw(keys: list[int], kinds: list[int] | None = None) -> DrawList:
    """Top-level rows only (enough for branch slots)."""
    n = len(keys)
    zeros = np.zeros(n, dtype=np.int32)
    return DrawList(
        key=np.array(keys, dtype=np.uint32),
        node=np.array(keys, dtype=np.int32),
        parent=np.full(n, -1, dtype=np.int32),
        kind=np.array(kinds or [0] * n, dtype=np.uint8),
        depth=np.ones(n, dtype=np.uint8),
        cls=np.zeros(n, dtype=np.uint8),
        label=np.zeros(n, dtype=np.uint8),
        size=np.ones(n, dtype=np.int64),
        count=np.zeros(n, dtype=np.uint32),
        names=[""] * n,
        geometry={"x": zeros, "y": zeros, "w": zeros, "h": zeros},
    )


def test_branch_slot_choice() -> None:
    hues = BranchHues()
    got = hues.hues(1, make_draw([10, 11, 12, KEY_SMALL | 1, 13], [0, 1, 2, 4, 0])).tolist()
    assert got == [1, 2, 3, 0, 4]  # least used first, the aggregate neutral
    # 20 appears between 10 and 11: least used among the slots its neighbours don't have
    got = hues.hues(1, make_draw([10, 20, 11, 12, 13])).tolist()
    assert got == [1, 5, 2, 3, 4]
    # more branches than slots: the least used again, never the neighbour's
    got = hues.hues(1, make_draw(list(range(30, 30 + SLOTS + 2)))).tolist()
    assert got[:SLOTS] == list(range(1, SLOTS + 1)) and got[SLOTS:] == [1, 2]
    hues.hues(1, make_draw(list(range(100, 100 + LIMIT + 50))))
    assert len(hues._slots) == LIMIT  # the least recently drawn go first
    assert 100 not in hues._slots and 100 + LIMIT + 49 in hues._slots
    assert hues.hues(2, make_draw([100])).tolist() == [1]  # another zoom root: fresh slots


def test_branch_slots_wrap_around_like_the_rings() -> None:
    # the polar view's first and last sectors meet at 12 o'clock: they never share a hue either
    got = BranchHues().hues(1, make_draw(list(range(10, 10 + SLOTS + 1)))).tolist()
    assert got[-1] not in (got[0], got[-2])


def test_row_lookup_scans_then_indexes() -> None:
    keys = [7, 3, KEY_SMALL | 3, 9, 3, 1]  # a key twice: the first row wins
    draw = make_draw(keys)
    probes = [3, 9, 1, 7, KEY_SMALL | 3, 4, 0, 2**31, 3, 9, 1, 5]  # more than ROW_SCANS lookups
    assert len(probes) > ROW_SCANS
    expect = [keys.index(k) if k in keys else None for k in probes]
    assert [draw.row(k) for k in probes] == expect
    assert draw._index is not None  # indexed by now, same answers
    assert [draw.row(k) for k in probes] == expect

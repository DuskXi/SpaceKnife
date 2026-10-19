"""View state machine: navigation history, detail, free/unknown, filter dim/hide, tags, colours, selection."""

from __future__ import annotations

import time
from pathlib import Path

import numpy as np
import pytest

from spaceknife.layout.engine import KEY_FREE, KEY_SMALL, KEY_UNKNOWN, RectKind
from spaceknife.store.nodes import Flag
from spaceknife.view import TagStore, View
from spaceknife.view.state import (
    F_DIM,
    F_FOLDER_HIT,
    F_MATCH,
    F_ON_PATH,
    F_SCANNING,
    F_SELECTED,
    F_TAGGED,
    RootInfoLike,
)

from .store_builder import build, find

MB = 1 << 20
GB = 1 << 30

SPEC: dict[str, object] = {
    "Users": {
        "lenny": {
            "Videos": {"trip.mp4": 900 * MB, "wedding.mov": 600 * MB},
            "Music": {"a.mp3": 40 * MB, "b.flac": 60 * MB},
            "Temp": {f"t{i}.tmp": 5_000 for i in range(400)} | {"big.log": 80 * MB},
        }
    },
    "Windows": {"System32": {"kernel.dll": 30 * MB, "drivers": {"x.sys": 5 * MB}}},
    "pagefile.sys": 300 * MB,
}


@pytest.fixture
def view(tmp_path: Path) -> View:
    store = build(SPEC, root="C:\\")
    roots = [RootInfoLike(node=1, whole_volume=True, free=500 * MB, used=2200 * MB)]
    v = View("v1", store, roots, tags=TagStore(tmp_path / "tags.json"))
    v.set_viewport(1200, 800, 1.0)
    return v


def row_of(frame_view: View, path: str) -> int:
    frame = frame_view.last
    assert frame is not None
    node = find(frame_view.store, path)
    hit = np.nonzero(frame.draw.node == node)[0]
    assert hit.size, path
    return int(hit[0])


def test_volume_root_shows_free_and_unknown_space(view: View) -> None:
    frame = view.render()
    keys = set(frame.draw.key.tolist())
    assert {KEY_FREE, KEY_UNKNOWN} <= keys
    unknown = frame.draw.size[frame.draw.row(KEY_UNKNOWN)]
    assert unknown == 2200 * MB - int(view.store.cols["tot_alloc"][1])
    view.show_free = False
    assert KEY_FREE not in view.render().draw.key.tolist()
    view.show_unknown = False
    assert KEY_UNKNOWN not in view.render().draw.key.tolist()
    view.show_free = view.show_unknown = True
    view.zoom_to_node(find(view.store, "Users"))
    assert KEY_FREE not in view.render().draw.key.tolist()  # only at the volume root


def test_zoom_back_forward_up_home(view: View) -> None:
    view.render()
    users = find(view.store, "Users")
    assert view.zoom_into(users)
    view.render()
    lenny = find(view.store, "Users/lenny")
    view.select(lenny)
    assert view.zoom_into(lenny)
    view.render()
    assert [k for k, _ in view.breadcrumb()] == [1, users, lenny]
    assert view.go_back()
    assert view.zoom.node == users and view.selected == lenny  # the selection of that moment comes back
    assert view.go_forward()
    assert view.zoom.node == lenny
    assert view.go_up()
    assert view.zoom.node == users and view.selected == lenny  # the folder just left is selected
    assert view.go_home()
    assert view.zoom.node == 1
    assert not view.go_home() and not view.go_up() and not view.can_go_up()
    while view.go_back():
        pass
    assert view.zoom.node == 1 and not view.back


def test_free_space_cannot_be_zoomed(view: View) -> None:
    view.render()
    assert not view.zoom_into(KEY_FREE)
    assert not view.zoom_into(KEY_UNKNOWN)


def test_zoom_into_small_items_and_back_up(view: View) -> None:
    temp = find(view.store, "Users/lenny/Temp")
    view.zoom_to_node(temp)
    frame = view.render()
    key = KEY_SMALL | temp
    row = frame.draw.row(key)
    assert row is not None and frame.draw.count[row] == 400
    assert view.zoom_into(key)
    inner = view.render()
    assert len(inner.draw) > 100  # the small files are spread out
    assert view.breadcrumb()[-1] == (key, "small:400")
    assert view.go_up()
    assert view.zoom.node == temp and view.selected == key
    assert view.render().selected_row == row


def test_zooming_into_a_huge_small_items_block_always_makes_progress(tmp_path: Path) -> None:
    store = build({"Big": {"big.iso": 4 * GB} | {f"f{i}.bin": 4096 for i in range(20_000)}})
    view = View("v", store, [RootInfoLike(node=1, whole_volume=False)], tags=TagStore(tmp_path / "t.json"))
    view.set_viewport(1200, 800, 1.0)
    big = find(store, "Big")
    view.render()
    key: int | None = KEY_SMALL | big
    keys: list[int] = []
    counts = [20_000]
    while key is not None:
        assert view.zoom_into(key)
        keys.append(key)
        frame = view.render()
        assert (frame.draw.kind == RectKind.FILE).sum() > 1000  # single files are visible
        aggregates = np.flatnonzero(frame.draw.kind != RectKind.FILE)
        key = None
        if aggregates.size:
            (row,) = aggregates
            key = int(frame.draw.key[row])
            assert key not in keys and frame.draw.count[row] < counts[-1]
            counts.append(int(frame.draw.count[row]))
    assert len(keys) == 5 and counts[-1] < 4000
    crumbs = view.breadcrumb()
    assert [k for k, _ in crumbs] == [1, big, *keys]
    assert [label for _, label in crumbs[2:]] == ["small:20000"] + [f"more:{n}" for n in counts[1:]]
    # up retraces the zooms one crumb at a time, selecting the block just left
    for k in reversed(keys):
        assert view.go_up()
        frame = view.render()
        row = frame.draw.row(k)
        assert view.selected == k and row is not None and frame.selected_row == row
    assert view.zoom.node == big and view.zoom.members is None


@pytest.mark.parametrize("at", ["", "Data/"])
def test_selection_in_a_small_items_block_too_small_to_draw_marks_its_folder(tmp_path: Path, at: str) -> None:
    x: dict[str, object] = {"big.bin": 40 * GB, "a.txt": 10, "b.txt": 10}
    spec: dict[str, object] = {"Data": {"X": x}} if at else {"X": x}
    spec["Y"] = {"y.bin": 20 * GB}
    store = build(spec)
    view = View("v", store, [RootInfoLike(node=1, whole_volume=False)], tags=TagStore(tmp_path / "t.json"))
    view.set_viewport(1200, 800, 1.0)
    folder, a = find(store, f"{at}X"), find(store, f"{at}X/a.txt")
    frame = view.render()
    assert frame.draw.row(KEY_SMALL | folder) is None and a in frame.draw.members[KEY_SMALL | folder]
    view.select(a)
    frame = view.render()
    assert frame.selected_row is not None and frame.selected_row == frame.draw.row(folder)
    assert frame.flags[frame.selected_row] & F_SELECTED
    on_path = {int(frame.draw.node[r]) for r in np.flatnonzero(frame.flags & F_ON_PATH)}
    assert on_path == ({find(store, "Data")} if at else set())


def test_detail_level_is_clamped(view: View) -> None:
    assert view.set_detail(0) == 1
    assert view.set_detail(99) == 15
    view.set_detail(1)
    frame = view.render()
    assert not (frame.draw.kind == RectKind.FOLDER).any()


def test_selection_inside_a_collapsed_folder_marks_the_visible_ancestor(view: View) -> None:
    view.set_detail(2)
    view.render()
    x_sys = find(view.store, "Windows/System32/drivers/x.sys")
    view.select(x_sys)
    frame = view.render()
    assert frame.selected_row is not None
    sel_node = int(frame.draw.node[frame.selected_row])
    assert sel_node == find(view.store, "Windows/System32")
    assert frame.flags[frame.selected_row] & F_SELECTED
    windows_row = row_of(view, "Windows")
    assert frame.flags[windows_row] & F_ON_PATH


def test_filter_dim_keeps_the_layout(view: View) -> None:
    plain = view.render()
    view.set_filter("*.mp4;*.mov")
    dim = view.render()
    for col in ("key", "x", "y", "w", "h", "kind"):
        assert np.array_equal(getattr(plain.draw, col), getattr(dim.draw, col))
    trip = row_of(view, "Users/lenny/Videos/trip.mp4")
    music = row_of(view, "Users/lenny/Music")
    users = row_of(view, "Users")
    assert dim.flags[trip] & F_MATCH and not dim.flags[trip] & F_DIM
    assert dim.flags[music] & F_DIM
    assert dim.flags[users] & F_MATCH  # contains matches
    assert dim.filter_totals == (2, 1500 * MB, 1500 * MB)


def test_filter_hide_lays_out_matches_only(view: View) -> None:
    view.filter_mode = "hide"
    view.set_filter(":class:video")
    frame = view.render()
    nodes = set(frame.draw.node.tolist())
    assert find(view.store, "Users/lenny/Music") not in nodes
    assert KEY_FREE not in frame.draw.key.tolist()
    users = row_of(view, "Users")
    assert frame.draw.size[users] == 1500 * MB  # matched part only


def test_filter_with_errors_keeps_the_last_valid_result(view: View) -> None:
    view.set_filter("*.mp4")
    good = view.filter.result
    state = view.set_filter("*.mp4;>500xb")
    assert state.stale and state.diagnostics[0].code == "unknown_unit"
    assert view.filter.result is good
    view.set_filter("")
    assert not view.filter.active
    frame = view.render()
    assert not (frame.flags & (F_MATCH | F_DIM)).any()


def test_folder_mask_hits_are_flagged(view: View) -> None:
    view.set_filter("\\temp")
    frame = view.render()
    assert frame.flags[row_of(view, "Users/lenny/Temp")] & F_FOLDER_HIT
    assert not frame.flags[row_of(view, "Users/lenny/Music")] & F_FOLDER_HIT


def test_tags_mark_inherit_persist_and_clear(view: View, tmp_path: Path) -> None:
    videos = find(view.store, "Users/lenny/Videos")
    view.select(videos)
    assert view.tag_selected("green")
    frame = view.render()
    row = row_of(view, "Users/lenny/Videos")
    assert frame.tag[row] == 3 and frame.flags[row] & F_TAGGED
    trip = row_of(view, "Users/lenny/Videos/trip.mp4")
    assert frame.tag[trip] == 0  # the marker is only on the tagged element
    view.set_filter(":green")
    assert view.filter.result is not None and view.filter.result.totals(1)[0] == 2  # inherited by both videos
    # persisted by path
    again = TagStore(tmp_path / "tags.json")
    assert again.of(view.store, videos) == "green"
    # Ctrl+0 on an ancestor clears everything below
    view.select(find(view.store, "Users"))
    assert view.tag_selected(None)
    assert view.tags.of(view.store, videos) is None
    assert view.filter.result is not None and view.filter.result.totals(1)[0] == 0  # refiltered


def test_time_tones(view: View) -> None:
    now = int(time.time())
    c = view.store.cols
    for path, age in (
        ("pagefile.sys", 3600),
        ("Windows/System32/kernel.dll", 3 * 86400),
        ("Users/lenny/Videos/trip.mp4", 400 * 86400),
    ):
        c["mtime"][find(view.store, path)] = now - age
    view.color_mode = "time"
    frame = view.render()
    assert frame.tone[row_of(view, "pagefile.sys")] == 0
    view.zoom_to_node(find(view.store, "Windows/System32"))
    frame = view.render()
    assert frame.tone[row_of(view, "Windows/System32/kernel.dll")] == 1
    view.zoom_to_node(find(view.store, "Users/lenny/Videos"))
    frame = view.render()
    assert frame.tone[row_of(view, "Users/lenny/Videos/trip.mp4")] == 4


def test_views_of_one_scan_are_independent(view: View) -> None:
    other = View("v2", view.store, view.roots, tags=view.tags, layout=view.layout)
    other.set_viewport(600, 400)
    view.zoom_to_node(find(view.store, "Users"))
    view.set_detail(3)
    view.set_filter("*.mp4")
    assert other.zoom.node == 1 and other.detail == 5 and not other.filter.active
    assert len(other.render().draw) > 0


def test_double_clicking_a_file_zooms_to_its_folder_and_selects_it(view: View) -> None:
    trip = find(view.store, "Users/lenny/Videos/trip.mp4")
    assert view.zoom_to_node(trip)
    assert view.zoom.node == find(view.store, "Users/lenny/Videos") and view.selected == trip
    frame = view.render()
    assert frame.selected_row is not None and int(frame.draw.node[frame.selected_row]) == trip
    # already in its folder: only the selection changes, no history entry
    depth = len(view.back)
    view.select(None)
    assert view.zoom_into(trip)
    assert view.selected == trip and len(view.back) == depth


def test_folders_still_being_scanned_are_marked_on_the_top_three_levels(tmp_path: Path) -> None:
    spec: dict[str, object] = {
        "a": {"b": {"c": {"d": {"e": {"f.bin": 1}, "h.bin": 20 * MB}, "g.bin": 30 * MB}}},
        "done": {"x.bin": 50 * MB},
    }
    store = build(spec, pending=("a/b/c/d/e",))
    v = View("v1", store, [RootInfoLike(node=1, whole_volume=False)], tags=TagStore(tmp_path / "tags.json"))
    v.set_viewport(1200, 800, 1.0)

    def marked(frame_view: View) -> set[str]:
        frame = frame_view.render()
        rows = np.flatnonzero(frame.flags & F_SCANNING)
        return {frame_view.store.name(int(frame.draw.node[r])) for r in rows}

    assert marked(v) == set()  # the scan is not running: nothing breathes
    v.scanning = True
    assert marked(v) == {"a", "b", "c"}  # d is the fourth level; "done" holds nothing unlisted
    assert v.zoom_into(find(store, "a"))
    assert marked(v) == {"b", "c", "d"}  # levels count from the view root
    store.clear(find(store, "a/b/c/d/e"), Flag.PENDING)
    assert marked(v) == set()

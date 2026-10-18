"""W3 result list: rows grouped by the visible block, totals, order, locating a result."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from spaceknife.layout.engine import KEY_SMALL, RectKind
from spaceknife.view import TagStore, View
from spaceknife.view.results import effective_tag, relative_path, result_rows, sort_rows, sorted_rows
from spaceknife.view.state import RootInfoLike

from .store_builder import build, find

MB = 1 << 20

SPEC: dict[str, object] = {
    "Users": {
        "lenny": {
            "Videos": {"trip.mp4": 900 * MB, "wedding.mov": 600 * MB, "clip.mp4": 0},
            "Music": {"a.mp3": 40 * MB, "b.flac": 60 * MB},
            "Temp": {f"t{i}.tmp": 5_000 for i in range(400)} | {"big.log": 80 * MB},
            "Deep": {"a": {"b": {"c": {"d": {"far.mp4": 50 * MB}}}}},
        }
    },
    "Windows": {"System32": {"kernel.dll": 30 * MB, "drivers": {"x.sys": 5 * MB}}},
    "pagefile.sys": 300 * MB,
}


@pytest.fixture
def view(tmp_path: Path) -> View:
    store = build(SPEC, root="C:\\")
    roots = [RootInfoLike(node=1, whole_volume=True, free=500 * MB, used=2600 * MB)]
    v = View("v1", store, roots, tags=TagStore(tmp_path / "tags.json"))
    v.set_viewport(1200, 800, 1.0)
    return v


def filtered(view: View, text: str) -> None:
    view.set_filter(text)
    assert view.filter.active
    view.render()


def check_totals(view: View) -> None:
    frame = view.last
    assert frame is not None and frame.filter_totals is not None
    rows = result_rows(view)
    files, alloc, size = frame.filter_totals
    assert sum(r.count for r in rows) == files
    assert sum(r.alloc for r in rows) == alloc
    assert sum(r.size for r in rows) == size


def test_drawn_files_are_their_own_rows(view: View) -> None:
    filtered(view, "*.mov;trip.mp4")
    rows = sort_rows(result_rows(view), "size")
    assert [(r.kind, r.name, r.count) for r in rows] == [("file", "trip.mp4", 1), ("file", "wedding.mov", 1)]
    assert relative_path(view, rows[0].parent) == os.sep.join(["Users", "lenny", "Videos"])
    assert rows[0].key == find(view.store, "Users/lenny/Videos/trip.mp4")
    check_totals(view)


def test_collapsed_folder_aggregates_its_matches(view: View) -> None:
    view.set_detail(1)
    filtered(view, "*.mp4;*.mov")
    rows = result_rows(view)
    assert len(rows) == 1
    (row,) = rows
    assert (row.kind, row.name, row.count) == ("folder", "Users", 4)  # trip, wedding, clip (0 B), far
    assert row.alloc == (900 + 600 + 50) * MB
    assert relative_path(view, row.parent) == ""
    check_totals(view)


def test_small_items_become_one_group_row(view: View) -> None:
    view.zoom_to_node(find(view.store, "Users/lenny/Temp"))
    filtered(view, "*.tmp")
    rows = result_rows(view)
    groups = [r for r in rows if r.kind == "group"]
    assert groups and all(r.key >= KEY_SMALL for r in groups)
    assert sum(r.count for r in rows) == 400
    check_totals(view)


def test_zero_size_and_undrawn_matches_still_count(view: View) -> None:
    filtered(view, "clip.mp4")
    rows = result_rows(view)
    assert sum(r.count for r in rows) == 1  # 0 B: not drawn, owned by the nearest drawn block
    check_totals(view)


@pytest.mark.parametrize("detail", [1, 2, 3, 5, 9])
@pytest.mark.parametrize("text", ["*.mp4", "*", ">1mb", "*.tmp;*.sys", "\\temp", "|*.tmp"])
@pytest.mark.parametrize("mode", ["dim", "hide"])
def test_rows_add_up_to_the_totals(view: View, detail: int, text: str, mode: str) -> None:
    view.set_detail(detail)
    view.filter_mode = mode  # type: ignore[assignment]
    filtered(view, text)
    check_totals(view)
    view.zoom_to_node(find(view.store, "Users/lenny"))
    view.render()
    check_totals(view)


def test_rows_inside_an_aggregate_zoom(view: View) -> None:
    view.zoom_to_node(find(view.store, "Users/lenny/Temp"))
    frame = view.render()
    small = next(
        int(k) for k, kind in zip(frame.draw.key, frame.draw.kind, strict=True) if kind == RectKind.SMALL
    )
    assert view.zoom_into(small)
    filtered(view, "*.tmp")
    rows = result_rows(view)
    assert rows and sum(r.count for r in rows) == view.last.filter_totals[0]  # type: ignore[union-attr]
    check_totals(view)


def test_a_huge_aggregate_lists_its_members_and_opens_its_rest(tmp_path: Path) -> None:
    store = build({"Big": {"big.iso": 4 << 30} | {f"f{i}.bin": 4096 for i in range(20_000)}})
    view = View("v", store, [RootInfoLike(node=1, whole_volume=False)], tags=TagStore(tmp_path / "t.json"))
    view.set_viewport(1200, 800, 1.0)
    view.render()
    assert view.zoom_into(KEY_SMALL | find(store, "Big"))
    filtered(view, "*.bin")
    rows = result_rows(view)
    files = [r for r in rows if r.kind == "file"]
    (group,) = [r for r in rows if r.kind == "group"]
    assert len(files) > 1000 and len(files) + group.count == 20_000
    check_totals(view)
    # a member of the leftover block is located there, without leaving the zoom
    frame = view.last
    assert frame is not None
    member = int(frame.draw.members[group.key][-1])
    depth = len(view.back)
    assert view.reveal(member)
    frame = view.render()
    assert len(view.back) == depth and frame.selected_row == frame.draw.row(group.key)
    # Enter on the group row opens it
    assert view.zoom_into(group.key)
    view.render()
    rows = result_rows(view)
    assert sum(r.count for r in rows) == group.count and any(r.kind == "file" for r in rows)
    check_totals(view)


def test_sorting_and_cache(view: View) -> None:
    filtered(view, "*.mp4;*.mov;*.mp3;*.flac")
    by_size = sorted_rows(view)
    assert by_size == sorted(by_size, key=lambda r: -r.alloc)
    assert sorted_rows(view) is by_size  # same frame, same result, same order: cached
    view.results_sort = "name"
    names = [r.name for r in sorted_rows(view)]
    assert names == sorted(names, key=str.casefold)
    view.results_sort = "count"
    counts = [r.count for r in sorted_rows(view)]
    assert counts == sorted(counts, reverse=True)
    view.render()
    assert sorted_rows(view) is not by_size


def test_effective_tag_inherits_from_folders(view: View) -> None:
    videos = find(view.store, "Users/lenny/Videos")
    trip = find(view.store, "Users/lenny/Videos/trip.mp4")
    view.tags.set(view.store, videos, "green")
    assert effective_tag(view, trip) == ("green", True)
    assert effective_tag(view, videos) == ("green", False)
    view.tags.set(view.store, trip, "red")
    assert effective_tag(view, trip) == ("red", False)


def test_reveal_a_drawn_block_only_selects(view: View) -> None:
    view.render()
    users = find(view.store, "Users")
    assert view.reveal(users)
    assert view.selected == users and not view.back
    assert not view.reveal(users)


def test_reveal_navigates_to_show_a_collapsed_file(view: View) -> None:
    view.set_detail(2)
    view.render()
    far = find(view.store, "Users/lenny/Deep/a/b/c/d/far.mp4")
    assert view.reveal(far)
    frame = view.render()
    assert frame.draw.row(far) is not None
    assert view.selected == far and frame.selected_row == frame.draw.row(far)
    assert view.go_back() and view.zoom.node == 1  # recorded in history


def test_reveal_selects_a_member_of_a_small_block(view: View) -> None:
    temp = find(view.store, "Users/lenny/Temp")
    view.zoom_to_node(temp)
    frame = view.render()
    t0 = find(view.store, "Users/lenny/Temp/t0.tmp")
    assert frame.draw.row(t0) is None
    depth = len(view.back)
    assert view.reveal(t0)
    frame = view.render()
    assert view.zoom.node == temp and len(view.back) == depth  # no navigation
    assert frame.selected_row is not None and frame.draw.kind[frame.selected_row] == RectKind.SMALL


def test_reveal_outside_the_zoom_root(view: View) -> None:
    view.zoom_to_node(find(view.store, "Windows"))
    view.render()
    trip = find(view.store, "Users/lenny/Videos/trip.mp4")
    assert view.reveal(trip)
    frame = view.render()
    assert frame.draw.row(trip) is not None and view.selected == trip

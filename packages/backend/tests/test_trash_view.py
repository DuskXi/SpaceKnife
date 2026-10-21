"""Blocks in a recycle bin / trash carry F_TRASH (view/trash.py): Windows, macOS and Linux bins, by
path, never by name alone."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from spaceknife.layout.engine import KEY_UNKNOWN
from spaceknife.store.nodes import NodeStore
from spaceknife.view import TagStore, View, trash
from spaceknife.view.state import F_TRASH, F_TRASH_BIN, RootInfoLike
from spaceknife.view.trash import WithoutBins, find_bins, in_trash, is_trash_path, subtree

from .store_builder import build, find

MB = 1 << 20


@pytest.mark.parametrize(
    "path",
    [
        r"C:\$Recycle.Bin",
        "D:/$RECYCLE.BIN",  # a Windows snapshot read on Linux
        r"\\server\share\$Recycle.Bin",
        r"E:\RECYCLER",
        "/Users/lenny/.Trash",
        "/Volumes/USB/.Trashes",
        "/.Trashes",
        "/home/lenny/.local/share/Trash",
        "/media/lenny/USB/.Trash-1000",
    ],
)
def test_bins(path: str) -> None:
    assert is_trash_path(path)


@pytest.mark.parametrize(
    "path",
    [
        r"C:\Users\lenny\$Recycle.Bin",  # only at the top of a drive
        "/home/lenny/Trash",
        "/home/lenny/.Trash-abc",
        "/Users/lenny/Documents/.Trash",
        "/home/lenny/.local/share/Trash/files",  # inside, not the bin
        "/mnt/usb/.Trash",  # a mount's shared trash only counts at the top of a scan
    ],
)
def test_lookalikes(path: str) -> None:
    assert not is_trash_path(path)


def test_shared_trash_at_the_top_of_a_scanned_mount() -> None:
    assert is_trash_path("/mnt/usb/.Trash", top=True)


def marks(store: NodeStore, *paths: str) -> list[bool]:
    nodes = np.array([find(store, p) for p in paths], dtype=np.int64)
    return in_trash(store, nodes).tolist()


def test_nodes_in_the_bins_of_each_system() -> None:
    win = build(
        {
            "$Recycle.Bin": {"S-1-5-21": {"$RX1.mp4": 700 * MB, "$IX1.mp4": 544}},
            "Users": {"lenny": {"$Recycle.Bin": {"a.txt": 10}, "b.txt": 10}},
        },
        root="C:\\",
    )
    assert marks(win, "$Recycle.Bin", "$Recycle.Bin/S-1-5-21", "$Recycle.Bin/S-1-5-21/$RX1.mp4") == [True] * 3
    assert marks(win, "Users", "Users/lenny/$Recycle.Bin/a.txt", "Users/lenny/b.txt") == [False] * 3
    posix = build(
        {
            "Users": {"lenny": {".Trash": {"old.dmg": 5 * MB}, "Documents": {".Trash": {"x": 1}}}},
            "home": {
                "lenny": {".local": {"share": {"Trash": {"files": {"y": 1}, "info": {"y.trashinfo": 1}}}}}
            },
            "media": {"usb": {".Trash-1000": {"files": {"z": 1}}}},
        },
        root="/",
    )
    inside = [
        "Users/lenny/.Trash/old.dmg",
        "home/lenny/.local/share/Trash/files/y",
        "home/lenny/.local/share/Trash/info/y.trashinfo",
        "media/usb/.Trash-1000/files/z",
    ]
    assert marks(posix, *inside) == [True] * 4
    assert (
        marks(posix, "Users/lenny/Documents/.Trash/x", "home/lenny/.local/share", "media/usb") == [False] * 3
    )
    # no node, the invisible top and ids past the end are never in a bin
    assert in_trash(posix, np.array([-1, 0, len(posix) + 5], dtype=np.int64)).tolist() == [False] * 3


def test_a_scanned_bin_and_the_shared_trash_of_a_scanned_mount() -> None:
    bin_only = build({"files": {"y": 1}}, root="/home/lenny/.local/share/Trash")
    assert marks(bin_only, "files", "files/y") == [True, True]
    mount = build({".Trash": {"1000": {"f": 1}}, "data": {"g": 1}}, root="/mnt/usb")
    assert marks(mount, ".Trash/1000/f", "data/g") == [True, False]


def test_frames_flag_the_bin_and_what_is_in_it(tmp_path: Path) -> None:
    store = build(
        {
            "$Recycle.Bin": {"S-1-5-21": {"$RX1.mp4": 700 * MB, "$RX2.iso": 400 * MB}},
            "Users": {"lenny": {"Videos": {"trip.mp4": 900 * MB}}},
        },
        root="C:\\",
    )
    roots = [RootInfoLike(node=1, whole_volume=True, free=500 * MB, used=2100 * MB)]
    view = View("v1", store, roots, tags=TagStore(tmp_path / "tags.json"))
    view.set_viewport(1200, 800, 1.0)
    frame = view.render()
    nodes = frame.draw.node.astype(np.int64)

    def flagged(path: str, flag: int = F_TRASH) -> bool:
        rows = np.nonzero(nodes == find(store, path))[0]
        assert rows.size, path
        return bool(frame.flags[rows[0]] & flag)

    assert flagged("$Recycle.Bin") and flagged("$Recycle.Bin/S-1-5-21/$RX1.mp4")
    assert not flagged("Users") and not flagged("Users/lenny/Videos/trip.mp4")
    # the bin itself, and only the bin, is the bin
    assert flagged("$Recycle.Bin", F_TRASH_BIN) and not flagged("$Recycle.Bin/S-1-5-21", F_TRASH_BIN)
    assert not (frame.flags[nodes < 0] & F_TRASH).any()  # free and unknown space
    assert not frame.root.flags & F_TRASH
    # zoomed into the bin: the root bar / centre disc is in it as well
    assert view.zoom_to_node(find(store, "$Recycle.Bin"))
    assert view.render().root.flags & (F_TRASH | F_TRASH_BIN) == F_TRASH | F_TRASH_BIN


# ------------------------------------------------------------------ the 「回收站」 chip


def test_every_bin_of_a_store_is_found() -> None:
    store = build(
        {
            "$Recycle.Bin": {"S-1-5-21": {"$R1.mp4": 700 * MB}},
            "Users": {
                "lenny": {
                    "$Recycle.Bin": {"a": 1},  # not at the top of the drive
                    ".local": {"share": {"Trash": {"files": {"y": 1}}}},
                    "Videos": {"trip.mp4": 900 * MB},
                }
            },
            "Trash": {"x": 1},  # the right name in the wrong place
        },
        root="C:\\",
    )
    found = find_bins(store).tolist()
    assert sorted(store.path(i) for i in found) == sorted(
        [store.path(find(store, "$Recycle.Bin")), store.path(find(store, "Users/lenny/.local/share/Trash"))]
    )
    assert find_bins(build({"Videos": {"a.mp4": 1}}, root="D:\\")).tolist() == []


def test_the_bins_are_looked_up_again_by_path_between_full_searches(monkeypatch: pytest.MonkeyPatch) -> None:
    store = build({"$Recycle.Bin": {"x": 1}, "data": {"y": 1}}, root="C:\\")
    first = find_bins(store).tolist()
    store.version += 1  # a change (monitoring): within FULL_SEARCH_S the known paths are resolved again
    calls: list[int] = []
    real = trash._search
    monkeypatch.setattr(trash, "_search", lambda s: calls.append(1) or real(s))
    assert find_bins(store).tolist() == first and calls == []
    monkeypatch.setattr(trash, "FULL_SEARCH_S", 0.0)
    store.version += 1
    assert find_bins(store).tolist() == first and calls == [1]


def test_without_bins_takes_their_bytes_out() -> None:
    store = build(
        {
            "$Recycle.Bin": {"S-1": {"a.mp4": 700 * MB, "b": {"c.iso": 300 * MB}}},
            "Users": {"v.mp4": 900 * MB},
        },
        root="C:\\",
    )
    from spaceknife.layout.api import StoreSizes
    from spaceknife.layout.basis import Basis

    base = StoreSizes(Basis(store))
    sizes = WithoutBins(base, store, find_bins(store))
    root, bin_, users = find(store, ""), find(store, "$Recycle.Bin"), find(store, "Users")
    inside = [
        find(store, p) for p in ("$Recycle.Bin/S-1", "$Recycle.Bin/S-1/a.mp4", "$Recycle.Bin/S-1/b/c.iso")
    ]
    ids = np.array([root, bin_, users, *inside], dtype=np.int64)
    got = sizes.of(ids).tolist()
    assert got[0] == int(base.of(np.array([root]))[0]) - 1000 * MB  # the root lost the bin
    assert got[1:] == [0, 900 * MB, 0, 0, 0]
    full = sizes.full()
    assert full[ids].tolist() == got and subtree(store, np.array([bin_])).tolist() == sorted(
        [bin_, *inside, find(store, "$Recycle.Bin/S-1/b")]
    )


def test_hiding_the_bin_takes_it_out_of_the_picture(tmp_path: Path) -> None:
    store = build(
        {"$Recycle.Bin": {"S-1-5-21": {"$RX1.mp4": 700 * MB}}, "Users": {"lenny": {"trip.mp4": 900 * MB}}},
        root="C:\\",
    )
    roots = [RootInfoLike(node=1, whole_volume=True, free=500 * MB, used=1700 * MB)]
    view = View("v1", store, roots, tags=TagStore(tmp_path / "tags.json"))
    view.set_viewport(1200, 800, 1.0)
    assert view.has_trash and view.show_trash
    bin_ = find(store, "$Recycle.Bin")
    assert (view.render().draw.node == bin_).any()
    view.show_trash = False
    frame = view.render()
    assert not (frame.draw.node == bin_).any()
    assert not (frame.flags & F_TRASH).any()
    # unknown space stays what the scan didn't see: the hidden bin is not counted as unknown
    unknown = frame.draw.size[frame.draw.key == KEY_UNKNOWN]
    assert unknown.tolist() == [1700 * MB - int(store.cols["tot_alloc"][1])]
    # zoomed into the bin, it stays drawn
    assert view.zoom_to_node(bin_)
    assert len(view.render().draw)
    plain = View("v2", build({"Users": {"a": 1}}, root="D:\\"), roots, tags=TagStore(tmp_path / "t2.json"))
    assert not plain.has_trash


def test_the_palette_toggle_needs_a_bin(tmp_path: Path) -> None:
    from spaceknife.palette.commands import Context, evaluate, registry

    toggle = next(c for c in registry() if c.id == "view.toggle_trash")
    with_bin = View(
        "v", build({"$Recycle.Bin": {"x": 1}}, root="C:\\"), [], tags=TagStore(tmp_path / "a.json")
    )
    without = View("w", build({"data": {"x": 1}}, root="C:\\"), [], tags=TagStore(tmp_path / "b.json"))
    assert evaluate(toggle, Context(tab="scan", view=with_bin))[0]
    assert evaluate(toggle, Context(tab="scan", view=without)) == (False, "这次扫描里没有回收站")

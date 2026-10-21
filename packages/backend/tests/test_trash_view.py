"""Blocks in a recycle bin / trash carry F_TRASH (view/trash.py): Windows, macOS and Linux bins, by
path, never by name alone."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from spaceknife.store.nodes import NodeStore
from spaceknife.view import TagStore, View
from spaceknife.view.state import F_TRASH, F_TRASH_BIN, RootInfoLike
from spaceknife.view.trash import in_trash, is_trash_path

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

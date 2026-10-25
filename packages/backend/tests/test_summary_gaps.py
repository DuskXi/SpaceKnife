"""tree.summary when a part of it cannot be worked out: the details panel gets the rest and the
reason, and never waits on 「正在读取…」 forever.

A scan of / on Linux (v0.3.3, a plain user, 375,339 items in 0.3 s) left the panel there: every
summary of / failed with 「handler for tree.summary failed」 (an IndexError of the folder class
column, fixed since in pathclass._entry), and one failing part took the whole answer with it.
"""

from __future__ import annotations

import os
from collections.abc import Iterator
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import numpy as np
import pytest
from loguru import logger

from spaceknife import pathclass
from spaceknife.scan.scanner import RootInfo
from spaceknife.server import Server, scans
from spaceknife.server.scans import ScanHandle, summary_of
from spaceknife.store import subtree
from spaceknife.store.nodes import ENTRY_COLUMNS, Kind, NodeStore
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import scan as p
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import SCAN_START, TREE_SUMMARY

from .store_builder import build, ext_of, find
from .test_scan_server import finished, wait_done

TREE: dict[str, Any] = {
    "usr": {
        "share": {"fonts": {"TTF": {"big.ttf": 7000, "small.ttf": 70}}},
        "lib": {"libc.so": 900},
    },
    "var": {"cache": {"pacman": {"pkg": {"linux-6.17.zst": 5000}}}},
    "notes.txt": 30,
}
NO_VIEWS = SimpleNamespace(views=SimpleNamespace(shared=lambda scan_id: None))


@pytest.fixture(autouse=True)
def isolated(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    pathclass.replace_folders([])
    monkeypatch.setattr(scans, "_gaps_logged", set(), raising=False)
    yield
    pathclass.replace_folders([])


def handle_of(store: NodeStore) -> ScanHandle:
    root = next(iter(store.roots()))
    roots = [RootInfo(path=store.path(root), node=root, volume=None, whole_volume=False)]
    return ScanHandle(id="scan-1", store=store, roots=roots, source="scan")


def fold(store: NodeStore, folder: int, children: dict[str, int | None]) -> int:
    """List ``folder``, as a scan reaching it does (None: a folder); the first new node."""
    names = [n.encode() for n in children]
    entries = {col: np.zeros(len(names), dtype=np.int64) for col in ENTRY_COLUMNS}
    for i, size in enumerate(children.values()):
        entries["kind"][i] = Kind.DIR if size is None else Kind.FILE
        entries["size"][i] = entries["alloc"][i] = size or 0
    ext = np.array([store.ext_id(ext_of(n)) for n in children], dtype=np.uint16)
    return store.ingest_names(folder, names, entries, ext)


def column_fails(monkeypatch: pytest.MonkeyPatch) -> None:
    """The folder class column cannot be worked out (as v0.3.3's came out too short)."""

    def broken(store: NodeStore, table: object) -> Any:
        raise IndexError("index 213288 is out of bounds for axis 0 with size 213161")

    monkeypatch.setattr(pathclass, "_entry", broken)


def test_the_last_listing_lands_while_the_first_summary_works_out_the_classes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A scan done in 0.3 s: the details panel's first summary of / works the folder classes out
    while the scan folds its last listing. The next summary walks the grown tree and must cover it
    (v0.3.3: IndexError on every summary of / from then on)."""
    store = build(TREE, root="/", pending=("usr/lib",))
    handle = handle_of(store)
    root = next(iter(store.roots()))
    lib = find(store, "usr/lib")
    detect = pathclass._detect
    landed: list[int] = []

    def fold_meanwhile(s: NodeStore, a: int, b: int) -> pathclass._Found:
        if not landed:  # the scan's thread gets the GIL while the column is worked out
            landed.append(fold(store, lib, {"libc.so": 900, "libLLVM.so": 8000, "python3.12": None}))
        return detect(s, a, b)

    monkeypatch.setattr(pathclass, "_detect", fold_meanwhile)
    first = summary_of(NO_VIEWS, handle, root, 5)
    after = summary_of(NO_VIEWS, handle, root, 5)
    assert landed
    assert not first.gaps and not after.gaps
    assert [f.node.name for f in after.top_files[:2]] == ["libLLVM.so", "big.ttf"]
    assert after.top_files[0].rel_parent == os.path.join("usr", "lib") + os.sep
    assert sum(c.files for c in after.classes) == 6


def test_without_the_folder_classes_the_rest_is_still_given(monkeypatch: pytest.MonkeyPatch) -> None:
    store = build(TREE, root="/")
    root = next(iter(store.roots()))
    column_fails(monkeypatch)
    s = summary_of(NO_VIEWS, handle_of(store), root, 5)
    assert {g.part for g in s.gaps} == {"classes", "folder_class"}
    assert all("IndexError" in g.reason for g in s.gaps)
    assert s.node.id == root and s.path == store.path(root)
    # by extension alone
    assert [(f.node.name, f.class_id) for f in s.top_files[:2]] == [
        ("big.ttf", "other"),
        ("linux-6.17.zst", "archive"),
    ]
    assert sum(c.files for c in s.classes) == 5 and s.latest_mtime >= 0
    assert s.folder_class is None


def test_without_the_walk_the_node_itself_is_still_given(monkeypatch: pytest.MonkeyPatch) -> None:
    store = build(TREE, root="/")
    fonts = find(store, "usr/share/fonts")

    def broken(*args: object, **kwargs: object) -> Any:
        raise RuntimeError("subtree walk below node 4 revisited nodes")

    monkeypatch.setattr(subtree, "summarize", broken)
    s = summary_of(NO_VIEWS, handle_of(store), fonts, 5)
    assert [g.part for g in s.gaps] == ["contents"]
    assert "revisited nodes" in s.gaps[0].reason
    assert s.top_files == [] and s.classes == [] and s.latest_mtime == 0
    assert s.node.name == "fonts" and s.node.alloc == 7070
    assert s.parent is not None and s.parent.name == "share"


def test_each_kind_of_failure_is_logged_once_per_scan(monkeypatch: pytest.MonkeyPatch) -> None:
    """The panel asks about once a second: a failure that stays is one warning, not one a second."""
    store = build(TREE, root="/")
    root = next(iter(store.roots()))
    column_fails(monkeypatch)
    warnings: list[str] = []
    sink = logger.add(lambda m: warnings.append(m.record["message"]), level="WARNING")
    try:
        handle = handle_of(store)
        for _ in range(3):
            assert len(summary_of(NO_VIEWS, handle, root, 5).gaps) == 2
    finally:
        logger.remove(sink)
    assert len(warnings) == 2  # the folder classes, and why the node takes one
    assert all(w.startswith(store.path(root)) and "IndexError" in w for w in warnings)


async def scanned(tmp_path: Path, server: Server, client: Connection) -> tuple[str, int]:
    root = tmp_path / "root"
    (root / "videos").mkdir(parents=True)
    (root / "videos" / "movie.mp4").write_bytes(b"v" * 5000)
    (root / "notes.txt").write_bytes(b"t" * 300)
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    status = await wait_done(client, started.scan_id)
    await finished(server, started.scan_id)
    return started.scan_id, status.roots[0].node


async def test_gaps_reach_the_client(
    tmp_path: Path, server: Server, client: Connection, monkeypatch: pytest.MonkeyPatch
) -> None:
    scan_id, root = await scanned(tmp_path, server, client)
    column_fails(monkeypatch)
    scans._summaries.clear()  # the first look at this scan: nothing cached yet
    res = await client.call(TREE_SUMMARY, p.TreeSummaryParams(scan_id=scan_id, node=root, top=10))
    assert {g.part for g in res.gaps} == {"classes", "folder_class"}
    assert {f.node.name: f.class_id for f in res.top_files} == {"movie.mp4": "video", "notes.txt": "doc"}


async def test_a_node_that_cannot_be_described_says_why(
    tmp_path: Path, server: Server, client: Connection, monkeypatch: pytest.MonkeyPatch
) -> None:
    scan_id, root = await scanned(tmp_path, server, client)

    def broken(store: NodeStore, node: int) -> bool:
        raise OSError("the disk went away")

    monkeypatch.setattr(scans, "on_disk", broken)
    with pytest.raises(RemoteError) as caught:
        await client.call(TREE_SUMMARY, p.TreeSummaryParams(scan_id=scan_id, node=root, top=5))
    assert caught.value.code == ErrorCode.INTERNAL_ERROR
    assert "读不到这一项的详情" in caught.value.message and "the disk went away" in caught.value.message

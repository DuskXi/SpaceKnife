"""compare.* over the real WebSocket: snapshot vs. now, the change tree view, summary, hover card."""

from __future__ import annotations

import asyncio
import os
from pathlib import Path

import numpy as np
import pytest

from spaceknife.diff import compare as diff_trees
from spaceknife.scan.scanner import Scanner
from spaceknife.server.compares import ChangeTree, short_label, subtree_sums
from spaceknife.store.nodes import NodeStore
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_client.views import ViewClient
from spaceknife_protocol import compare as pc
from spaceknife_protocol import scan as p
from spaceknife_protocol import view as pv
from spaceknife_protocol.methods import (
    COMPARE_CLOSE,
    COMPARE_NODE,
    COMPARE_OPEN,
    COMPARE_SOURCES,
    COMPARE_SUMMARY,
    EVENT_COMPARE_PROGRESS,
    SCAN_START,
    VIEW_OPEN,
    VIEW_SET,
)

from .test_scan_server import wait_done
from .tree_fixture import build_tree

BIG = 300_000


async def scanned_store(root: Path) -> NodeStore:
    scanner = Scanner([str(root)])
    await scanner.run()
    return scanner.store


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


def change_files(root: Path) -> None:
    """+BIG new file, a/two.bin deleted (−5000 bytes), a/one.bin grown by 9000 bytes."""
    (root / "a" / "two.bin").unlink()
    (root / "a" / "one.bin").write_bytes(b"y" * 10_000)
    (root / "new").mkdir()
    (root / "new" / "big.iso").write_bytes(b"z" * BIG)


async def scan(client: Connection, root: Path) -> p.ScanStatus:
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    return await wait_done(client, started.scan_id)


async def compared(client: Connection, scan_id: str, base: str, target: str = pc.NOW) -> pc.CompareInfo:
    loop = asyncio.get_running_loop()
    done: asyncio.Future[pc.CompareProgressEvent] = loop.create_future()
    task = await client.call(COMPARE_OPEN, pc.CompareOpenParams(scan_id=scan_id, base=base, target=target))

    def on_event(ev: pc.CompareProgressEvent) -> None:
        if (
            ev.compare_id == task.compare_id
            and ev.phase in ("ready", "failed", "cancelled")
            and not done.done()
        ):
            done.set_result(ev)

    client.on_event(EVENT_COMPARE_PROGRESS, on_event)
    ev = await asyncio.wait_for(done, timeout=60)
    assert ev.phase == "ready", ev.message
    assert ev.info is not None
    return ev.info


async def test_snapshot_against_now(tmp_path: Path, client: Connection) -> None:
    root = tmp_path / "root"
    build_tree(root)
    first = await scan(client, root)
    assert first.snapshot
    change_files(root)
    await asyncio.sleep(1.1)  # snapshot names have a 1 s resolution
    now = await scan(client, root)

    sources = await client.call(COMPARE_SOURCES, pc.CompareSourcesParams(scan_id=now.scan_id))
    assert first.snapshot in [s.path for s in sources.snapshots]
    assert sources.now is not None and sources.now.kind == "now"

    info = await compared(client, now.scan_id, first.snapshot)
    alloc_new = info.grow  # allocation units differ by file system: compare with the scans' own numbers
    assert info.grow > 0 and info.shrink > 0
    assert info.base.kind == "snapshot" and info.target.kind == "now"

    # the change tree as a view: areas are |change|, frames carry delta and dstat
    vc = ViewClient(client)
    update, frame = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=info.compare_id, viewport=pv.Viewport(width=900, height=600))
    )
    assert frame is not None and update.state.compare
    names = frame.names
    assert "many" not in names  # unchanged folders are not in the change tree
    assert {"new", "a"} <= set(names)
    delta, dstat = frame.columns["delta"], frame.columns["dstat"]
    new_row = names.index("new")
    assert delta[new_row] > 0 and dstat[new_row] == pc.DSTAT_NEW
    view_id = update.state.view_id

    summary = await client.call(COMPARE_SUMMARY, pc.CompareSummaryParams(view_id=view_id))
    assert summary.whole and summary.net == summary.grow - summary.shrink
    assert summary.grow == alloc_new
    top = summary.grow_items[0]
    assert top.label == "big.iso" and top.status == "new" and top.on_disk  # one big new file: the file
    assert [i.status for i in summary.shrink_items] == ["deleted"]
    assert summary.shrink_items[0].path.endswith("two.bin") and not summary.shrink_items[0].on_disk

    # hover card of folder a: one grown and one deleted child
    a_key = int(frame.columns["key"][names.index("a")])
    card = await client.call(COMPARE_NODE, pc.CompareNodeParams(view_id=view_id, key=a_key))
    assert card.item.kind == "dir" and {c.status for c in card.children} == {"changed", "deleted"}

    # 「增长」 only: the deleted file leaves the layout
    update, frame = await vc.call(
        VIEW_SET, pv.ViewSetParams(view_id=view_id, settings=pv.ViewSettingsParams(direction="grow"))
    )
    assert update.state.direction == "grow"
    assert frame is not None and "two.bin" not in frame.names
    # a view opened like it (new ends, a split pane) keeps showing growth only
    update, frame = await vc.call(
        VIEW_OPEN,
        pv.ViewOpenParams(
            scan_id=info.compare_id,
            viewport=pv.Viewport(width=900, height=600),
            settings=pv.ViewSettingsParams(direction="grow"),
        ),
    )
    assert update.state.direction == "grow"
    assert frame is not None and "two.bin" not in frame.names

    await client.call(COMPARE_CLOSE, pc.CompareIdParams(compare_id=info.compare_id))
    with pytest.raises(RemoteError):
        await client.call(COMPARE_SUMMARY, pc.CompareSummaryParams(view_id=view_id))


async def test_errors(tmp_path: Path, client: Connection) -> None:
    root = tmp_path / "root"
    build_tree(root)
    st = await scan(client, root)
    other = tmp_path / "other"
    build_tree(other)
    elsewhere = await scan(client, other)
    with pytest.raises(RemoteError):  # both ends the same
        await client.call(COMPARE_OPEN, pc.CompareOpenParams(scan_id=st.scan_id, base="now"))
    # a snapshot of another root fails with a clear message, nothing else changes
    task = await client.call(
        COMPARE_OPEN, pc.CompareOpenParams(scan_id=st.scan_id, base=str(elsewhere.snapshot))
    )
    failed: asyncio.Future[pc.CompareProgressEvent] = asyncio.get_running_loop().create_future()

    def on_event(ev: pc.CompareProgressEvent) -> None:
        if ev.compare_id == task.compare_id and ev.phase == "failed" and not failed.done():
            failed.set_result(ev)

    client.on_event(EVENT_COMPARE_PROGRESS, on_event)
    ev = await asyncio.wait_for(failed, timeout=30)
    assert "扫描位置" in ev.message


async def test_change_tree_keeps_only_changes(tmp_path: Path) -> None:
    root = tmp_path / "root"
    build_tree(root)
    base = await scanned_store(root)
    change_files(root)
    target = await scanned_store(root)
    diff = diff_trees(base, target)
    tree = ChangeTree.of(diff)
    store = tree.store
    names = {store.name(i) for i in range(1, len(store))}
    assert "f000.log" not in names and {"big.iso", "two.bin", "one.bin"} <= names
    # the store's sizes are |change|: a file its own, a folder its own blocks (POSIX directories
    # have some) plus everything below it — together the engine's grow + shrink
    c = store.cols
    n = len(store)
    is_dir = c["kind"][:n] == 1
    area = tree.grow + tree.shrink
    assert np.array_equal(c["alloc"][:n][~is_dir], np.abs(tree.delta)[~is_dir])
    assert np.array_equal((c["tot_alloc"][:n] + c["alloc"][:n])[is_dir], area[is_dir])
    assert np.array_equal(subtree_sums(store, c["alloc"][:n].copy()), area)
    # child ranges are valid and contiguous
    for node in range(n):
        for ch in store.children(node):
            assert int(c["parent"][ch]) == node
    assert int(tree.new_files[0]) == 1


async def test_short_labels(tmp_path: Path) -> None:
    root = tmp_path / "Root"
    for rel in ("WeChat Files/wxid_7k2m9x/FileStorage/Video/v.mp4", "SoftwareDistribution/Download/x.cab"):
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_bytes(b"1")
    store = await scanned_store(root)
    video = store.find(str(root / "WeChat Files" / "wxid_7k2m9x" / "FileStorage" / "Video"))
    download = store.find(str(root / "SoftwareDistribution" / "Download"))
    assert video is not None and download is not None
    top = next(iter(store.roots()))
    sep = os.sep
    assert short_label(store, video, top) == f"WeChat Files{sep}…{sep}Video"
    assert short_label(store, download, top) == f"SoftwareDistribution{sep}Download"

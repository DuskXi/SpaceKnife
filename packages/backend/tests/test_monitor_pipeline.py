"""M5 end to end: a scan is watched; changes on disk reach the tree, which then equals a fresh scan."""

from __future__ import annotations

import asyncio
import inspect
import os
import shutil
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife.scan.scanner import Scanner
from spaceknife.server.monitors import MonitorManager, ScanMonitor
from spaceknife.server.scans import ScanHandle, ScanManager
from spaceknife.server.views import ViewManager
from spaceknife.store import mutate
from spaceknife.store.lock import STRUCTURE
from spaceknife.store.nodes import Flag
from spaceknife_protocol import scan as p

from .test_scan_server import isolated_home
from .test_store_mutate import check_structure, facts
from .tree_fixture import build_tree

__all__ = ["isolated_home"]  # autouse fixture: never touch the real snapshot folder


@pytest.fixture
async def watched() -> Any:
    mgr = ScanManager()
    views = ViewManager(mgr)
    monitors = MonitorManager(mgr, views)
    mgr.monitors = monitors
    try:
        yield mgr, views, monitors
    finally:
        await mgr.close()


async def scanned(mgr: ScanManager, root: Path, workers: int | None = 0) -> ScanHandle:
    handle = await mgr.start(p.ScanStartParams(roots=[str(root)], workers=workers, auto_snapshot=False))
    assert handle.task is not None
    await asyncio.wait_for(asyncio.shield(handle.task), 30)
    assert handle.monitor is not None
    for _ in range(250):  # the watcher's first read is issued on its own thread, folders are fed
        if handle.monitor.status().state in ("running", "degraded") and handle.monitor.fed:
            break
        await asyncio.sleep(0.02)
    await asyncio.sleep(0.05)  # inotify places the watches on its thread
    return handle


async def until(check: Callable[[], Awaitable[bool] | bool], limit: float = 10.0) -> None:
    end = time.monotonic() + limit
    while True:
        got = check()
        ok = await got if inspect.isawaitable(got) else got
        if ok or time.monotonic() >= end:
            break
        await asyncio.sleep(0.1)
    assert ok


def same_as_disk(handle: ScanHandle, root: Path) -> Callable[[], Awaitable[bool]]:
    """The watched tree equals a fresh scan of the disk with the same options."""

    async def check() -> bool:
        fresh = Scanner([str(root)])
        await fresh.run()
        return facts(handle.store) == facts(fresh.store)

    return check


async def test_a_busy_helper_does_not_hold_up_the_scan_start(
    watched: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # the change-journal checkpoint waits for the elevated helper, which may be reading this
    # scan's MFT for half a minute: starting the scan (and the window waiting for it) goes on
    mgr, _, monitors = watched
    release = asyncio.Event()
    asked: list[str] = []

    async def busy_helper(self: Any) -> None:
        asked.append(self.handle.id)
        await release.wait()

    monkeypatch.setattr(ScanMonitor, "start_usn", busy_helper)
    build_tree(tmp_path)
    handle = await asyncio.wait_for(
        mgr.start(p.ScanStartParams(roots=[str(tmp_path)], workers=0, auto_snapshot=False)), 5
    )
    mon = handle.monitor
    assert mon is not None and mon.usn_task is not None and not mon.usn_task.done()
    await asyncio.sleep(0)
    assert asked == [handle.id]
    await monitors.stop(handle.id)  # closing the scan does not wait for the helper either
    assert mon.usn_task is None
    release.set()


async def test_changes_reach_the_tree(watched: Any, tmp_path: Path) -> None:
    mgr, _, _ = watched
    root = build_tree(tmp_path / "root").root
    handle = await scanned(mgr, root)
    deep = max((p for p in root.rglob("*") if p.is_dir()), key=lambda p: len(p.parts))
    (deep / "new.bin").write_bytes(b"n" * 9000)  # created
    grow = next(p for p in root.rglob("*") if p.is_file())
    grow.write_bytes(grow.read_bytes() + b"g" * 100_000)  # modified: bigger
    victim = next(p for p in root.rglob("*") if p.is_file() and p != grow)
    victim.unlink()  # deleted
    (root / "fresh" / "inner").mkdir(parents=True)  # a new folder with content
    (root / "fresh" / "inner" / "x.dat").write_bytes(b"x" * 4321)
    first_dir = sorted(p for p in root.iterdir() if p.is_dir() and p.name != "fresh")[0]
    first_dir.rename(root / "renamed")  # a folder moved with everything in it
    await until(same_as_disk(handle, root))
    check_structure(handle.store)
    status = handle.status().monitor
    assert status is not None and status.changes > 0 and status.events > 0
    assert mutate.gone_count(handle.store) > 0


async def test_a_view_follows_moved_ids(watched: Any, tmp_path: Path) -> None:
    mgr, views, _ = watched
    root = build_tree(tmp_path / "root").root
    handle = await scanned(mgr, root)
    folder = sorted(p for p in root.iterdir() if p.is_dir())[0]
    node = handle.store.find(str(folder))
    assert node is not None
    from spaceknife.view.state import RootInfoLike, View

    view = View("v", handle.store, [RootInfoLike(node=r.node, whole_volume=False) for r in handle.roots])
    view.zoom_to_node(node)
    host = type("Host", (), {})()  # the parts of a ViewHost the change path touches
    host.view, host.scan_id, host.push_task, host.change_pending = view, handle.id, None, False
    views.views["v"] = host
    views._push = lambda *a, **k: asyncio.sleep(0)  # no session here
    (root / "added-at-the-top.txt").write_bytes(b"t" * 10)  # the root is listed again: folder moves
    await until(lambda: handle.store.find(str(folder)) not in (None, node))
    moved = handle.store.find(str(folder))
    assert view.zoom.node == moved and not handle.store.cols["flags"][moved] & np.uint32(Flag.GONE)


async def test_storm_rescans_quietly(watched: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.server import monitors as mon

    monkeypatch.setattr(mon, "STORM_FOLDERS", 3)  # a small storm is enough to switch modes
    monkeypatch.setattr(mon, "QUIET_RESCAN_S", 0.3)
    mgr, _, _ = watched
    root = build_tree(tmp_path / "root").root
    handle = await scanned(mgr, root)
    for i in range(6):
        d = root / f"storm{i}"
        d.mkdir()
        (d / "f.txt").write_bytes(b"s" * (100 + i))
    for gone in [d for d in root.rglob("*") if d.is_dir() and "storm" not in d.name][:3]:
        if gone.exists():
            shutil.rmtree(gone)
    await until(same_as_disk(handle, root), 15)
    check_structure(handle.store)


async def test_switching_monitoring_off(watched: Any, tmp_path: Path) -> None:
    mgr, _, monitors = watched
    root = build_tree(tmp_path / "root").root
    handle = await scanned(mgr, root)

    class Off:
        def get(self, key: str) -> Any:
            return False if key == "scan.monitor" else {"scan.coalesce_ms": 150}.get(key)

    mgr.settings = Off()
    await monitors.apply_settings()
    assert handle.monitor is None and handle.status().monitor is None
    (root / "unseen.txt").write_bytes(b"u" * 5000)
    await asyncio.sleep(0.6)
    assert handle.store.find(str(root / "unseen.txt")) is None


@pytest.mark.skipif(os.name != "nt", reason="the hard scan's Windows allocation estimate")
async def test_sizes_follow_the_hard_scan_rule(watched: Any, tmp_path: Path) -> None:
    mgr, _, _ = watched
    root = build_tree(tmp_path / "root").root
    handle = await scanned(mgr, root)
    f = next(p for p in root.rglob("*") if p.is_file())
    f.write_bytes(b"r" * 600)  # resident in the MFT record: no cluster listed, one by the estimate
    await until(same_as_disk(handle, root))


async def test_concurrent_writers_end_consistent(watched: Any, tmp_path: Path) -> None:
    """Acceptance (PLAN M5): writers keep creating, growing, renaming and deleting files and folders
    while the scan is watched; once they stop, the tree equals a fresh scan (short and light)."""
    import random
    import threading

    mgr, _, _ = watched
    root = build_tree(tmp_path / "root").root
    handle = await scanned(mgr, root)
    stop = threading.Event()

    def writer(seed: int) -> None:
        rnd = random.Random(seed)
        mine = root / f"w{seed}"
        mine.mkdir()
        made: list[Path] = []
        for step in range(150):
            if stop.is_set():
                break
            op = rnd.random()
            if op < 0.4 or not made:
                f = mine / f"f{step}.bin"
                f.write_bytes(b"w" * rnd.randrange(0, 20_000))
                made.append(f)
            elif op < 0.6:
                with open(rnd.choice(made), "ab") as fh:
                    fh.write(b"a" * rnd.randrange(1, 9000))
            elif op < 0.75:
                f = made.pop(rnd.randrange(len(made)))
                f.unlink()
            elif op < 0.9:
                f = made.pop(rnd.randrange(len(made)))
                to = mine / f"r{step}.bin"
                f.rename(to)
                made.append(to)
            else:
                sub = mine / f"d{step}"
                sub.mkdir()
                (sub / "inner.txt").write_bytes(b"i" * rnd.randrange(0, 3000))
            time.sleep(0.002)

    threads = [threading.Thread(target=writer, args=(i,)) for i in range(4)]
    for t in threads:
        t.start()

    def join() -> None:
        for t in threads:
            t.join()

    await asyncio.to_thread(join)
    await until(same_as_disk(handle, root), 20)
    check_structure(handle.store)


@pytest.mark.skipif(os.name != "nt", reason="the USN journal is NTFS on Windows")
async def test_overflow_replays_the_usn_journal(watched: Any, tmp_path: Path) -> None:
    """An overflow on a volume with a journal checkpoint lists just the folders the journal names."""
    from spaceknife import native
    from spaceknife.monitor import Change

    if not native.has("scan_slice"):
        pytest.skip("file ids come from the native lister")
    mgr, _, _ = watched
    root = build_tree(tmp_path / "root").root
    handle = await scanned(mgr, root, workers=None)
    mon = handle.monitor
    assert mon is not None
    target = max((d for d in root.rglob("*") if d.is_dir()), key=lambda d: len(d.parts))
    node = handle.store.find(str(target))
    assert node is not None
    ino = int(handle.store.cols["ino"][node])
    vol = os.path.splitdrive(str(root))[0].upper()
    calls: list[tuple[str, int, int]] = []

    class Journal:
        async def read_usn(self, volume: str, start: int, jid: int, **_: Any) -> dict[str, Any]:
            calls.append((volume, start, jid))
            return {"parent_frn": np.array([ino | (7 << 48)], dtype=np.uint64), "truncated": False}

    async def client() -> Any:
        return Journal()

    mon._usn_client = client  # type: ignore[method-assign]
    mon.usn = {vol: (42, 1000, 2000)}
    for w in [w for _, w in mon.watchers]:  # the watcher "loses" this change
        await asyncio.to_thread(w.stop)
    (target / "lost.bin").write_bytes(b"l" * 7000)
    mon._sink([Change("overflow", str(root), is_dir=True)])
    await until(same_as_disk(handle, root))
    assert calls == [(vol, 1000, 42)]  # read from the safe checkpoint
    assert not mon.trees  # no subtree rescan was needed


async def test_a_stopped_scan_goes_on(watched: Any, tmp_path: Path) -> None:
    mgr, _, monitors = watched
    root = build_tree(tmp_path / "root").root
    mgr.monitors = None  # start() then returns before the scan task ever ran ...
    handle = await mgr.start(p.ScanStartParams(roots=[str(root)], workers=0, auto_snapshot=False))
    mgr.cancel(handle.id)  # ... so it stops with every folder still queued
    mgr.monitors = monitors
    assert handle.task is not None
    await asyncio.wait_for(asyncio.shield(handle.task), 30)
    stopped = handle.status()
    assert stopped.state == "cancelled" and stopped.pending_dirs > 0
    await mgr.continue_scan(handle.id)
    assert handle.task is not None
    await asyncio.wait_for(asyncio.shield(handle.task), 30)
    assert handle.status().state == "done" and handle.status().pending_dirs == 0
    await until(same_as_disk(handle, root))
    with pytest.raises(Exception, match="没有可以继续"):
        await mgr.continue_scan(handle.id)


async def test_rescan_a_folder_without_monitoring(watched: Any, tmp_path: Path) -> None:
    mgr, _, monitors = watched

    class Off:
        def get(self, key: str) -> Any:
            return False if key == "scan.monitor" else {"scan.coalesce_ms": 150}.get(key)

    mgr.settings = Off()  # type: ignore[assignment]
    root = build_tree(tmp_path / "root").root
    handle = await mgr.start(p.ScanStartParams(roots=[str(root)], workers=0, auto_snapshot=False))
    assert handle.task is not None
    await asyncio.wait_for(asyncio.shield(handle.task), 30)
    assert handle.monitor is None
    (root / "later.bin").write_bytes(b"l" * 12345)
    deep = max((d for d in root.rglob("*") if d.is_dir()), key=lambda d: len(d.parts))
    (deep / "also.bin").write_bytes(b"a" * 999)
    await monitors.rescan(handle, 1)  # F5 on the root: the whole tree is read again
    await until(same_as_disk(handle, root))
    assert handle.status().monitor is None  # rescanning did not switch monitoring on


async def test_rescan_the_top_of_several_roots(watched: Any, tmp_path: Path) -> None:
    # F5 at the top of a scan of several roots (node 0, the virtual top): every root is read again
    mgr, _, monitors = watched

    class Off:
        def get(self, key: str) -> Any:
            return False if key == "scan.monitor" else {"scan.coalesce_ms": 150}.get(key)

    mgr.settings = Off()  # type: ignore[assignment]
    roots = [build_tree(tmp_path / name).root for name in ("one", "two")]
    handle = await mgr.start(p.ScanStartParams(roots=[str(r) for r in roots], workers=0, auto_snapshot=False))
    assert handle.task is not None
    await asyncio.wait_for(asyncio.shield(handle.task), 30)
    for i, r in enumerate(roots):
        (r / f"later{i}.bin").write_bytes(b"l" * (1000 + i))

    async def both() -> bool:
        fresh = Scanner([str(r) for r in roots])
        await fresh.run()
        return facts(handle.store) == facts(fresh.store)

    await monitors.rescan(handle, 0)
    await until(both)


async def test_new_folders_are_folded_in_under_the_write_lock(
    watched: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """NodeStore.compact runs on the compute pool as a reader and replaces the columns: the monitor
    adds the rows of new folders only while no reader runs."""
    mgr, _, _ = watched
    root = build_tree(tmp_path / "root").root
    handle = await scanned(mgr, root)
    store = handle.store
    held: list[bool] = []
    real = store.ingest_slice

    def ingest(*args: Any, **kwargs: Any) -> Any:
        held.append(STRUCTURE._writing and STRUCTURE.readers == 0)
        return real(*args, **kwargs)

    monkeypatch.setattr(store, "ingest_slice", ingest)
    (root / "fresh" / "inner" / "deeper").mkdir(parents=True)
    (root / "fresh" / "inner" / "deeper" / "x.dat").write_bytes(b"x" * 4321)
    await until(same_as_disk(handle, root))
    assert held and all(held)

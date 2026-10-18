"""ScanManager robustness: broken worker pools, eviction, auto-save failures, snapshot states."""

from __future__ import annotations

import asyncio
from collections.abc import Iterator
from datetime import UTC, datetime, timedelta
from pathlib import Path
from types import SimpleNamespace
from typing import Any, cast

import pytest
from loguru import logger

from spaceknife.scan.scanner import RootInfo, Scanner, ScanProgress, ScanState
from spaceknife.server import scans as scans_mod
from spaceknife.server.rpc import RpcError
from spaceknife.server.scans import ScanHandle, ScanManager, last_item_count
from spaceknife.server.views import ViewHost, ViewManager
from spaceknife.settings import SettingsStore
from spaceknife.store import snapshot
from spaceknife.store.nodes import NodeStore
from spaceknife_protocol import scan as p
from spaceknife_protocol.errors import ErrorCode

from .test_scan_server import isolated_home
from .tree_fixture import build_tree

__all__ = ["isolated_home"]  # autouse fixture: never touch the real snapshot folder


@pytest.fixture
def events() -> Iterator[list[str]]:
    """Messages of the log-panel events emitted while the test runs."""
    got: list[str] = []

    def sink(message: Any) -> None:
        if "event" in message.record["extra"]:
            got.append(message.record["message"])

    token = logger.add(sink, level="DEBUG")
    try:
        yield got
    finally:
        logger.remove(token)


@pytest.fixture
async def manager() -> Any:
    mgr = ScanManager()
    mgr._pool_size = 2  # cheap to spawn
    mgr._slicer = None  # these tests are about the worker process pool, not the native walker
    try:
        yield mgr
    finally:
        await mgr.close()


async def scan(mgr: ScanManager, root: Path, **kw: Any) -> ScanHandle:
    handle = await mgr.start(p.ScanStartParams(roots=[str(root)], **kw))
    assert handle.task is not None
    await asyncio.wait_for(handle.task, timeout=60)
    return handle


# ---------------------------------------------------------------------------- worker pool


async def test_broken_worker_pool_is_replaced(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = build_tree(tmp_path / "root").root
    warm = await scan(manager, root, workers=2, auto_snapshot=False)
    assert warm.progress().state == ScanState.DONE
    pool = manager._pool
    assert pool is not None

    class KillingScanner(Scanner):
        """Kills the pool's workers right before scanning, like a worker crash would."""

        async def run(self, *args: Any, **kw: Any) -> ScanProgress:
            for proc in list(cast(Any, pool)._processes.values()):
                proc.terminate()
            for _ in range(400):
                if cast(Any, pool)._broken:
                    break
                await asyncio.sleep(0.025)
            return await super().run(*args, **kw)

    monkeypatch.setattr(scans_mod, "Scanner", KillingScanner)
    crashed = await scan(manager, root, workers=2, auto_snapshot=False)
    assert crashed.progress().state == ScanState.FAILED
    assert manager._pool is None  # the dead pool is dropped at once
    monkeypatch.setattr(scans_mod, "Scanner", Scanner)

    again = await scan(manager, root, workers=2, auto_snapshot=False)
    assert again.progress().state == ScanState.DONE
    assert manager._pool is not None and manager._pool is not pool


async def test_pool_found_broken_is_replaced_before_the_next_scan(
    tmp_path: Path, manager: ScanManager
) -> None:
    root = build_tree(tmp_path / "root").root
    await scan(manager, root, workers=2, auto_snapshot=False)
    pool = cast(Any, manager._pool)
    for proc in list(pool._processes.values()):
        proc.terminate()
    for _ in range(400):
        if pool._broken:
            break
        await asyncio.sleep(0.025)
    assert pool._broken
    again = await scan(manager, root, workers=2, auto_snapshot=False)
    assert again.progress().state == ScanState.DONE
    assert manager._pool is not pool


# ---------------------------------------------------------------------------- listeners


async def test_failing_listener_never_fails_the_scan(tmp_path: Path, manager: ScanManager) -> None:
    facts = build_tree(tmp_path / "root")
    handle = await manager.start(p.ScanStartParams(roots=[str(facts.root)], workers=0, auto_snapshot=False))
    seen: list[str] = []

    def broken(_: p.ScanStatus) -> None:
        raise RuntimeError("subscriber bug")

    handle.listeners["a-broken"] = broken
    handle.listeners["b-fine"] = lambda status: seen.append(status.state)
    assert handle.task is not None
    await asyncio.wait_for(handle.task, timeout=60)
    assert handle.progress().state == ScanState.DONE
    assert seen and seen[-1] == "done"


# ---------------------------------------------------------------------------- eviction


def idle_handle(scan_id: str, root: Path, minutes: int) -> ScanHandle:
    store = NodeStore()
    node = store.add_root(str(root))
    finished = datetime(2026, 1, 1, tzinfo=UTC) + timedelta(minutes=minutes)
    progress = ScanProgress(
        state=ScanState.DONE,
        items=1,
        files=0,
        dirs=1,
        bytes_alloc=0,
        bytes_size=0,
        errors=0,
        pending_dirs=0,
        current_path="",
        elapsed_s=0.0,
        rate=0.0,
        progress=1.0,
        started_at=finished,
        finished_at=finished,
    )
    roots = [RootInfo(path=str(root), node=node, volume=None, whole_volume=False)]
    return ScanHandle(id=scan_id, store=store, roots=roots, source="snapshot", saved_progress=progress)


def open_view(views: ViewManager, view_id: str, scan_id: str) -> None:
    views.views[view_id] = ViewHost(
        view=cast(Any, SimpleNamespace(id=view_id)), scan_id=scan_id, session=cast(Any, None)
    )


def test_eviction_skips_scans_with_open_views(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(scans_mod, "MAX_SCANS", 3)
    mgr = ScanManager()
    views = ViewManager(mgr)
    for i in range(3):
        handle = idle_handle(f"scan-{i}", tmp_path, minutes=i)
        mgr.scans[handle.id] = handle
    open_view(views, "view-1", "scan-0")  # the oldest scan is still shown in a tab
    assert views.shared("scan-1") is not None  # tree.summary made shared caches without a view

    mgr._make_room()
    assert set(mgr.scans) == {"scan-0", "scan-2"}
    assert views.shared("scan-0") is not None
    assert "scan-1" not in views._shared  # the evicted scan's caches are gone with it


def test_eviction_refuses_when_every_scan_is_in_use(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(scans_mod, "MAX_SCANS", 2)
    mgr = ScanManager()
    views = ViewManager(mgr)
    for i in range(2):
        handle = idle_handle(f"scan-{i}", tmp_path, minutes=i)
        mgr.scans[handle.id] = handle
        open_view(views, f"view-{i}", handle.id)
    with pytest.raises(RpcError) as info:
        mgr._make_room()
    assert info.value.code == ErrorCode.SCAN_BUSY
    assert set(mgr.scans) == {"scan-0", "scan-1"}
    views.close("view-0")
    mgr._make_room()
    assert set(mgr.scans) == {"scan-1"}


# ---------------------------------------------------------------------------- auto-save


async def test_prune_failure_keeps_the_saved_snapshot(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch, events: list[str]
) -> None:
    def prune(folder: Path, keep_recent: int = 0) -> list[Path]:
        raise PermissionError(13, "in use")

    monkeypatch.setattr(snapshot, "prune", prune)
    handle = await scan(manager, build_tree(tmp_path / "root").root, workers=0)
    assert handle.snapshot_path is not None and Path(handle.snapshot_path).exists()
    assert not any("自动保存快照失败" in m for m in events)


async def test_unexpected_save_error_still_publishes(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch, events: list[str]
) -> None:
    def save(self: ScanManager, handle: ScanHandle, path: Path) -> snapshot.SnapshotHeader:
        raise ValueError("header cannot be encoded")

    monkeypatch.setattr(ScanManager, "_save", save)
    facts = build_tree(tmp_path / "root")
    handle = await manager.start(p.ScanStartParams(roots=[str(facts.root)], workers=0))
    statuses: list[p.ScanStatus] = []
    handle.listeners["test"] = statuses.append
    assert handle.task is not None
    await asyncio.wait_for(handle.task, timeout=60)  # does not raise
    assert handle.snapshot_path is None
    assert statuses and statuses[-1].state == "done"
    assert any("自动保存快照失败：header cannot be encoded" in m for m in events)


def lossy_scanner(extra_errors: int) -> type[Scanner]:
    class Lossy(Scanner):
        """Reports more inaccessible folders than it records (past MAX_RECORDED_DENIED)."""

        async def run(self, *args: Any, **kw: Any) -> ScanProgress:
            self._errors += extra_errors
            return await super().run(*args, **kw)

    return Lossy


@pytest.mark.parametrize("enabled", [True, False])
async def test_unlisted_denied_summary_follows_the_warning_setting(
    tmp_path: Path,
    manager: ScanManager,
    monkeypatch: pytest.MonkeyPatch,
    events: list[str],
    enabled: bool,
) -> None:
    settings = SettingsStore(tmp_path / "settings.json")
    settings.update({"general.log_scan_warnings": enabled})
    manager.settings = settings
    monkeypatch.setattr(scans_mod, "Scanner", lossy_scanner(250))
    handle = await scan(manager, build_tree(tmp_path / "root").root, workers=0, auto_snapshot=False)
    assert handle.progress().errors == 250
    summary = [m for m in events if m.startswith("另有")]
    assert summary == (["另有 250 个文件夹无法访问，已计入未知空间"] if enabled else [])


# ---------------------------------------------------------------------------- snapshot states


def save_snapshot(store: NodeStore, root: Path, state: str, when: datetime) -> Path:
    path = snapshot.auto_path([str(root)], when=when)
    snapshot.save(store, path, roots=[str(root)], stats={"state": state})
    return path


def scanned_store(root: Path) -> NodeStore:
    scanner = Scanner([str(root)])
    asyncio.run(scanner.run())
    return scanner.store


def test_expected_items_ignore_partial_snapshots(tmp_path: Path) -> None:
    root = build_tree(tmp_path / "root").root
    full = scanned_store(root)
    partial = NodeStore()
    partial.add_root(str(root))
    t0 = datetime(2026, 5, 1, 12, 0, tzinfo=UTC)
    save_snapshot(full, root, "done", t0)
    save_snapshot(partial, root, "cancelled", t0 + timedelta(minutes=5))
    assert last_item_count([str(root)]) == len(full) - 1
    save_snapshot(partial, root, "done", t0 + timedelta(minutes=10))
    assert last_item_count([str(root)]) == len(partial) - 1


def test_expected_items_none_without_complete_snapshot(tmp_path: Path) -> None:
    root = tmp_path / "r"
    root.mkdir()
    store = NodeStore()
    store.add_root(str(root))
    save_snapshot(store, root, "failed", datetime(2026, 5, 1, tzinfo=UTC))
    assert last_item_count([str(root)]) is None


@pytest.mark.parametrize(
    ("state", "progress"), [("done", 1.0), ("cancelled", None), ("failed", None), (None, 1.0)]
)
async def test_loaded_snapshot_keeps_its_state(
    tmp_path: Path, state: str | None, progress: float | None
) -> None:
    root = tmp_path / "r"
    root.mkdir()
    store = NodeStore()
    store.add_root(str(root))
    path = tmp_path / "s.sks"
    snapshot.save(store, path, roots=[str(root)], stats={"state": state} if state else {})
    mgr = ScanManager()
    status = (await mgr.load_snapshot(str(path))).status()
    assert (status.state, status.progress) == (state or "done", progress)

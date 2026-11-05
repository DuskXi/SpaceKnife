"""M4 in the scan manager: disk-load governor, pause / resume, load changes, MFT path and fallback."""

from __future__ import annotations

import asyncio
import os
import sys
import threading
import time
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest
from loguru import logger

from spaceknife.helper import HelperDenied
from spaceknife.io import IoRegistry
from spaceknife.io.sampler import DiskSample, DiskSampler
from spaceknife.scan import mft, worker
from spaceknife.scan.scanner import ScanState
from spaceknife.server.scans import ScanHandle, ScanManager, mft_duty, on_disk, snapshot_info
from spaceknife.store import snapshot
from spaceknife_protocol import scan as p

from . import mft_image as im
from .test_scan_server import isolated_home
from .tree_fixture import build_tree

__all__ = ["isolated_home"]


class FakeSampler(DiskSampler):
    """Every path is on one sampled disk that is idle except for us (deterministic, no OS calls)."""

    name = "fake"
    supported = True
    min_interval = 0.0

    def __init__(self) -> None:
        self.calls = 0

    def refs(self) -> list[Any]:
        return []

    def ref(self, disk: Any) -> Any:
        return None

    def watch(self, ref: Any) -> None:
        pass

    def sample(self) -> dict[Any, DiskSample]:
        self.calls += 1
        return {}

    def close(self) -> None:
        pass


@pytest.fixture
def events() -> Iterator[list[str]]:
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
    mgr.io = IoRegistry(sampler_factory=FakeSampler, max_workers=4, idle_interval=0.05, active_interval=0.05)
    await mgr.start_io()
    try:
        yield mgr
    finally:
        await mgr.close()


async def finished(handle: ScanHandle, limit: float = 30) -> ScanHandle:
    assert handle.task is not None
    await asyncio.wait_for(asyncio.shield(handle.task), limit)
    return handle


# ------------------------------------------------------------------ governor, pause, load


async def test_hard_scans_run_under_the_disk_governor(tmp_path: Path, manager: ScanManager) -> None:
    facts = build_tree(tmp_path / "root")
    handle = await manager.start(
        p.ScanStartParams(roots=[str(facts.root)], load_level="quiet", auto_snapshot=False)
    )
    assert handle.governor is not None and handle.governor.level == "quiet"
    running = handle.status()
    assert running.method == "hard" and running.io is not None and running.io.level == "quiet"
    await finished(handle)
    done = handle.status()
    assert done.state == "done" and done.files == facts.files and done.io is None


async def test_pause_holds_dispatch_and_resume_finishes(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch
) -> None:
    facts = build_tree(tmp_path / "root")
    calls: list[float] = []
    gate = threading.Event()

    def slow(tasks: Any, opts: Any) -> Any:
        calls.append(time.monotonic())
        gate.wait(0.2)
        return worker.scan_slice(tasks, opts)

    monkeypatch.setattr(manager, "_slicer", slow)
    handle = await manager.start(
        p.ScanStartParams(roots=[str(facts.root)], load_level="max", auto_snapshot=False, workers=1)
    )
    assert handle.scanner is not None
    manager.pause(handle.id)
    assert handle.status().paused
    gate.set()
    await asyncio.sleep(0.3)
    held = len(calls)
    await asyncio.sleep(0.3)
    assert len(calls) == held, "no new batch while paused"
    assert handle.status().state == "scanning"
    manager.resume(handle.id)
    await finished(handle)
    assert handle.status().state == "done" and handle.status().files == facts.files
    assert not handle.status().paused


async def test_the_load_level_changes_while_scanning(tmp_path: Path, manager: ScanManager) -> None:
    facts = build_tree(tmp_path / "root")
    handle = await manager.start(
        p.ScanStartParams(roots=[str(facts.root)], load_level="balanced", auto_snapshot=False)
    )
    manager.set_load(p.ScanSetLoadParams(scan_id=handle.id, level="custom", custom=40, concurrency=2))
    gov = handle.governor
    assert gov is not None and gov.level == "custom" and abs(gov.limit - 40) < 1e-6
    assert gov.concurrency <= 2
    await finished(handle)


def test_mft_duty_follows_the_level() -> None:
    class G:
        def __init__(self, level: str, limit: float) -> None:
            self.level, self.limit = level, limit

    assert mft_duty(None) == 1.0
    assert mft_duty(G("max", 100)) == 1.0  # type: ignore[arg-type]
    assert mft_duty(G("quiet", 35)) == pytest.approx(0.35)  # type: ignore[arg-type]
    assert mft_duty(G("idle", 100)) == 0.2  # type: ignore[arg-type]


# ------------------------------------------------------------------ MFT path


class FakeClient:
    alive = True
    elevated = True

    def __init__(self, table: dict[str, Any] | None = None, hang: bool = False) -> None:
        self._table = table
        self._hang = hang
        self.last_scan: dict[str, Any] = {"record_size": 1024, "cluster_size": 4096}
        self.duty: float | None = None

    async def scan_mft(self, volume: str, *, duty: float = 1.0, on_progress: Any = None) -> dict[str, Any]:
        self.duty = duty
        if on_progress is not None:
            on_progress("scan", 0.1, 0.5)
        if self._hang:
            await asyncio.sleep(3600)
        assert self._table is not None
        return self._table


class FakeShared:
    def __init__(self, client: FakeClient | None = None, error: Exception | None = None) -> None:
        self.client, self.error = client, error
        self.current = client
        self.asked: list[bool] = []

    async def get(self, elevate: bool = True) -> FakeClient:
        self.asked.append(elevate)
        if self.error is not None:
            raise self.error
        assert self.client is not None
        return self.client

    async def close(self) -> None:
        pass


def _sample_table() -> dict[str, Any]:
    data = im.build_mft(im.sample_records())
    return mft.parse_mft_table(data, 1024, 4096)


async def test_mft_scan_adopts_the_volume_tree(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch, events: list[str]
) -> None:
    root = tmp_path / "vol"
    root.mkdir()
    client = FakeClient(_sample_table())
    manager.helper = FakeShared(client)  # type: ignore[assignment]
    monkeypatch.setattr(manager, "_mft_wanted", lambda handle: True)
    handle = await manager.start(p.ScanStartParams(roots=[str(root)], method="mft", load_level="quiet"))
    await finished(handle)
    status = handle.status()
    assert status.state == "done" and status.method == "mft" and status.fallback == ""
    assert status.items > 40 and handle.store.name(1) == str(root)
    store = handle.store
    meta = store.find(os.path.join(str(root), mft.META_FOLDER))
    mft_file = store.find(os.path.join(str(root), mft.META_FOLDER, "$MFT"))
    assert meta is not None and mft_file is not None
    assert on_disk(store, 1) and not on_disk(store, meta) and not on_disk(store, mft_file)
    assert client.duty == pytest.approx(0.35)
    assert any("MFT 快速扫描完成" in e for e in events)
    assert status.snapshot is not None  # saved like any finished scan
    # the snapshot remembers how the tree was read, also through a reload and a second save
    saved = Path(status.snapshot)
    assert snapshot_info(saved, snapshot.read_header(saved)).method == "mft"
    reopened = await manager.load_snapshot(str(saved))
    assert reopened.status().method == "snapshot" and reopened.method == "mft"
    again = await manager.save_snapshot(reopened.id, str(tmp_path / "again.sks"))
    assert again.method == "mft"


async def test_mft_falls_back_to_a_hard_scan(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch, events: list[str]
) -> None:
    facts = build_tree(tmp_path / "root")
    manager.helper = FakeShared(error=HelperDenied())  # type: ignore[assignment]
    monkeypatch.setattr(manager, "_mft_wanted", lambda handle: True)
    handle = await manager.start(
        p.ScanStartParams(roots=[str(facts.root)], method="mft", auto_snapshot=False)
    )
    await finished(handle)
    status = handle.status()
    assert status.state == "done" and status.method == "hard" and status.files == facts.files
    assert "用户拒绝了管理员授权" in status.fallback
    assert any("改用硬扫描" in e for e in events)


async def test_an_mft_read_can_be_cancelled(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "vol"
    root.mkdir()
    manager.helper = FakeShared(FakeClient(hang=True))  # type: ignore[assignment]
    monkeypatch.setattr(manager, "_mft_wanted", lambda handle: True)
    handle = await manager.start(p.ScanStartParams(roots=[str(root)], method="mft", auto_snapshot=False))
    for _ in range(100):
        if handle.mft_task is not None and handle.mft_read is not None:
            break
        await asyncio.sleep(0.01)
    # half of the $MFT read: the share of the read phase, with time left once time has passed
    status = handle.status()
    assert status.phase == "mft" and status.progress == pytest.approx(0.5 * 0.85)
    handle.mft_elapsed = 4.0
    assert handle.status().remaining_s == pytest.approx(4.0 * (1 - 0.425) / 0.425, rel=0.05)
    manager.cancel(handle.id)
    await finished(handle, limit=5)
    assert handle.status().state == "cancelled" and handle.status().method == "mft"
    assert handle.mft_read is None


def test_mft_is_chosen_for_whole_ntfs_volumes(
    manager: ScanManager, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    from spaceknife.scan.scanner import RootInfo
    from spaceknife.scan.volume import VolumeInfo
    from spaceknife.server import scans as scans_mod
    from spaceknife.settings import SettingsStore

    if not sys.platform.startswith("win"):
        pytest.skip("MFT scans are Windows-only")
    vol = VolumeInfo(
        mount_point="C:\\",
        device="C:",
        fs_type="NTFS",
        label="",
        total=1,
        free=0,
        cluster_size=4096,
        is_remote=False,
        is_removable=False,
    )
    handle = ScanHandle(
        id="s", store=None, roots=[RootInfo("C:\\", 1, vol, True)], source="scan", wanted="auto"
    )  # type: ignore[arg-type]
    monkeypatch.setattr(scans_mod, "is_elevated", lambda: False)
    manager.helper = FakeShared()  # type: ignore[assignment]
    manager.helper.current = None  # type: ignore[attr-defined]
    assert manager._mft_wanted(handle)  # auto: MFT, asking for administrator rights once (W1-18/34)
    settings = SettingsStore(tmp_path / "settings.json")
    settings.update({"scan.elevate": False})
    manager.settings = settings
    assert not manager._mft_wanted(handle)  # may not ask, and would have to
    monkeypatch.setattr(scans_mod, "is_elevated", lambda: True)
    assert manager._mft_wanted(handle) and manager.mft_ready()  # no prompt needed
    handle.wanted = "hard"
    assert not manager._mft_wanted(handle)
    handle.wanted = "auto"
    handle.roots[0].whole_volume = False
    assert not manager._mft_wanted(handle)


async def test_scan_stays_a_scan_after_mft_states(tmp_path: Path, manager: ScanManager) -> None:
    """A plain hard scan reports method hard and an empty phase (the protocol defaults)."""
    facts = build_tree(tmp_path / "root")
    handle = await manager.start(p.ScanStartParams(roots=[str(facts.root)], auto_snapshot=False, workers=0))
    await finished(handle)
    s = handle.status()
    assert s.method == "hard" and s.phase == "" and handle.scanner is not None
    assert handle.scanner.state == ScanState.DONE


# ------------------------------------------------------------------ over the protocol


async def test_io_methods_and_topic_over_the_protocol(tmp_path: Path, client: Any) -> None:
    from spaceknife_protocol import io as pio
    from spaceknife_protocol.base import Empty
    from spaceknife_protocol.methods import (
        EVENT_IO_LOAD,
        IO_STATUS,
        SCAN_PAUSE,
        SCAN_RESUME,
        SCAN_SET_LOAD,
        SCAN_START,
    )

    from .test_scan_server import wait_done

    listed = await client.call(IO_STATUS, Empty())
    assert isinstance(listed.disks, list)
    got: list[pio.IoStatusEvent] = []
    client.on_event(EVENT_IO_LOAD, got.append)
    await client.subscribe(pio.TOPIC_IO, pio.IoTopicParams())
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], load_level="balanced"))
    paused = await client.call(SCAN_PAUSE, p.ScanIdParams(scan_id=started.scan_id))
    assert paused.scan_id == started.scan_id
    changed = await client.call(SCAN_SET_LOAD, p.ScanSetLoadParams(scan_id=started.scan_id, level="quiet"))
    assert changed.io is None or changed.io.level == "quiet"
    await client.call(SCAN_RESUME, p.ScanIdParams(scan_id=started.scan_id))
    done = await wait_done(client, started.scan_id)
    assert done.state == "done" and done.files == facts.files and not done.paused
    if listed.disks:  # a machine with sampled disks pushes their load
        for _ in range(40):
            if got:
                break
            await asyncio.sleep(0.05)
        assert got and got[0].status.disk


# ------------------------------------------------------------------ switching a hard scan to MFT (W2-24/25)


def _slow_slicer(delay: float) -> Any:
    def slow(tasks: Any, opts: Any) -> Any:
        time.sleep(delay)
        return worker.scan_slice(tasks, opts)

    return slow


async def test_use_mft_takes_over_a_running_hard_scan(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch
) -> None:
    facts = build_tree(tmp_path / "root")
    monkeypatch.setattr(manager, "_slicer", _slow_slicer(0.05))
    monkeypatch.setattr(ScanManager, "_mft_eligible", staticmethod(lambda handle: True))
    manager.helper = FakeShared(FakeClient(_sample_table()))  # type: ignore[assignment]
    handle = await manager.start(
        p.ScanStartParams(roots=[str(facts.root)], method="hard", workers=1, auto_snapshot=False)
    )
    assert handle.status().mft_available and handle.status().method == "hard"
    manager.use_mft(handle.id)
    await finished(handle)
    status = handle.status()
    assert status.state == "done" and status.method == "mft" and status.fallback == ""
    assert status.items > 40  # the sample $MFT's tree, not the hard scan's
    assert not status.paused and status.phase == ""


async def test_use_mft_refused_keeps_the_hard_scan_going(
    tmp_path: Path, manager: ScanManager, monkeypatch: pytest.MonkeyPatch, events: list[str]
) -> None:
    facts = build_tree(tmp_path / "root")
    monkeypatch.setattr(manager, "_slicer", _slow_slicer(0.05))
    monkeypatch.setattr(ScanManager, "_mft_eligible", staticmethod(lambda handle: True))
    manager.helper = FakeShared(error=HelperDenied())  # type: ignore[assignment]
    handle = await manager.start(
        p.ScanStartParams(roots=[str(facts.root)], method="hard", workers=1, auto_snapshot=False)
    )
    manager.use_mft(handle.id)
    await finished(handle)
    status = handle.status()
    assert status.state == "done" and status.method == "hard" and status.files == facts.files
    assert "用户拒绝了管理员授权" in status.fallback and status.mft_available
    assert any("继续硬扫描" in e for e in events)


async def test_use_mft_needs_a_whole_ntfs_volume(tmp_path: Path, manager: ScanManager) -> None:
    from spaceknife.server.rpc import RpcError

    facts = build_tree(tmp_path / "root")
    handle = await manager.start(p.ScanStartParams(roots=[str(facts.root)], auto_snapshot=False))
    assert not handle.status().mft_available
    with pytest.raises(RpcError):
        manager.use_mft(handle.id)
    await finished(handle)

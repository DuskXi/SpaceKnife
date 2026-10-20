from __future__ import annotations

import asyncio
import os
import subprocess
import sys
from pathlib import Path

import pytest
import typer

from spaceknife.cli_scan import load_limit
from spaceknife.server import Server
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EVENT_SCAN_PROGRESS,
    SCAN_CLOSE,
    SCAN_START,
    SCAN_STATUS,
    SNAPSHOT_LIST,
    SNAPSHOT_LOAD,
    TREE_CHILDREN,
    TREE_FIND,
    TREE_NODE,
    VOLUME_LIST,
)

from .tree_fixture import build_tree


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


async def wait_done(client: Connection, scan_id: str) -> p.ScanStatus:
    done: asyncio.Future[p.ScanStatus] = asyncio.get_running_loop().create_future()

    def on_event(ev: p.ScanProgressEvent) -> None:
        if (
            ev.status.scan_id == scan_id
            and ev.status.state != "scanning"
            and ev.status.snapshot
            and not done.done()
        ):
            done.set_result(ev.status)

    client.on_event(EVENT_SCAN_PROGRESS, on_event)
    await client.subscribe(p.TOPIC_SCAN, p.ScanTopicParams(scan_id=scan_id))
    return await asyncio.wait_for(done, timeout=60)


@pytest.mark.parametrize("workers", [0, 2])
async def test_scan_over_the_protocol(tmp_path: Path, client: Connection, workers: int) -> None:
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=workers))
    assert started.state in ("queued", "scanning", "done")
    status = await wait_done(client, started.scan_id)
    assert status.state == "done"
    assert (status.files, status.dirs - 1, status.bytes_size) == (facts.files, facts.dirs, facts.size)
    assert status.snapshot is not None and Path(status.snapshot).exists()

    root = status.roots[0].node
    children = await client.call(
        TREE_CHILDREN, p.TreeChildrenParams(scan_id=started.scan_id, node=root, limit=3)
    )
    assert children.total == len({rel.split("/")[0] for rel in [*facts_tops(facts)]})
    allocs = [c.alloc for c in children.items]
    assert allocs == sorted(allocs, reverse=True)
    assert len(children.items) == 3

    found = await client.call(
        TREE_FIND, p.TreeFindParams(scan_id=started.scan_id, path=str(facts.root / "a" / "sub" / "movie.mp4"))
    )
    assert found.node is not None and found.node.ext == "mp4" and found.node.size == 123456
    node = await client.call(TREE_NODE, p.TreeNodeParams(scan_id=started.scan_id, node=found.node.id))
    assert Path(node.path) == facts.root / "a" / "sub" / "movie.mp4"


def facts_tops(facts: object) -> list[str]:
    from .tree_fixture import EXTRA_DIRS, FILES

    return [*FILES, *EXTRA_DIRS]


async def test_snapshot_list_and_load(tmp_path: Path, client: Connection) -> None:
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=0))
    status = await wait_done(client, started.scan_id)
    listed = await client.call(SNAPSHOT_LIST, Empty())
    assert [s.path for s in listed.snapshots] == [status.snapshot]
    assert listed.snapshots[0].files == facts.files
    loaded = await client.call(SNAPSHOT_LOAD, p.SnapshotLoadParams(path=status.snapshot or ""))
    assert loaded.source == "snapshot"
    assert loaded.scan_id != started.scan_id
    assert (loaded.files, loaded.bytes_size) == (facts.files, facts.size)
    # the next scan of the same folder knows how many items to expect (UI-W2-10, UI-W2-15)
    assert status.expected_items is None and status.progress is None and status.remaining_s is None
    again = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=0))
    assert again.expected_items == status.items
    done = await wait_done(client, again.scan_id)
    assert done.progress == 1.0 and done.rate > 0


async def test_close_and_errors(tmp_path: Path, client: Connection) -> None:
    facts = build_tree(tmp_path / "root")
    started = await client.call(
        SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=0, auto_snapshot=False)
    )
    await client.call(SCAN_CLOSE, p.ScanIdParams(scan_id=started.scan_id))
    with pytest.raises(RemoteError) as info:
        await client.call(SCAN_STATUS, p.ScanIdParams(scan_id=started.scan_id))
    assert info.value.code == ErrorCode.UNKNOWN_SCAN
    with pytest.raises(RemoteError) as info:
        await client.call(SCAN_START, p.ScanStartParams(roots=[str(tmp_path / "missing")]))
    assert info.value.code == ErrorCode.SCAN_FAILED
    with pytest.raises(RemoteError) as info:
        await client.call(SNAPSHOT_LOAD, p.SnapshotLoadParams(path=str(tmp_path / "nope.sks")))
    assert info.value.code == ErrorCode.SNAPSHOT_ERROR


async def test_unknown_node(tmp_path: Path, server: Server, client: Connection) -> None:
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=0))
    with pytest.raises(RemoteError) as info:
        await client.call(TREE_NODE, p.TreeNodeParams(scan_id=started.scan_id, node=10**9))
    assert info.value.code == ErrorCode.UNKNOWN_NODE


async def test_volume_list(client: Connection) -> None:
    vols = await client.call(VOLUME_LIST, Empty())
    assert vols.volumes
    # unavailable drives (no media, disconnected) are listed too, with a reason instead of a size (UI-W1-22)
    assert all(v.total > 0 or v.reason for v in vols.volumes)
    assert any(v.total > 0 and not v.reason for v in vols.volumes)


def test_cli_scan_and_snapshot_commands(tmp_path: Path) -> None:
    facts = build_tree(tmp_path / "root")
    env = {**os.environ, "SPACEKNIFE_HOME": str(tmp_path / "home"), "COLUMNS": "120"}
    run = lambda *args: subprocess.run(  # noqa: E731
        [sys.executable, "-X", "utf8", "-m", "spaceknife", *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=env,
        timeout=120,
    )
    scan = run("scan", str(facts.root), "--workers", "0", "--top", "3")
    assert scan.returncode == 0, scan.stderr
    assert "扫描结果" in scan.stdout and "最大的文件" in scan.stdout and "movie.mp4" in scan.stdout
    profiled = run("scan", str(facts.root), "--top", "3", "--profile", "--no-snapshot")  # as in the app
    assert profiled.returncode == 0, profiled.stderr
    assert "耗时分解" in profiled.stdout and "列目录线程平均忙碌" in profiled.stdout
    limited = run(
        "scan", str(facts.root), "--top", "3", "--profile", "--no-snapshot", "--disk-limit", "quiet"
    )
    assert limited.returncode == 0, limited.stderr
    assert "等磁盘负载上限放行" in limited.stdout
    assert run("scan", str(facts.root), "--disk-limit", "fast").returncode == 2
    listing = run("snapshot", "list")
    assert listing.returncode == 0
    assert "共 1 个快照" in listing.stdout
    snaps = list((tmp_path / "home").rglob("*.sks"))
    assert len(snaps) == 1
    show = run("snapshot", "show", str(snaps[0]))
    assert show.returncode == 0 and "最大的项目" in show.stdout
    bad = run("scan", str(tmp_path / "missing"))
    assert bad.returncode == 2


def test_cli_disk_limit_values() -> None:
    assert load_limit("balanced") == ("balanced", None)
    assert load_limit(" Quiet ") == ("quiet", None)
    assert load_limit("45%") == ("custom", 45.0)
    for bad in ("0", "101", "fast", "", "-5"):
        with pytest.raises(typer.BadParameter):
            load_limit(bad)


async def test_scan_events_for_the_log_panel(tmp_path: Path, server: Server, client: Connection) -> None:
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=0))
    await wait_done(client, started.scan_id)
    events = [e for e in server.log_hub.recent(500) if e.extra.get("event")]
    texts = [
        (e.extra["event"], e.level, e.message) for e in events if e.extra.get("scan_id") == started.scan_id
    ]
    assert texts[0][0] == "scan" and texts[0][2].startswith("开始扫描 ")
    assert texts[-1][0] == "scan" and texts[-1][2].startswith("硬扫描完成：")
    assert events[-1].extra["path"] == str(facts.root)


@pytest.mark.skipif(sys.platform == "win32" or os.geteuid() == 0, reason="POSIX permissions (and not root)")
async def test_denied_folders_become_warnings(tmp_path: Path, server: Server, client: Connection) -> None:
    facts = build_tree(tmp_path / "root")
    locked = facts.root / "locked"
    locked.mkdir()
    (locked / "x.bin").write_bytes(b"x")
    locked.chmod(0)
    try:
        started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=0))
        status = await wait_done(client, started.scan_id)
    finally:
        locked.chmod(0o755)
    assert status.errors == 1 and status.denied == [str(locked)]
    warnings = [e for e in server.log_hub.recent(500) if e.extra.get("event") == "perm"]
    assert warnings and warnings[-1].level == "WARNING" and warnings[-1].extra["path"] == str(locked)
    assert warnings[-1].message.startswith(f"无法访问 {locked}：") and warnings[-1].message.endswith(
        "已计入未知空间"
    )

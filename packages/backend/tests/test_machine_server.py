"""``machine.disks`` (the 「本机磁盘」 page): the live answer, and what scans and snapshots tell."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from spaceknife.disks import Probe
from spaceknife.scan.volume import VolumeInfo
from spaceknife.server import Server, machine_handlers
from spaceknife_client.connection import Connection
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty
from spaceknife_protocol.machine import DiskPart, PhysicalDisk
from spaceknife_protocol.methods import MACHINE_DISKS, SCAN_START, VOLUME_LIST

from .test_scan_server import wait_done
from .tree_fixture import build_tree


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


async def test_this_machine_over_the_protocol(client: Connection) -> None:
    volumes = (await client.call(VOLUME_LIST, Empty())).volumes
    res = await client.call(MACHINE_DISKS, Empty())
    assert [v.volume.mount_point for v in res.volumes] == [v.mount_point for v in volumes]
    ids = {d.id for d in res.disks}
    mounts = {v.mount_point for v in volumes}
    assert all(set(v.disks) <= ids for v in res.volumes)
    assert all(part.mount_point in mounts for d in res.disks for part in d.parts)
    assert res.collected_at.tzinfo is not None


async def test_what_a_scan_and_a_snapshot_tell(
    tmp_path: Path, client: Connection, server: Server, monkeypatch: pytest.MonkeyPatch
) -> None:
    facts = build_tree(tmp_path / "root")
    root = str(facts.root)
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[root]))
    status = await wait_done(client, started.scan_id)
    # let the scanned folder stand in for a whole volume
    key = os.path.normcase(os.path.normpath(root))
    monkeypatch.setattr(machine_handlers, "_whole_root", lambda h: key if h.id == started.scan_id else None)
    volume = VolumeInfo(root, "", "NTFS", "", 10**9, 10**8, 4096, False, False)
    found = Probe(
        disks=[PhysicalDisk(id="d0", model="Disk", size=10**9)], parts={root: [("d0", 10**9)]}, offsets={}
    )

    out = machine_handlers.overview(server, [volume], found, {})
    assert out.disks[0].parts == [DiskPart(mount_point=root, bytes=10**9)]
    v = out.volumes[0]
    assert v.disks == ["d0"] and v.scan is not None
    scan = v.scan
    assert (scan.source, scan.scan_id, scan.running) == ("scan", started.scan_id, False)
    assert (scan.files, scan.bytes_alloc) == (status.files, status.bytes_alloc)
    assert scan.at == status.finished_at and scan.method == "hard"
    assert 0 < len(scan.classes) <= machine_handlers.TOP_CLASSES
    assert [c.bytes for c in scan.classes] == sorted((c.bytes for c in scan.classes), reverse=True)
    assert 0 < len(scan.folders) <= machine_handlers.TOP_FOLDERS
    assert all((facts.root / f.name).is_dir() for f in scan.folders)
    assert [f.bytes for f in scan.folders] == sorted((f.bytes for f in scan.folders), reverse=True)

    # without the open scan: the newest snapshot (its header only)
    saved = machine_handlers.newest_snapshot(root)
    assert saved is not None and saved.source == "snapshot" and saved.snapshot == status.snapshot
    assert (saved.files, saved.bytes_alloc, saved.method) == (status.files, status.bytes_alloc, "hard")
    assert saved.classes == [] and saved.folders == []
    monkeypatch.setattr(machine_handlers, "_whole_root", lambda h: None)
    out = machine_handlers.overview(server, [volume], found, {root: saved})
    assert out.volumes[0].scan == saved
    assert machine_handlers.newest_snapshot(str(tmp_path / "never scanned")) is None

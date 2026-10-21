"""``machine.disks`` (the 「本机磁盘」 page): the live answer, and what scans and snapshots tell."""

from __future__ import annotations

import os
from datetime import UTC, datetime
from pathlib import Path

import pytest

from spaceknife.disks import Probe
from spaceknife.helper import HelperDenied, HelperError, HelperUnavailable
from spaceknife.scan.volume import VolumeInfo
from spaceknife.server import Server, machine_handlers
from spaceknife.server.volume_summaries import Kept, VolumeSummaries, identity
from spaceknife_client.connection import Connection
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty
from spaceknife_protocol.machine import DiskHealth, DiskPart, PhysicalDisk, ReadHealthParams
from spaceknife_protocol.methods import (
    MACHINE_DISKS,
    MACHINE_READ_HEALTH,
    SCAN_CLOSE,
    SCAN_START,
    VOLUME_LIST,
)
from spaceknife_protocol.scan import ClassStat

from .test_disks import nvme_log
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

    # without the open scan: the newest snapshot (its header only), and the history of the volume
    saved, history = machine_handlers.saved_scans(root)
    assert saved is not None and saved.source == "snapshot" and saved.snapshot == status.snapshot
    assert (saved.files, saved.bytes_alloc, saved.method) == (status.files, status.bytes_alloc, "hard")
    assert saved.classes == [] and saved.folders == []
    assert [(h.files, h.bytes_alloc) for h in history] == [(status.files, status.bytes_alloc)]
    monkeypatch.setattr(machine_handlers, "_whole_root", lambda h: None)
    out = machine_handlers.overview(server, [volume], found, {root: (saved, history)})
    assert out.volumes[0].scan == saved and out.volumes[0].history == history
    assert machine_handlers.saved_scans(str(tmp_path / "never scanned")) == (None, [])

    # a second complete scan: the history grows, newest first (the first file renamed to an older name:
    # snapshot files are named by the second, and both scans finish within one)
    first = Path(status.snapshot or "")
    first.rename(first.with_name("20000101-000000.sks"))
    (facts.root / "grown.bin").write_bytes(b"x" * 300_000)
    again = await client.call(SCAN_START, p.ScanStartParams(roots=[root]))
    second = await wait_done(client, again.scan_id)
    _saved, history = machine_handlers.saved_scans(root)
    assert [h.bytes_alloc for h in history] == [second.bytes_alloc, status.bytes_alloc]
    assert history[0].at >= history[1].at and second.bytes_alloc > status.bytes_alloc


# ---------------------------------------------------------------- records that need administrator rights


class FakeClient:
    """An elevated helper: the NVMe drive answers, the SATA disk refuses."""

    elevated = True
    alive = True

    def __init__(self) -> None:
        self.asked: list[str] = []

    async def disk_health(self, disk: str) -> dict[str, object]:
        self.asked.append(disk)
        if disk == "sda":
            raise HelperError("磁盘没有设备统计日志", "not_supported")
        log = nvme_log(units_written=1_000_000, hours=8070, kelvin=330)
        return {"kind": "nvme", "log": log.hex()}


class FakeShared:
    def __init__(self, fail: Exception | None = None) -> None:
        self.fail = fail
        self.client = FakeClient()
        self.prompts = 0
        self.running = False

    @property
    def current(self) -> FakeClient | None:
        return self.client if self.running else None

    async def get(self, elevate: bool = True) -> FakeClient:
        assert elevate
        self.prompts += 1
        if self.fail is not None:
            raise self.fail
        self.running = True
        return self.client

    async def close(self) -> None:
        self.running = False


def linux_disks() -> list[PhysicalDisk]:
    return [
        PhysicalDisk(
            id="nvme0n1",
            model="ZHITAI TiPro9000",
            serial="S1",
            size=4 * 10**12,
            media="nvme_ssd",
            interface="nvme",
            health=DiskHealth(source="nvme", temperature_c=47),  # hwmon: the current one
            health_note="needs_admin",
        ),
        PhysicalDisk(id="sda", model="CT2000MX500SSD1", serial="S2", size=2 * 10**12, interface="sata",
                     health_note="needs_admin"),
        PhysicalDisk(id="sdc", model="STORAGE DEVICE", size=10**9, interface="usb", health_note="usb"),
    ]  # fmt: skip


async def test_records_are_read_only_on_request(
    client: Connection, server: Server, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(machine_handlers.disks, "probe", lambda volumes: Probe(disks=linux_disks()))
    shared = FakeShared()
    monkeypatch.setattr(server.scans, "helper", shared)

    page = await client.call(MACHINE_DISKS, Empty())
    assert shared.prompts == 0  # opening the page never asks
    assert [d.health_note for d in page.disks] == ["needs_admin", "needs_admin", "usb"]

    read = await client.call(MACHINE_READ_HEALTH, ReadHealthParams())
    assert read.outcome == "read" and read.message == "" and shared.prompts == 1
    assert shared.client.asked == ["nvme0n1", "sda"]  # not the card reader
    nvme, sata, reader = read.machine.disks
    assert nvme.health is not None and nvme.health_note == ""
    assert nvme.health.written_bytes == 1_000_000 * 512_000 and nvme.health.power_on_hours == 8070
    assert nvme.health.temperature_c == 47 and nvme.health.read_at is not None  # the live temperature
    assert (sata.health, sata.health_note) == (None, "unsupported")  # it didn't answer: why
    assert reader.health_note == "usb"

    # later reads keep the record; while the helper runs they ask it again, without a prompt
    again = await client.call(MACHINE_DISKS, Empty())
    assert again.disks[0].health == nvme.health.model_copy(update={"read_at": again.disks[0].health.read_at})  # type: ignore[union-attr]
    assert shared.client.asked == ["nvme0n1", "sda", "nvme0n1"] and shared.prompts == 1
    await shared.close()
    kept = await client.call(MACHINE_DISKS, Empty())
    assert (
        kept.disks[0].health is not None and kept.disks[0].health.written_bytes == nvme.health.written_bytes
    )
    assert kept.disks[1].health_note == "unsupported" and len(shared.client.asked) == 3

    # nothing left to read
    nothing = await client.call(MACHINE_READ_HEALTH, ReadHealthParams())
    assert nothing.outcome == "nothing" and shared.prompts == 1


@pytest.mark.parametrize(
    ("fail", "outcome"),
    [
        (HelperDenied(), "denied"),
        (HelperUnavailable("没有可以询问管理员密码的身份验证代理", "no_agent"), "unavailable"),
    ],
)
async def test_records_when_rights_are_not_given(
    client: Connection, server: Server, monkeypatch: pytest.MonkeyPatch, fail: Exception, outcome: str
) -> None:
    monkeypatch.setattr(machine_handlers.disks, "probe", lambda volumes: Probe(disks=linux_disks()))
    monkeypatch.setattr(server.scans, "helper", FakeShared(fail))
    read = await client.call(MACHINE_READ_HEALTH, ReadHealthParams(disks=["nvme0n1"]))
    assert read.outcome == outcome and read.message
    assert [d.health_note for d in read.machine.disks] == [
        "needs_admin",
        "needs_admin",
        "usb",
    ]  # still offered


# ---------------------------------------------------------------- what a scan showed, kept


async def test_what_a_scan_showed_is_kept(
    tmp_path: Path, client: Connection, server: Server, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Closing the scan's tab keeps its classes and folders; a new backend still shows them, and they
    add up to the page's class total."""
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)]))
    status = await wait_done(client, started.scan_id)
    # let the scanned folder stand in for its whole volume
    monkeypatch.setattr(machine_handlers, "_whole_root", lambda h: "x" if h.id == started.scan_id else None)
    volume = server.scans.get(started.scan_id).roots[0].volume
    assert volume is not None
    await client.call(SCAN_CLOSE, p.ScanIdParams(scan_id=started.scan_id))

    summaries = VolumeSummaries()  # read back from the file, as after a restart
    kept = summaries.get(volume)
    assert kept is not None and (kept.files, kept.bytes_alloc) == (status.files, status.bytes_alloc)
    assert kept.classes and kept.folders and kept.at == status.finished_at
    assert [c.bytes for c in kept.classes] == sorted((c.bytes for c in kept.classes), reverse=True)

    out = machine_handlers.overview(server, [volume], Probe(), {}, summaries.get)
    scan = out.volumes[0].scan
    assert scan is not None and scan.source == "kept" and scan.scan_id is None
    assert scan.classes == kept.classes[: machine_handlers.TOP_CLASSES] and scan.folders == kept.folders
    assert out.classes is not None
    assert (out.classes.volumes, out.classes.bytes) == ([volume.mount_point], status.bytes_alloc)
    assert sum(c.bytes for c in out.classes.classes) == sum(c.bytes for c in kept.classes)

    # a stopped scan keeps nothing; without a known mix there is no total
    assert machine_handlers.overview(server, [volume], Probe(), {}).classes is None


def test_class_total_adds_up_volumes(server: Server) -> None:
    def vol(mount: str, serial: str) -> VolumeInfo:
        return VolumeInfo(mount, mount, "NTFS", "", 10**12, 10**11, 4096, False, False, serial=serial)

    c, d, e = vol("C:\\", "1"), vol("D:\\", "2"), vol("E:\\", "3")
    store = {
        identity(c): Kept(mount="C:\\", identity=identity(c), at=datetime.now(UTC), seen=datetime.now(UTC),
                          bytes_alloc=700, classes=[ClassStat(class_id="game", bytes=500, files=5),
                                                    ClassStat(class_id="code", bytes=200, files=50)]),
        identity(d): Kept(mount="D:\\", identity=identity(d), at=datetime.now(UTC), seen=datetime.now(UTC),
                          bytes_alloc=300, classes=[ClassStat(class_id="code", bytes=350, files=30)]),
    }  # fmt: skip
    out = machine_handlers.overview(server, [c, d, e], Probe(), {}, lambda v: store.get(identity(v)))
    total = out.classes
    assert total is not None and total.volumes == ["C:\\", "D:\\"] and total.bytes == 1000  # E: never scanned
    assert [(x.class_id, x.bytes, x.files) for x in total.classes] == [("code", 550, 80), ("game", 500, 5)]

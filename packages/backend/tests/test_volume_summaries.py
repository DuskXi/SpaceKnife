"""What the 「本机磁盘」 page keeps of each volume's last complete scan (server.volume_summaries)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from spaceknife.scan.volume import VolumeInfo
from spaceknife.server import volume_summaries as vs
from spaceknife.server.volume_summaries import Kept, VolumeSummaries, identity
from spaceknife_protocol.machine import FolderStat
from spaceknife_protocol.scan import ClassStat

NOW = datetime(2026, 10, 21, 12, 0, tzinfo=UTC)


def volume(mount: str = "D:\\", serial: str = "7A3C-11F0", total: int = 800 * 10**9) -> VolumeInfo:
    return VolumeInfo(mount, mount, "NTFS", "SSDData", total, total // 2, 4096, False, False, serial=serial)


def kept(v: VolumeInfo, at: datetime = NOW, **patch: object) -> Kept:
    fields: dict[str, object] = {
        "mount": v.mount_point,
        "identity": identity(v),
        "at": at,
        "seen": at,
        "files": 42_533,
        "dirs": 3_100,
        "bytes_alloc": 280 * 10**9,
        "method": "mft",
        "classes": [
            ClassStat(class_id="game", bytes=211 * 10**9, files=9_000),
            ClassStat(class_id="archive", bytes=62 * 10**9, files=400),
        ],
        "folders": [FolderStat(name="Koikatsu Party", bytes=211 * 10**9)],
    }
    fields.update(patch)
    return Kept.model_validate(fields)


def test_kept_across_a_restart(tmp_path: Path) -> None:
    file = tmp_path / "data" / "volume_summaries.json"
    d = volume()
    store = VolumeSummaries(file)
    assert store.get(d) is None
    store.put([kept(d)])
    again = VolumeSummaries(file)  # a new backend reads the same file
    k = again.get(d)
    assert k is not None and k == kept(d)
    assert [c.class_id for c in k.classes] == ["game", "archive"] and k.folders[0].name == "Koikatsu Party"
    assert not file.with_suffix(".tmp").exists()


def test_only_the_same_volume(tmp_path: Path) -> None:
    store = VolumeSummaries(tmp_path / "s.json")
    d = volume()
    store.put([kept(d)])
    assert store.get(volume(serial="0000-0001")) is None  # another drive on D:
    assert store.get(volume(mount="E:\\")) is None
    no_serial = volume(serial="")
    assert identity(no_serial) == "ntfs:800000000000"
    store.put([kept(no_serial)])
    assert store.get(no_serial) is not None and store.get(volume(serial="", total=1)) is None
    assert len(store) == 2  # both drives that were on D: keep their entries


def test_a_newer_scan_replaces_an_older_one(tmp_path: Path) -> None:
    store = VolumeSummaries(tmp_path / "s.json")
    d = volume()
    store.put([kept(d, files=1)])
    store.put([kept(d, at=NOW - timedelta(hours=1), files=2)])  # older: ignored
    assert store.get(d).files == 1  # type: ignore[union-attr]
    store.put([kept(d, at=NOW + timedelta(hours=1), files=3)])
    assert store.get(d).files == 3  # type: ignore[union-attr]


def test_no_write_when_nothing_changed(tmp_path: Path) -> None:
    file = tmp_path / "s.json"
    store = VolumeSummaries(file)
    d = volume()
    store.put([kept(d)])
    stamp = file.stat().st_mtime_ns
    store.put([kept(d, seen=NOW + timedelta(minutes=5))])  # the same scan read again
    store.seen([d], NOW + timedelta(hours=1))  # seen within the day
    assert file.stat().st_mtime_ns == stamp


def test_bounded_and_forgotten(tmp_path: Path) -> None:
    store = VolumeSummaries(tmp_path / "s.json")
    recent = datetime.now(UTC)
    drives = [volume(mount=f"/media/drive{i}", serial=f"S{i}") for i in range(vs.KEEP + 5)]
    store.put([kept(v, at=recent, seen=recent - timedelta(minutes=i)) for i, v in enumerate(drives)])
    assert len(store) == vs.KEEP
    assert store.get(drives[0]) is not None and store.get(drives[-1]) is None  # the least recently seen go
    # an unplugged drive keeps its entry for a while, then goes; a present one stays
    later = recent + vs.FORGET + timedelta(days=1)
    store.seen([drives[0]], recent + vs.FORGET - timedelta(days=1))
    store.seen([], later)
    assert len(store) == 1 and store.get(drives[0]) is not None


def test_unreadable_file_starts_over(tmp_path: Path) -> None:
    file = tmp_path / "s.json"
    file.write_text("{not json", encoding="utf-8")
    store = VolumeSummaries(file)
    assert len(store) == 0
    store.put([kept(volume())])
    assert VolumeSummaries(file).get(volume()) is not None


def test_what_the_page_shows_of_it(tmp_path: Path) -> None:
    snap = tmp_path / "D-0198a2ef3ad2" / "2026-10-21.sksnap"
    snap.parent.mkdir()
    snap.write_bytes(b"x")
    k = kept(volume(), snapshot=str(snap))
    scan = k.volume_scan(top=1)
    assert (scan.source, scan.scan_id, scan.running, scan.files, scan.snapshot) == (
        "kept",
        None,
        False,
        42_533,
        str(snap),
    )
    assert [c.class_id for c in scan.classes] == ["game"] and scan.folders == k.folders
    snap.unlink()
    assert k.volume_scan(top=5).snapshot is None  # the snapshot is gone: nothing to open

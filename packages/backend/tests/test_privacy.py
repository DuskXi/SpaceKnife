"""spaceknife.privacy: Full Disk Access check and the folders that ask one by one (macOS TCC)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

from spaceknife import privacy


@pytest.fixture
def mac(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "platform", "darwin")


def tcc_db(home: Path) -> Path:
    db = home / "Library" / "Application Support" / "com.apple.TCC" / "TCC.db"
    db.parent.mkdir(parents=True)
    db.write_bytes(b"SQLite format 3\0")
    return db


def test_off_macos_there_is_nothing_to_tell(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "platform", "linux")
    tcc_db(tmp_path)
    assert privacy.full_disk_access(str(tmp_path)) is None
    assert privacy.protected_folders(str(tmp_path)) == ()


def test_full_disk_access_reads_the_tcc_database(tmp_path: Path, mac: None) -> None:
    assert privacy.full_disk_access(str(tmp_path)) is None  # no probe at all
    tcc_db(tmp_path)
    assert privacy.full_disk_access(str(tmp_path)) is True


def test_refused_probe_means_no_full_disk_access(
    tmp_path: Path, mac: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    tcc_db(tmp_path)

    def refuse(*_args: object, **_kw: object) -> None:
        raise PermissionError(1, "Operation not permitted")

    monkeypatch.setattr(privacy, "open", refuse, raising=False)
    assert privacy.full_disk_access(str(tmp_path)) is False


def test_protected_folders_are_the_existing_prompting_places(tmp_path: Path, mac: None) -> None:
    for rel in ("Desktop", "Downloads", "Library/Mobile Documents"):
        (tmp_path / rel).mkdir(parents=True)
    found = privacy.protected_folders(str(tmp_path))
    assert [Path(p).relative_to(tmp_path).as_posix() for p in found] == [
        "Desktop",
        "Downloads",
        "Library/Mobile Documents",
    ]


def test_inside() -> None:
    assert privacy.inside("/Users/lenny/Desktop", "/Users/lenny")
    assert privacy.inside("/Users/lenny/Desktop", "/")
    assert not privacy.inside("/Users/lenny", "/Users/lenny")
    assert not privacy.inside("/Users/lennyx/Desktop", "/Users/lenny")


def test_scan_start_decides_whether_to_skip_protected_places(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.server import scans

    monkeypatch.setattr(scans, "full_disk_access", lambda: False)
    assert scans.wants_skip_protected(True, "allow") is True  # the app asked the user: that decides
    assert scans.wants_skip_protected(False, "skip") is False
    assert scans.wants_skip_protected(None, "skip") is True  # the setting, without Full Disk Access
    assert scans.wants_skip_protected(None, "ask") is False  # nobody to ask (command line): access
    assert scans.wants_skip_protected(None, "allow") is False
    monkeypatch.setattr(scans, "full_disk_access", lambda: True)
    assert scans.wants_skip_protected(None, "skip") is False  # nothing would ask
    monkeypatch.setattr(scans, "full_disk_access", lambda: None)  # not macOS
    assert scans.wants_skip_protected(None, "skip") is False

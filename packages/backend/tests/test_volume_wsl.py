"""Inside WSL, Windows drives (9P / DrvFs) are listed but marked as slow; WSL plumbing is hidden."""

from __future__ import annotations

import pytest

from spaceknife.scan import volume as vol
from spaceknife.scan.volume import VolumeInfo


def mount(point: str, fs: str, source: str, minor: int) -> vol._Mount:
    return vol._Mount(point, fs, source, 0, minor)


MOUNTS = [
    mount("/", "ext4", "/dev/sdf", 1),
    mount("/mnt/c", "9p", "C:\\", 2),
    mount("/mnt/d", "9p", "D:\\", 3),
    mount("/Docker/host", "9p", "C:\\Program Files\\Docker\\Docker\\resources", 4),
    mount("/usr/lib/wsl/drivers", "9p", "drivers", 5),
    mount("/mnt/wslg/distro", "ext4", "/dev/sdf", 6),
    mount("/mnt/wsl/docker-desktop/cli-tools", "iso9660", "/dev/loop0", 7),
    mount("/srv/share", "nfs4", "nas:/export", 8),
]


def fake_statvfs(path: str, point: str, fs: str, source: str, remote: bool) -> VolumeInfo:
    return VolumeInfo(
        point, source, fs, point, 100, 50, 4096, remote, False, remote_path=source if remote else ""
    )


@pytest.mark.parametrize("wsl", [True, False])
def test_wsl_volume_list(monkeypatch: pytest.MonkeyPatch, wsl: bool) -> None:
    monkeypatch.setattr(vol, "_linux_mounts", lambda: MOUNTS)
    monkeypatch.setattr(vol, "_posix_from_statvfs", fake_statvfs)
    monkeypatch.setattr(vol, "in_wsl", lambda: wsl)
    listed = {v.mount_point: v for v in vol._linux_list()}
    if wsl:
        assert set(listed) == {"/", "/mnt/c", "/mnt/d", "/srv/share"}
        c = listed["/mnt/c"]
        assert c.label == "C:（Windows 盘）" and c.note == vol.WSL_DRIVE_NOTE and c.is_remote
        assert listed["/"].note == "" and listed["/srv/share"].note == ""
    else:  # a plain Linux machine keeps every real mount and marks nothing
        assert "/Docker/host" in listed and "/mnt/wslg/distro" in listed
        assert all(v.note == "" for v in listed.values())


def test_wsl_drive_sources() -> None:
    assert vol._wsl_drive(mount("/mnt/c", "9p", "C:\\", 1)) == "C:"
    assert vol._wsl_drive(mount("/mnt/e", "drvfs", "e:", 1)) == "E:"
    assert vol._wsl_drive(mount("/x", "9p", "drivers", 1)) == ""
    assert vol._wsl_drive(mount("/x", "ext4", "C:\\", 1)) == ""

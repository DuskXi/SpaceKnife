"""Inside WSL, Windows drives (9P / DrvFs) are listed but marked as slow; WSL plumbing is hidden."""

from __future__ import annotations

import sys
from pathlib import Path

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
        assert (
            c.label == "C:（Windows 盘）"
            and c.note == vol.WSL_DRIVE_NOTE
            and c.is_remote
            and not c.remote_path
        )
        assert listed["/"].note == "" and listed["/srv/share"].note == ""
    else:  # a plain Linux machine keeps every real mount and marks nothing
        assert "/Docker/host" in listed and "/mnt/wslg/distro" in listed
        assert all(v.note == "" for v in listed.values())


def test_wsl_drive_sources() -> None:
    assert vol._wsl_drive(mount("/mnt/c", "9p", "C:\\", 1)) == "C:"
    assert vol._wsl_drive(mount("/mnt/e", "drvfs", "e:", 1)) == "E:"
    assert vol._wsl_drive(mount("/x", "9p", "drivers", 1)) == ""
    assert vol._wsl_drive(mount("/x", "ext4", "C:\\", 1)) == ""


@pytest.mark.skipif(sys.platform == "win32", reason="statvfs is POSIX")
def test_root_reserved_blocks_are_neither_used_nor_free(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    class St:  # 1000 blocks of 4 KiB: 200 in use, 800 free of which 50 only for root
        f_frsize = f_bsize = 4096
        f_blocks, f_bfree, f_bavail = 1000, 800, 750

    monkeypatch.setattr(vol.os, "statvfs", lambda path: St())
    v = vol._posix_from_statvfs(str(tmp_path), "/", "ext4", "/dev/sda1", False)
    assert (v.total, v.free, v.reserved, v.used) == (1000 * 4096, 750 * 4096, 50 * 4096, 200 * 4096)


def test_hypervisor_disks_are_virtual(tmp_path: Path) -> None:
    from spaceknife.scan import media

    for vendor, model, virtual in [
        ("Msft    ", "Virtual Disk    ", True),  # WSL 2 / Hyper-V (claims to rotate)
        ("QEMU", "QEMU HARDDISK", True),
        ("ATA", "WDC WD40EFRX-68N", False),
    ]:
        disk = tmp_path / model.strip().replace(" ", "_")
        (disk / "device").mkdir(parents=True)
        (disk / "device" / "vendor").write_text(vendor)
        (disk / "device" / "model").write_text(model)
        assert media._virtual_disk(str(disk)) is virtual

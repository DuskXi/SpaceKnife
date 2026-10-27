"""Volume → physical disk mapping and the OS I/O priority helpers."""

from __future__ import annotations

import os
import subprocess
import sys
import threading
from collections.abc import Iterator
from pathlib import Path

import pytest

from spaceknife.io.disks import (
    darwin_whole_disk,
    disk_for_path,
    disk_ref_for_path,
    linux_whole_disk,
    local_disks,
)
from spaceknife.io.priority import (
    io_priority_is_low,
    no_cloud_downloads,
    set_low_io_priority,
    set_thread_low_io_priority,
)


def test_this_checkout_maps_to_a_physical_disk() -> None:
    ref = disk_ref_for_path(os.getcwd())
    if sys.platform == "win32":
        assert ref.id.startswith("PhysicalDrive") and ref.number is not None  # C: → \\.\PhysicalDriveN
        assert ref.id == f"PhysicalDrive{ref.number}"
        assert disk_for_path(os.environ.get("SYSTEMDRIVE", "C:") + "\\").startswith("PhysicalDrive")
    elif sys.platform == "linux":
        if ref.kind == "volume":
            pytest.skip("no block device behind the checkout (overlay / tmpfs)")
        assert os.path.isdir(f"/sys/block/{ref.id}")
    assert ref.sampled
    assert ref.id in {r.id for r in local_disks()} or sys.platform == "darwin"


def test_mapping_never_raises() -> None:
    ref = disk_ref_for_path(os.path.join(os.getcwd(), "no", "such", "dir"))
    assert ref.id  # resolved through the existing part of the path, or a vol: stand-in


@pytest.mark.skipif(sys.platform != "win32", reason="Windows UNC paths")
def test_unc_roots_are_network_disks() -> None:
    from spaceknife.io.disks import _win_ref_for_root

    ref = _win_ref_for_root("\\\\nas\\share\\")  # decided from the path alone, no network access
    assert ref.kind == "network" and ref.id == "net:\\\\nas\\share" and not ref.sampled


@pytest.mark.skipif(sys.platform == "win32", reason="sysfs symlinks")
def test_linux_whole_disk_resolves_partitions_and_device_mapper(tmp_path: Path) -> None:
    sysfs = tmp_path / "sys"
    nvme = sysfs / "devices" / "pci0000:00" / "nvme" / "nvme0" / "nvme0n1"
    (nvme / "nvme0n1p2").mkdir(parents=True)
    (nvme / "nvme0n1p2" / "partition").write_text("2")
    dm = sysfs / "devices" / "virtual" / "block" / "dm-0"
    (dm / "slaves").mkdir(parents=True)
    classes = sysfs / "class" / "block"
    classes.mkdir(parents=True)
    os.symlink(nvme / "nvme0n1p2", classes / "nvme0n1p2")
    os.symlink(nvme, classes / "nvme0n1")
    os.symlink(dm, classes / "dm-0")
    os.symlink(classes / "nvme0n1p2", dm / "slaves" / "nvme0n1p2")
    assert linux_whole_disk(str(classes / "nvme0n1p2"), str(sysfs)) == "nvme0n1"  # partition → disk
    assert linux_whole_disk(str(classes / "nvme0n1"), str(sysfs)) == "nvme0n1"
    assert linux_whole_disk(str(classes / "dm-0"), str(sysfs)) == "nvme0n1"  # LVM / LUKS → backing disk
    assert linux_whole_disk(str(classes / "missing"), str(sysfs)) is None


def test_darwin_whole_disk_from_diskutil_info() -> None:
    infos = {"disk3s5": {"ParentWholeDisk": "disk0"}, "disk0s2": {"ParentWholeDisk": "disk0"}}
    apfs = {"DeviceIdentifier": "disk3s1s1", "APFSPhysicalStores": [{"APFSPhysicalStore": "disk0s2"}]}
    assert darwin_whole_disk(apfs, lambda name: infos.get(name, {})) == "disk0"
    assert darwin_whole_disk({"DeviceIdentifier": "disk4s1", "ParentWholeDisk": "disk4"}) == "disk4"
    assert darwin_whole_disk({"DeviceIdentifier": "disk5s2"}) == "disk5"
    assert darwin_whole_disk({}) is None


# --------------------------------------------------------------------------- I/O priority

needs_priority = pytest.mark.skipif(sys.platform == "darwin", reason="covered by the macOS policy API")


@pytest.fixture(autouse=True)
def normal_io_priority() -> Iterator[None]:
    """The priority tests start from normal I/O priority. Some environments start every process at
    low (GitHub's Windows runners: see the "process I/O priority at start" report header), which
    children inherit: raise it for the test and put it back afterwards."""
    started_low = io_priority_is_low() is True
    if started_low:
        assert set_low_io_priority(None, False)
    yield
    if started_low:
        set_low_io_priority(None, True)


@needs_priority
def test_process_io_priority_round_trip() -> None:
    assert io_priority_is_low() is False
    try:
        assert set_low_io_priority(None, True)
        assert io_priority_is_low() is True
    finally:
        assert set_low_io_priority(None, False)
    assert io_priority_is_low() is False


@needs_priority
def test_thread_io_priority_is_per_thread() -> None:
    seen: dict[str, object] = {}

    def worker() -> None:
        seen["set"] = set_thread_low_io_priority(True)
        seen["low"] = io_priority_is_low(thread=True)
        seen["restored"] = set_thread_low_io_priority(False)
        seen["after"] = io_priority_is_low(thread=True)

    t = threading.Thread(target=worker)
    t.start()
    t.join()
    assert seen == {"set": True, "low": True, "restored": True, "after": False}
    assert io_priority_is_low() is False  # the rest of the process is untouched


@needs_priority
def test_child_process_io_priority() -> None:
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
    try:
        assert io_priority_is_low(child.pid) is False
        assert set_low_io_priority(child.pid, True)  # a pool worker, lowered from the parent
        assert io_priority_is_low(child.pid) is True
        assert set_low_io_priority(child.pid, False)
        assert io_priority_is_low(child.pid) is False
    finally:
        child.kill()
        child.wait()


@pytest.mark.skipif(sys.platform != "win32", reason="Windows background processing mode")
def test_windows_background_mode() -> None:
    try:
        assert set_low_io_priority(None, True, background_mode=True)
        assert io_priority_is_low() is True
    finally:
        assert set_low_io_priority(None, False, background_mode=True)
    assert io_priority_is_low() is False


def test_no_cloud_downloads_is_a_macos_process_policy() -> None:
    """macOS 10.15+: setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, …, OFF) succeeds (CI
    checks the constants on a real macOS); elsewhere there is nothing to set."""
    assert no_cloud_downloads() is (sys.platform == "darwin")

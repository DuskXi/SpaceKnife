"""Disk load samplers: counter parsing (Linux sysfs, PDH, DISK_PERFORMANCE, IOKit) and the live ones."""

from __future__ import annotations

import os
import plistlib
import struct
import sys
import time
from pathlib import Path

import pytest

from spaceknife.io.disks import DiskId, disk_ref_for_path, linux_physical_disks
from spaceknife.io.sampler import (
    DarwinStat,
    DiskPerf,
    LinuxStat,
    SysfsSampler,
    darwin_sample,
    default_sampler,
    linux_sample,
    parse_disk_performance,
    parse_ioreg,
    parse_linux_stat,
    pdh_disk_number,
    pdh_instance_letters,
    pdh_samples,
    perf_sample,
)

# a real line from Ubuntu 22.04 / kernel 6.6 (17 fields)
STAT_17 = " 4136354  4515195 69216064   470692  2082300  6051928 65073816  7927831        0   675528  8398529        0        0        0        0        1        5\n"


def test_parse_linux_stat_field_counts() -> None:
    st = parse_linux_stat(STAT_17)
    assert st == LinuxStat(4136354, 470692, 2082300, 7927831, 69216064, 0, 675528, 8398529)
    eleven = " ".join(STAT_17.split()[:11])
    assert parse_linux_stat(eleven) == st  # kernels before 4.18
    assert parse_linux_stat("1 2 3") is None
    assert parse_linux_stat("a b c d e f g h i j k") is None


def test_linux_sample_deltas() -> None:
    prev = LinuxStat(
        reads=1000,
        read_ms=500,
        writes=10,
        write_ms=40,
        sectors_read=8000,
        in_flight=0,
        io_ticks=10_000,
        time_in_queue=20_000,
    )
    cur = LinuxStat(
        reads=3000,
        read_ms=900,
        writes=20,
        write_ms=140,
        sectors_read=8000 + 2000 * 32,
        in_flight=3,
        io_ticks=10_250,
        time_in_queue=20_750,
    )
    s = linux_sample(prev, cur, dt=0.5, t=100.0)
    assert s is not None
    assert s.busy == pytest.approx(0.5)  # 250 ms of 500 ms with I/O in flight
    assert s.queue == pytest.approx(1.5)  # 750 ms weighted / 500 ms
    assert s.read_iops == pytest.approx(4000)
    assert s.read_latency == pytest.approx(0.0002)  # 400 ms over 2000 reads
    assert s.read_bps == pytest.approx(2000 * 32 * 512 / 0.5)  # 16 KiB reads
    wrapped = cur._replace(io_ticks=5)
    assert linux_sample(prev, wrapped, 0.5, 100.0) is None
    assert linux_sample(prev, cur, 0.0, 100.0) is None


def test_pdh_instances_and_combination() -> None:
    assert pdh_disk_number("3 C: D:") == 3
    assert pdh_disk_number("12") == 12
    assert pdh_disk_number("_Total") is None
    assert pdh_instance_letters("1 D: e:") == ["D:\\", "E:\\"]
    values = {
        "idle": {"0 D:": 100.0028, "3 C:": 40.0, "_Total": 70.0},
        "queue": {"0 D:": 0.0, "3 C:": 2.5, "_Total": 2.5},
        "latency": {"3 C:": 0.0001},
        "iops": {"3 C:": 25000.0},
        "bps": {"3 C:": 25000.0 * 16384},
    }
    out = pdh_samples(values, t=5.0, dt=0.25)
    assert set(out) == {DiskId("PhysicalDrive0"), DiskId("PhysicalDrive3")}
    c = out[DiskId("PhysicalDrive3")]
    assert c.busy == pytest.approx(0.6) and c.queue == 2.5 and c.dt == 0.25
    assert c.read_iops == 25000.0 and c.read_latency == 0.0001 and c.read_bps == 25000.0 * 16384
    d = out[DiskId("PhysicalDrive0")]
    assert d.busy == 0.0  # "% Idle Time" above 100 is clamped
    assert d.read_iops is None


def test_disk_performance_counters() -> None:
    # BytesRead, BytesWritten, ReadTime, WriteTime, IdleTime, ReadCount, WriteCount, QueueDepth, SplitCount, QueryTime
    raw0 = struct.pack("<qqqqqIIIIqI16s", 0, 0, 0, 0, 0, 0, 0, 0, 0, 1_000_000, 3, b"") + b"\0" * 4
    raw1 = struct.pack(
        "<qqqqqIIIIqI16s", 100 * 4096, 0, 2_000_000, 500_000, 2_500_000, 100, 10, 2, 0, 6_000_000, 3, b""
    )
    p0, p1 = parse_disk_performance(raw0), parse_disk_performance(raw1)
    assert p0 is not None and p1 is not None
    assert p1 == DiskPerf(100 * 4096, 0, 2_000_000, 500_000, 2_500_000, 100, 10, 6_000_000)
    s = perf_sample(p0, p1, t=1.0)
    assert s is not None
    assert s.dt == pytest.approx(0.5)  # 5e6 × 100 ns
    assert s.busy == pytest.approx(0.5)
    assert s.queue == pytest.approx(0.5)  # (2e6 + 0.5e6) / 5e6
    assert s.read_iops == pytest.approx(200)
    assert s.read_latency == pytest.approx(0.002)
    assert perf_sample(p1, p0, t=1.0) is None
    assert parse_disk_performance(b"short") is None


def test_ioreg_statistics() -> None:
    def entry(name: str, reads: int, read_ns: int, write_ns: int) -> dict[str, object]:
        return {
            "IOObjectClass": "IOBlockStorageDriver",
            "Statistics": {
                "Operations (Read)": reads,
                "Operations (Write)": 7,
                "Total Time (Read)": read_ns,
                "Total Time (Write)": write_ns,
                "Bytes (Read)": reads * 4096,
            },
            "IORegistryEntryChildren": [{"IOObjectClass": "IOMedia", "BSD Name": name}],
        }

    raw = plistlib.dumps([entry("disk0", 1000, 2_000_000_000, 0), {"no": "stats"}])
    stats = parse_ioreg(raw)
    assert stats == {"disk0": DarwinStat(2_000_000_000, 1000, 2_000_000_000, 4_096_000)}
    later = DarwinStat(2_500_000_000, 2000, 2_400_000_000, 8_192_000)
    s = darwin_sample(stats["disk0"], later, dt=1.0, t=2.0)
    assert s is not None
    assert s.queue == pytest.approx(0.5) and s.busy == pytest.approx(0.5)
    assert s.read_iops == 1000 and s.read_latency == pytest.approx(0.0004)
    assert parse_ioreg(b"not a plist") == {}


def _fake_sysfs(root: Path, disks: dict[str, str]) -> None:
    for name, stat in disks.items():
        d = root / "block" / name
        d.mkdir(parents=True)
        (d / "stat").write_text(stat, encoding="ascii")


def test_sysfs_sampler_on_a_fake_tree(tmp_path: Path) -> None:
    _fake_sysfs(tmp_path, {"sda": STAT_17})
    assert linux_physical_disks(str(tmp_path)) == ["sda"]
    sampler = SysfsSampler(root=str(tmp_path))
    assert [r.id for r in sampler.refs()] == ["sda"]
    fields = STAT_17.split()
    fields[0] = str(int(fields[0]) + 500)  # reads
    fields[3] = str(int(fields[3]) + 50)  # read ms
    fields[9] = str(int(fields[9]) + 100)  # io_ticks
    fields[10] = str(int(fields[10]) + 300)  # time_in_queue
    time.sleep(0.2)
    (tmp_path / "block" / "sda" / "stat").write_text(" ".join(fields), encoding="ascii")
    out = sampler.sample()
    s = out[DiskId("sda")]
    assert 0.0 < s.busy <= 1.0 and s.queue > s.busy
    assert s.read_latency == pytest.approx(0.0001)


@pytest.mark.skipif(sys.platform not in ("win32", "linux"), reason="live sampler for this platform")
def test_live_sampler_sees_the_disk_of_this_checkout() -> None:
    sampler = default_sampler()
    try:
        assert sampler.supported, sampler.name
        disk = disk_ref_for_path(os.getcwd()).id
        sampler.sample()
        time.sleep(0.3)
        out = sampler.sample()
        if disk.startswith("vol:"):  # e.g. an overlay root in a container
            pytest.skip(f"no physical disk behind {os.getcwd()}")
        assert disk in out, (disk, list(out))
        s = out[disk]
        assert 0.0 <= s.busy <= 1.0 and s.queue >= 0.0 and 0.2 < s.dt < 2.0
    finally:
        sampler.close()


@pytest.mark.skipif(sys.platform != "win32", reason="Windows raw disk counters")
def test_disk_performance_ioctl_without_elevation() -> None:
    from spaceknife.io.sampler import WinPerfSampler

    sampler = WinPerfSampler()  # IOCTL_DISK_PERFORMANCE on \\.\PhysicalDriveN opened with access 0
    time.sleep(0.2)
    out = sampler.sample()
    assert disk_ref_for_path(os.getcwd()).id in out

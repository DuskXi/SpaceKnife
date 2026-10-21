"""This computer's disks (spaceknife.disks): link decoding, the Linux sysfs walk on a fake tree, the
macOS parsing on recorded diskutil / system_profiler shapes, and a live probe of this machine."""

from __future__ import annotations

import posixpath
import sys
from datetime import UTC, datetime

import pytest

from spaceknife.disks import darwin as mac
from spaceknife.disks import health, links, probe
from spaceknife.disks.linux import probe as linux_probe
from spaceknife.scan.volume import VolumeInfo, list_volumes
from spaceknife_protocol.machine import DiskLink

# ---------------------------------------------------------------- links


def test_sata_identify_words() -> None:
    # recorded: a Micron SATA SSD at 6 Gb/s (w76 0xb50e: Gen1-3, w77 0x46: current Gen3)
    assert links.sata_identify(0xB50E, 0x0046) == DiskLink(kind="sata", rate_mbps=6000, max_rate_mbps=6000)
    # a 6 Gb/s disk on a 3 Gb/s port
    assert links.sata_identify(0x000E, 0x0004) == DiskLink(kind="sata", rate_mbps=3000, max_rate_mbps=6000)
    assert links.sata_identify(0x0006, 0) == DiskLink(kind="sata", rate_mbps=None, max_rate_mbps=3000)
    assert links.sata_identify(0, 0) is None and links.sata_identify(0xFFFF, 0x46) is None


def test_link_texts() -> None:
    assert links.gbps_text("6.0 Gbps") == 6000 and links.gbps_text("1.5 Gbps") == 1500
    assert links.gbps_text("3 Gigabit") == 3000 and links.gbps_text("<unknown>") is None
    assert [links.pcie_gen_text(t) for t in ("2.5 GT/s PCIe", "8.0 GT/s", "16.0 GT/s PCIe", "Unknown")] == [
        1,
        3,
        4,
        None,
    ]
    assert links.pcie_width_text("4") == 4 and links.pcie_width_text("x16") == 16
    assert links.pcie_width_text("3") is None and links.pcie_width_text("") is None
    assert links.pcie_gen_code(4) == 4 and links.pcie_gen_code(0) is None and links.pcie_gen_code(9) is None
    assert links.pcie(None, None, 5, 4) is None


def test_usb_links() -> None:
    # recorded: a USB 3 card reader behind a USB 2 hub port (high speed, SuperSpeed capable)
    assert links.usb_windows(2, 0x2) == DiskLink(kind="usb", rate_mbps=480, max_rate_mbps=5000)
    assert links.usb_windows(3, 0x1 | 0x2) == DiskLink(kind="usb", rate_mbps=5000, max_rate_mbps=5000)
    assert links.usb_windows(3, 0x1 | 0x2 | 0x4 | 0x8) == DiskLink(
        kind="usb", rate_mbps=10000, max_rate_mbps=10000
    )
    assert links.usb_windows(2, None) == DiskLink(kind="usb", rate_mbps=480)
    assert links.usb_windows(None, None) is None
    assert links.usb_linux("480", " 3.00") == DiskLink(kind="usb", rate_mbps=480, max_rate_mbps=5000)
    assert links.usb_linux("5000", " 3.20") == DiskLink(kind="usb", rate_mbps=5000)
    assert links.usb_linux("480", " 2.00") == DiskLink(kind="usb", rate_mbps=480)
    assert links.usb_linux("", "3.00") is None
    assert links.usb_mac("super_speed") == DiskLink(kind="usb", rate_mbps=5000)
    assert links.usb_mac("Up to 10 Gb/s") == DiskLink(kind="usb", rate_mbps=10000)
    assert links.usb_mac("unknown") is None


# ---------------------------------------------------------------- Linux (a fake sysfs)


class FakeFs:
    """A tree of files and symbolic links (absolute or relative targets), POSIX paths."""

    def __init__(self) -> None:
        self.files: dict[str, str] = {}
        self.links: dict[str, str] = {}

    def file(self, path: str, text: str) -> None:
        self.files[path] = text

    def link(self, path: str, target: str) -> None:
        self.links[path] = target

    def real(self, path: str) -> str:
        out = ""
        for part in path.strip("/").split("/"):
            out = f"{out}/{part}"
            for _ in range(16):
                target = self.links.get(out)
                if target is None:
                    break
                out = posixpath.normpath(posixpath.join(posixpath.dirname(out), target))
        return out

    def _all(self) -> list[str]:
        return [*self.files, *self.links]

    def read(self, path: str) -> str:
        return self.files.get(self.real(path), "").strip()

    def listdir(self, path: str) -> list[str]:
        base = self.real(path).rstrip("/") + "/"
        return sorted({p[len(base) :].split("/")[0] for p in self._all() if p.startswith(base)})

    def exists(self, path: str) -> bool:
        r = self.real(path)
        return r in self.files or any(p.startswith(r + "/") for p in self._all())

    def readlink(self, path: str) -> str:
        return self.links.get(path, "")


def disk(fs: FakeFs, name: str, path: str, sectors: int, *, rot: str = "0", removable: str = "0") -> str:
    fs.link(f"/sys/block/{name}", path)
    fs.link(f"/sys/class/block/{name}", path)
    fs.file(f"{path}/size", str(sectors))
    fs.file(f"{path}/queue/rotational", rot)
    fs.file(f"{path}/removable", removable)
    fs.file(f"{path}/queue/discard_max_bytes", "0" if rot == "1" else "2147450880")
    return path


def part(fs: FakeFs, disk_path: str, name: str, start: int, sectors: int) -> None:
    path = f"{disk_path}/{name}"
    fs.link(f"/sys/class/block/{name}", path)
    fs.file(f"{path}/partition", "1")
    fs.file(f"{path}/start", str(start))
    fs.file(f"{path}/size", str(sectors))


def pci(fs: FakeFs, path: str, speed: str, width: str, top: str, top_width: str) -> None:
    for key, value in (("current_link_speed", speed), ("current_link_width", width)):
        fs.file(f"{path}/{key}", value)
    for key, value in (("max_link_speed", top), ("max_link_width", top_width)):
        fs.file(f"{path}/{key}", value)


def vol(mount: str, device: str) -> VolumeInfo:
    return VolumeInfo(mount, device, "ext4", "", 10**12, 10**11, 4096, False, False)


@pytest.fixture
def machine() -> FakeFs:
    fs = FakeFs()
    # an NVMe drive (PCIe 5.0 capable, in a 4.0 slot) holding the system
    nvme_pci = "/sys/devices/pci0000:00/0000:00:1d.0/0000:3d:00.0"
    nvme = disk(fs, "nvme0n1", f"{nvme_pci}/nvme/nvme0/nvme0n1", 7814037168)
    fs.link(f"{nvme}/device", f"{nvme_pci}/nvme/nvme0")
    fs.file(f"{nvme_pci}/nvme/nvme0/model", "ZHITAI TiPro9000 4TB                    ")
    fs.file(f"{nvme_pci}/nvme/nvme0/serial", "ZTC84T0AB251751VWB  ")
    fs.file(f"{nvme_pci}/nvme/nvme0/firmware_rev", "ZTA31003")
    pci(fs, nvme_pci, "16.0 GT/s PCIe", "4", "32.0 GT/s PCIe", "4")
    part(fs, nvme, "nvme0n1p1", 2048, 1048576)
    part(fs, nvme, "nvme0n1p2", 1050624, 7812986544)
    # a SATA SSD on an add-in controller with one PCIe 2.0 lane
    card = "/sys/devices/pci0000:00/0000:00:1c.4/0000:03:00.0"
    sda = disk(fs, "sda", f"{card}/ata5/host4/target4:0:0/4:0:0:0/block/sda", 3907029168)
    fs.link(f"{sda}/device", f"{card}/ata5/host4/target4:0:0/4:0:0:0")
    fs.file(f"{card}/ata5/host4/target4:0:0/4:0:0:0/vendor", "ATA     ")
    fs.file(f"{card}/ata5/host4/target4:0:0/4:0:0:0/model", "CT2000MX500SSD1 ")
    fs.file(f"{card}/ata5/host4/target4:0:0/4:0:0:0/rev", "M3CR")
    fs.file("/sys/class/ata_link/link5/sata_spd", "6.0 Gbps")
    pci(fs, card, "5.0 GT/s PCIe", "1", "5.0 GT/s PCIe", "1")
    fs.link("/dev/disk/by-id/ata-CT2000MX500SSD1_2247E688D3C9", "../../sda")
    fs.link("/dev/disk/by-id/ata-CT2000MX500SSD1_2247E688D3C9-part1", "../../sda1")
    part(fs, sda, "sda1", 2048, 3907026944)
    # a hard disk on the chipset's SATA (no link of its own), under LVM
    chip = "/sys/devices/pci0000:00/0000:00:17.0"
    sdb = disk(fs, "sdb", f"{chip}/ata1/host0/target0:0:0/0:0:0:0/block/sdb", 7814037168, rot="1")
    fs.link(f"{sdb}/device", f"{chip}/ata1/host0/target0:0:0/0:0:0:0")
    fs.file(f"{chip}/ata1/host0/target0:0:0/0:0:0:0/model", "WDC WD40EFRX-68N32N0")
    fs.file("/sys/class/ata_link/link1/sata_spd", "3.0 Gbps")
    pci(fs, chip, "Unknown", "0", "Unknown", "0")
    part(fs, sdb, "sdb1", 2048, 7814035087)
    lv = "/sys/devices/virtual/block/dm-0"
    fs.link("/sys/block/dm-0", lv)
    fs.link("/sys/class/block/dm-0", lv)
    fs.file(f"{lv}/size", "4000000000")
    fs.link(f"{lv}/slaves/sdb1", f"{sdb}/sdb1")
    fs.link("/dev/mapper/vg-media", "../dm-0")
    # a USB 3 card reader on a USB 2 port
    reader = "/sys/devices/pci0000:00/0000:00:14.0/usb1/1-4/1-4.2"
    sdc = disk(fs, "sdc", f"{reader}/1-4.2:1.0/host6/target6:0:0/6:0:0:0/block/sdc", 998244352, removable="1")
    fs.link(f"{sdc}/device", f"{reader}/1-4.2:1.0/host6/target6:0:0/6:0:0:0")
    fs.file(f"{reader}/1-4.2:1.0/host6/target6:0:0/6:0:0:0/vendor", "Generic ")
    fs.file(f"{reader}/1-4.2:1.0/host6/target6:0:0/6:0:0:0/model", "STORAGE DEVICE  ")
    fs.file(f"{reader}/speed", "480")
    fs.file(f"{reader}/version", " 3.00")
    fs.file(f"{reader}/serial", "000000001532")
    # an SD card on the SoC, a virtio disk, a loop device and an empty nbd
    mmc = "/sys/devices/platform/soc/fe340000.mmc/mmc_host/mmc0/mmc0:aaaa"
    sd = disk(fs, "mmcblk0", f"{mmc}/block/mmcblk0", 124735488, removable="0")
    fs.link(f"{sd}/device", mmc)
    fs.file(f"{mmc}/name", "SC64G")
    fs.file(f"{mmc}/serial", "0x1234abcd")
    disk(fs, "vda", "/sys/devices/pci0000:00/0000:00:04.0/virtio1/block/vda", 20971520, rot="1")
    disk(fs, "loop0", "/sys/devices/virtual/block/loop0", 1000)
    disk(fs, "nbd0", "/sys/devices/virtual/block/nbd0", 0)
    return fs


def test_linux_disks(machine: FakeFs) -> None:
    volumes = [
        vol("/", "/dev/nvme0n1p2"),
        vol("/boot/efi", "/dev/nvme0n1p1"),
        vol("/data", "/dev/sda1"),
        vol("/media", "/dev/mapper/vg-media"),
        vol("/run/media/card", "/dev/sdc"),
        vol("/mnt/share", "server:/export"),
    ]
    found = linux_probe(volumes, fs=machine)  # type: ignore[arg-type]
    by_id = {d.id: d for d in found.disks}
    assert sorted(by_id) == ["mmcblk0", "nvme0n1", "sda", "sdb", "sdc", "vda"]  # no loop / dm / empty nbd

    nvme = by_id["nvme0n1"]
    assert (nvme.model, nvme.serial, nvme.firmware) == (
        "ZHITAI TiPro9000 4TB",
        "ZTC84T0AB251751VWB",
        "ZTA31003",
    )
    assert (nvme.interface, nvme.media, nvme.size, nvme.trim, nvme.system) == (
        "nvme",
        "nvme_ssd",
        7814037168 * 512,
        True,
        True,
    )
    assert nvme.link == DiskLink(kind="pcie", pcie_gen=4, pcie_width=4, max_pcie_gen=5, max_pcie_width=4)

    sata = by_id["sda"]
    assert (sata.model, sata.vendor, sata.serial, sata.media) == (
        "CT2000MX500SSD1",
        "",
        "2247E688D3C9",
        "sata_ssd",
    )
    assert sata.link == DiskLink(kind="sata", rate_mbps=6000)
    assert sata.controller_link == DiskLink(
        kind="pcie", pcie_gen=2, pcie_width=1, max_pcie_gen=2, max_pcie_width=1
    )

    hdd = by_id["sdb"]
    assert (hdd.media, hdd.trim, hdd.controller_link) == ("hdd", False, None)  # chipset: no link reported
    assert hdd.link == DiskLink(kind="sata", rate_mbps=3000)

    reader = by_id["sdc"]
    assert (reader.interface, reader.vendor, reader.model, reader.serial) == (
        "usb",
        "Generic",
        "STORAGE DEVICE",
        "000000001532",
    )
    assert reader.removable and reader.link == DiskLink(kind="usb", rate_mbps=480, max_rate_mbps=5000)

    card = by_id["mmcblk0"]
    assert (card.interface, card.media, card.model, card.serial) == ("sd", "sd_card", "SC64G", "1234abcd")
    assert by_id["vda"].interface == "virtual" and by_id["vda"].media == "virtual"

    # volumes: their disk, their size on it, and where they start
    assert found.parts["/"] == [("nvme0n1", 7812986544 * 512)]
    assert found.offsets["/boot/efi"] == 2048 * 512 < found.offsets["/"]
    assert found.parts["/data"] == [("sda", 3907026944 * 512)]
    assert found.parts["/media"] == [("sdb", 4000000000 * 512)]  # the LV, not the whole PV
    assert found.parts["/run/media/card"] == [("sdc", 998244352 * 512)]  # formatted without partitions
    assert "/mnt/share" not in found.parts


def test_linux_volume_over_two_disks(machine: FakeFs) -> None:
    lv = "/sys/devices/virtual/block/dm-1"
    machine.link("/sys/class/block/dm-1", lv)
    machine.file(f"{lv}/size", "9000000000")
    machine.link(f"{lv}/slaves/sda1", machine.real("/sys/class/block/sda1"))
    machine.link(f"{lv}/slaves/sdb1", machine.real("/sys/class/block/sdb1"))
    machine.link("/dev/mapper/vg-big", "../dm-1")
    found = linux_probe([vol("/big", "/dev/mapper/vg-big")], fs=machine)  # type: ignore[arg-type]
    assert found.parts["/big"] == [("sda", None), ("sdb", None)]  # the split is not known


# ---------------------------------------------------------------- macOS (recorded shapes)


def test_mac_disk_info() -> None:
    nvme = mac.disk_from_info(
        {
            "DeviceIdentifier": "disk0",
            "BusProtocol": "PCI-Express",
            "MediaName": "APPLE SSD AP0512Q",
            "SolidState": True,
            "Removable": False,
            "Internal": True,
            "TotalSize": 500277790720,
            "VirtualOrPhysical": "Physical",
        }
    )
    assert nvme is not None
    assert (nvme.id, nvme.model, nvme.interface, nvme.media, nvme.size) == (
        "disk0",
        "APPLE SSD AP0512Q",
        "nvme",
        "nvme_ssd",
        500277790720,
    )
    stick = mac.disk_from_info(
        {
            "DeviceIdentifier": "disk4",
            "BusProtocol": "USB",
            "MediaName": "SanDisk Ultra Media",
            "SolidState": False,
            "Removable": True,
            "Ejectable": True,
            "Size": 61530439680,
        }
    )
    assert stick is not None and (stick.model, stick.interface, stick.removable) == (
        "SanDisk Ultra",
        "usb",
        True,
    )
    assert mac.disk_from_info({"DeviceIdentifier": "disk3", "VirtualOrPhysical": "Virtual"}) is None  # APFS
    assert mac.disk_from_info({"DeviceIdentifier": "disk0s2"}) is None
    assert [mac.interface_of(p) for p in ("Apple Fabric", "SATA", "Thunderbolt", "Disk Image", "")] == [
        "nvme",
        "sata",
        "thunderbolt",
        "virtual",
        "unknown",
    ]


def test_mac_profile_facts() -> None:
    profile = {
        "SPNVMeDataType": [
            {
                "_name": "Generic SSD Controller",
                "_items": [
                    {
                        "_name": "Samsung SSD 970 EVO Plus 1TB",
                        "bsd_name": "disk2",
                        "device_model": "Samsung SSD 970 EVO Plus 1TB",
                        "device_revision": "2B2QEXM7",
                        "device_serial": "S4EWNX0N123456",
                        "spnvme_linkspeed": "8.0 GT/s",
                        "spnvme_linkwidth": "x4",
                        "spnvme_trim_support": "Yes",
                    }
                ],
            }
        ],
        "SPSerialATADataType": [
            {
                "_name": "Intel 8 Series Chipset",
                "spsata_negotiatedlinkspeed": "6 Gigabit",
                "spsata_linkspeed": "6 Gigabit",
                "_items": [
                    {
                        "_name": "Samsung SSD 860 EVO 1TB",
                        "bsd_name": "disk0",
                        "device_serial": "S3Z9NB0K123456",
                        "spsata_trim_support": "spsata_yes",
                    }
                ],
            }
        ],
        "SPUSBDataType": [
            {
                "_name": "USB31Bus",
                "_items": [
                    {
                        "_name": "Ultra",
                        "device_speed": "super_speed",
                        "serial_num": "4C530001",
                        "Media": [
                            {
                                "_name": "SanDisk Ultra",
                                "bsd_name": "disk4",
                                "volumes": [{"bsd_name": "disk4s1"}],
                            }
                        ],
                    }
                ],
            }
        ],
    }
    facts = mac.profile_facts(profile)
    assert facts["disk2"]["serial"] == "S4EWNX0N123456" and facts["disk2"]["trim"] is True
    assert facts["disk2"]["link"] == DiskLink(kind="pcie", pcie_gen=3, pcie_width=4)
    assert facts["disk0"]["link"] == DiskLink(kind="sata", rate_mbps=6000, max_rate_mbps=6000)
    assert facts["disk0"]["trim"] is True
    assert facts["disk4"]["link"] == DiskLink(kind="usb", rate_mbps=5000)
    assert "disk4s1" not in facts  # volumes are not disks


def test_mac_volume_disks() -> None:
    apfs = {"APFSPhysicalStores": [{"APFSPhysicalStore": "disk0s2"}], "ParentWholeDisk": "disk3"}
    assert mac.volume_disks(apfs) == [("disk0", None)]  # shares its container
    exfat = {"ParentWholeDisk": "disk4", "TotalSize": 61530439680}
    assert mac.volume_disks(exfat) == [("disk4", 61530439680)]
    assert mac.volume_disks({}) == []


# ---------------------------------------------------------------- this machine


def test_probe_this_machine() -> None:
    """Read-only: whatever this system reports, the probe never fails and stays consistent."""
    volumes = list_volumes()
    found = probe(volumes)
    ids = {d.id for d in found.disks}
    assert len(ids) == len(found.disks)
    for mount, parts in found.parts.items():
        assert mount in {v.mount_point for v in volumes}
        assert all(disk in ids for disk, _size in parts)
    if sys.platform == "win32":
        assert all(d.id == f"PhysicalDrive{d.number}" for d in found.disks)
        assert sum(d.system for d in found.disks) <= 1


# ---------------------------------------------------------------- what disks record about themselves


def nvme_log(**fields: int) -> bytes:
    """A SMART / Health log page with the given fields at their offsets."""
    b = bytearray(512)
    b[0] = fields.get("crit", 0)
    b[1:3] = fields.get("kelvin", 320).to_bytes(2, "little")
    b[3], b[4], b[5] = fields.get("spare", 100), fields.get("threshold", 10), fields.get("used", 9)
    for offset, key in (
        (32, "units_read"),
        (48, "units_written"),
        (112, "cycles"),
        (128, "hours"),
        (144, "unsafe"),
        (160, "media"),
    ):
        b[offset : offset + 16] = fields.get(key, 0).to_bytes(16, "little")
    return bytes(b)


def test_nvme_health_log() -> None:
    h = health.nvme_health(
        nvme_log(units_read=206_874_470, units_written=493_120_475, cycles=686, hours=8070, unsafe=106)
    )
    assert h is not None and h.source == "nvme"
    assert (h.temperature_c, h.percentage_used, h.available_spare, h.spare_threshold) == (47, 9, 100, 10)
    assert h.read_bytes == 206_874_470 * 512_000 and h.written_bytes == 493_120_475 * 512_000
    assert (h.power_cycles, h.power_on_hours, h.unsafe_shutdowns, h.media_errors, h.critical_warning) == (
        686,
        8070,
        106,
        0,
        0,
    )
    assert health.nvme_health(bytes(512)) is None  # an empty answer
    assert health.nvme_health(b"\x01" * 100) is None  # a short one
    assert health.nvme_health(nvme_log(kelvin=0)).temperature_c is None  # type: ignore[union-attr]


def qword(value: int, valid: bool = True) -> bytes:
    return (value | (1 << 63) | ((1 << 62) if valid else 0)).to_bytes(8, "little")


def test_ata_device_statistics() -> None:
    general = bytearray(512)
    general[0x08:0x10] = qword(2859)
    general[0x10:0x18] = qword(30322)
    general[0x18:0x20] = qword(25_974_947_733)
    general[0x28:0x30] = qword(78_116_006_871, valid=False)  # not valid: left out
    temps = bytearray(512)
    temps[0x08:0x10] = qword(24)
    h = health.ata_statistics(bytes(general), bytes(temps))
    assert h is not None and h.source == "ata"
    assert (h.power_cycles, h.power_on_hours, h.temperature_c) == (2859, 30322, 24)
    assert h.written_bytes == 25_974_947_733 * 512 and h.read_bytes is None
    below = bytearray(temps)
    below[0x08:0x10] = qword(0xF6)  # −10 °C, a signed byte
    assert health.ata_statistics(bytes(general), bytes(below)).temperature_c == -10  # type: ignore[union-attr]
    assert health.ata_statistics(bytes(512), None) is None  # nothing supported


def test_ata_sector_size() -> None:
    words = bytearray(512)
    assert health.ata_sector_size(bytes(words)) == 512
    words[212:214] = (0x4000 | (1 << 12)).to_bytes(2, "little")  # word 106: logical sector > 256 words
    words[234:238] = (2048).to_bytes(4, "little")  # words 117-118: 2048 words
    assert health.ata_sector_size(bytes(words)) == 4096
    assert health.ata_sector_size(b"") == 512


def test_linux_stat_and_boot_time() -> None:
    line = (
        "  305918    11872 20453866   180210   152731   149412 26389336  2209360        0   350784  2397856"
    )
    assert health.linux_stat(line) == (20453866 * 512, 26389336 * 512)
    assert health.linux_stat("") is None and health.linux_stat("a b c d e f g") is None
    boot = health.boot_time()
    assert boot is None or boot < datetime.now(UTC)


def test_linux_health_and_counters(machine: FakeFs) -> None:
    nvme_ctrl = "/sys/devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0"
    machine.file(f"{nvme_ctrl}/hwmon3/temp1_input", "47850")
    machine.file(machine.real("/sys/block/nvme0n1") + "/stat", "1 0 2000 0 1 0 4000 0 0 0 0")
    sda_dev = machine.real("/sys/block/sda/device")
    machine.file(f"{sda_dev}/hwmon/hwmon5/temp1_input", "31000")
    found = linux_probe([], fs=machine)  # type: ignore[arg-type]
    by_id = {d.id: d for d in found.disks}
    nvme, sata, reader = by_id["nvme0n1"], by_id["sda"], by_id["sdc"]
    assert nvme.health is not None and (nvme.health.source, nvme.health.temperature_c) == ("nvme", 48)
    assert nvme.health.written_bytes is None and nvme.health_note == "needs_admin"  # lifetime: root only
    assert nvme.io is not None and (nvme.io.read_bytes, nvme.io.written_bytes) == (2000 * 512, 4000 * 512)
    assert sata.health is not None and sata.health.temperature_c == 31
    assert by_id["sdb"].health is None and by_id["sdb"].health_note == "needs_admin"  # no drivetemp
    assert reader.health is None and reader.health_note == "usb"


def test_mac_io_counters() -> None:
    tree = [
        {
            "IOObjectClass": "IOBlockStorageDriver",
            "Statistics": {"Bytes (Read)": 4_000_000, "Bytes (Write)": 9_000_000, "Operations (Read)": 10},
            "IORegistryEntryChildren": [
                {
                    "BSD Name": "disk0",
                    "IORegistryEntryChildren": [{"BSD Name": "disk0s1"}, {"BSD Name": "disk0s2"}],
                }
            ],
        },
        {"IOObjectClass": "IOBlockStorageDriver", "Statistics": {"Bytes (Read)": "n/a"}},
    ]
    assert mac.io_counters(tree) == {"disk0": (4_000_000, 9_000_000)}
    assert mac.io_counters(None) == {}

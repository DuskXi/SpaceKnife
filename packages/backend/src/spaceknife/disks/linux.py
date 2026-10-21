"""Linux: physical disks, their links and the volumes on them, from sysfs (no root needed).

- ``/sys/block/<disk>``: size, ``queue/rotational``, ``removable``, ``queue/discard_max_bytes``
  (TRIM), ``device/model`` / ``vendor`` / ``serial`` / ``firmware_rev`` / ``rev``.
- Where the disk hangs (the real path of ``/sys/block/<disk>``):
  NVMe: the PCI function before ``/nvme/`` (``current_link_speed``, ``current_link_width`` and the
  ``max_`` pair); SATA: ``/ataN/`` -> ``/sys/class/ata_link/linkN/sata_spd`` and the controller's
  PCI link; USB: the USB device directory (``speed`` in Mb/s, ``version``); SD: ``mmcblk``.
- ``/dev/disk/by-id``: the serial of a SATA disk (``ata-<model>_<serial>``).
- Volumes: their device's partition (``start``, ``size``) or device-mapper / md ``slaves``.

All file access goes through :class:`Sysfs`, so the tests run on a fake tree on any system.
"""

from __future__ import annotations

import os
import re
from collections.abc import Sequence

from loguru import logger

from spaceknife_protocol.machine import DiskInterface, DiskLink, PhysicalDisk

from ..scan.media import classify
from ..scan.volume import VolumeInfo
from . import Probe
from .links import gbps_text, pcie, pcie_gen_text, pcie_width_text, usb_linux

SKIP = ("loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd", "zd")  # logical, optical, floppy
_VIRTUAL = ("msft virtual disk", "vmware", "qemu", "vbox", "virtual disk")
_PCI_FN = re.compile(r"^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$")
_USB_DEV = re.compile(r"^\d+-\d+(\.\d+)*$")
_MEDIA_BUS = {"nvme": "nvme", "usb": "usb", "sata": "sata", "sd": "sd", "virtual": "virtual"}


class Sysfs:
    """The real filesystem (tests pass a fake with the same methods)."""

    def real(self, path: str) -> str:
        return os.path.realpath(path)

    def read(self, path: str) -> str:
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                return fh.read().strip()
        except OSError:
            return ""

    def listdir(self, path: str) -> list[str]:
        try:
            return sorted(os.listdir(path))
        except OSError:
            return []

    def exists(self, path: str) -> bool:
        return os.path.exists(path)

    def readlink(self, path: str) -> str:
        try:
            return os.readlink(path)
        except OSError:
            return ""


def _int(text: str) -> int | None:
    try:
        return int(text)
    except ValueError:
        return None


def _pci_dir(path: str, before: str) -> str | None:
    """The PCI function directory right above ``before`` in a sysfs device path."""
    head = path.split(before, 1)[0] if before in path else ""
    name = os.path.basename(head)
    return head if _PCI_FN.match(name) else None


def _pci_link(fs: Sysfs, pci: str | None) -> DiskLink | None:
    if pci is None:
        return None
    return pcie(
        pcie_gen_text(fs.read(f"{pci}/current_link_speed")),
        pcie_width_text(fs.read(f"{pci}/current_link_width")),
        pcie_gen_text(fs.read(f"{pci}/max_link_speed")),
        pcie_width_text(fs.read(f"{pci}/max_link_width")),
    )


def _usb_dir(path: str) -> str | None:
    """The deepest USB device directory (2-1, 2-1.4) above the disk."""
    parts = path.split("/")
    for i in range(len(parts) - 1, 0, -1):
        if _USB_DEV.match(parts[i]):
            return "/".join(parts[: i + 1])
    return None


def _by_id_serial(fs: Sysfs, name: str, model: str) -> str:
    """The serial in /dev/disk/by-id/ata-<model>_<serial> (or scsi-SATA_…) of disk ``name``."""
    folder = "/dev/disk/by-id"
    model_part = "_".join(model.split())
    for entry in fs.listdir(folder):
        if "-part" in entry or os.path.basename(fs.readlink(f"{folder}/{entry}")) != name:
            continue
        for prefix in ("ata-", "scsi-SATA_"):
            if entry.startswith(prefix):
                rest = entry[len(prefix) :]
                if model_part and rest.startswith(model_part + "_"):
                    return rest[len(model_part) + 1 :]
                return rest.rsplit("_", 1)[-1]
    return ""


def _interface(fs: Sysfs, name: str, path: str) -> DiskInterface:
    if name.startswith("nvme") or "/nvme/" in path:
        return "nvme"
    if "/usb" in path:
        return "usb"
    if name.startswith("mmcblk"):
        return "sd"
    if name.startswith(("vd", "xvd")) or any(s in path for s in ("/virtio", "/vmbus", "/xen", "/virtual/")):
        return "virtual"
    who = " ".join(f"{fs.read(f'{path}/device/vendor')} {fs.read(f'{path}/device/model')}".lower().split())
    if any(v in who for v in _VIRTUAL):
        return "virtual"
    m = re.search(r"/ata(\d+)/", path)
    if m:
        return "sata" if fs.read(f"/sys/class/ata_link/link{m.group(1)}/sata_spd") else "ata"
    if "/end_device-" in path or "/sas_" in path:
        return "sas"
    if "/host" in path and "/target" in path:
        return "scsi"
    return "other"


def _disk(fs: Sysfs, name: str) -> PhysicalDisk | None:
    path = fs.real(f"/sys/block/{name}")
    sectors = _int(fs.read(f"{path}/size"))
    removable = fs.read(f"{path}/removable") == "1"
    if not sectors and not removable:
        return None  # an absent device (nbd, an unused md slot)
    rot = fs.read(f"{path}/queue/rotational")
    rotational = None if rot == "" else rot == "1"
    discard = _int(fs.read(f"{path}/queue/discard_max_bytes"))
    dev = f"{path}/device"
    interface = _interface(fs, name, path)
    model = fs.read(f"{dev}/model") or fs.read(f"{dev}/name")
    vendor = fs.read(f"{dev}/vendor")
    serial = fs.read(f"{dev}/serial")
    firmware = fs.read(f"{dev}/firmware_rev") or fs.read(f"{dev}/rev")
    link: DiskLink | None = None
    controller: DiskLink | None = None
    if interface == "nvme":
        link = _pci_link(fs, _pci_dir(path, "/nvme/"))
    elif interface in ("sata", "ata"):
        m = re.search(r"/ata(\d+)/", path)
        if m:
            rate = gbps_text(fs.read(f"/sys/class/ata_link/link{m.group(1)}/sata_spd"))
            link = DiskLink(kind="sata", rate_mbps=rate) if rate else None
            controller = _pci_link(fs, _pci_dir(path, f"/ata{m.group(1)}/"))
        serial = serial or _by_id_serial(fs, name, model)
    elif interface == "usb":
        usb = _usb_dir(path)
        if usb is not None:
            link = usb_linux(fs.read(f"{usb}/speed"), fs.read(f"{usb}/version"))
            if not model:
                model = fs.read(f"{usb}/product")
                vendor = vendor or fs.read(f"{usb}/manufacturer")
            serial = serial or fs.read(f"{usb}/serial")
    elif interface == "sd" and serial.startswith("0x"):
        serial = serial[2:]
    return PhysicalDisk(
        id=name,
        model=" ".join(model.split()),
        vendor="" if vendor.strip().upper() in ("", "ATA", "NVME") else " ".join(vendor.split()),
        serial=serial.strip(),
        firmware=firmware.strip(),
        size=sectors * 512 if sectors else None,
        media=classify(_MEDIA_BUS.get(interface, "other"), rotational, removable),
        interface=interface,
        removable=removable,
        trim=None if discard is None else discard > 0,
        link=link,
        controller_link=controller,
    )


def _leaves(fs: Sysfs, name: str, depth: int = 0) -> list[tuple[str, int | None, int | None]]:
    """(disk, bytes on it, start) under a block device: a partition's disk, a whole disk, or the
    disks under a device-mapper / md device."""
    path = fs.real(f"/sys/class/block/{name}")
    if depth > 8 or not fs.exists(path):
        return []
    sectors = _int(fs.read(f"{path}/size"))
    size = sectors * 512 if sectors else None
    if fs.exists(f"{path}/partition"):
        start = _int(fs.read(f"{path}/start"))
        return [(os.path.basename(os.path.dirname(path)), size, start * 512 if start is not None else None)]
    slaves = fs.listdir(f"{path}/slaves")
    if not slaves:
        return [(name, size, 0)]
    found = [leaf for slave in slaves for leaf in _leaves(fs, slave, depth + 1)]
    if len(found) == 1:  # one disk under it (LVM on one partition, LUKS): this volume's size
        disk, inner, start = found[0]
        sizes = [x for x in (size, inner) if x is not None]
        return [(disk, min(sizes) if sizes else None, start)]
    return [(disk, None, start) for disk, _inner, start in found]  # spread over several: unknown split


def probe(volumes: Sequence[VolumeInfo], fs: Sysfs | None = None) -> Probe:
    fs = fs or Sysfs()
    result = Probe()
    for name in fs.listdir("/sys/block"):
        if name.startswith(SKIP):
            continue
        try:
            disk = _disk(fs, name)
        except Exception as exc:  # one odd device must not hide the others
            logger.opt(exception=exc).debug("disk {} probe failed", name)
            continue
        if disk is not None:
            result.disks.append(disk)
    known = {d.id for d in result.disks}
    for v in volumes:
        if v.is_remote or not v.device.startswith("/dev/"):
            continue
        leaves = [leaf for leaf in _leaves(fs, os.path.basename(fs.real(v.device))) if leaf[0] in known]
        if not leaves:
            continue
        result.parts[v.mount_point] = [(disk, size) for disk, size, _start in leaves]
        if leaves[0][2] is not None:
            result.offsets[v.mount_point] = leaves[0][2]
        if v.mount_point == "/":
            on = {disk for disk, _s, _o in leaves}
            result.disks = [d.model_copy(update={"system": True}) if d.id in on else d for d in result.disks]
    return result


__all__ = ["Sysfs", "probe"]

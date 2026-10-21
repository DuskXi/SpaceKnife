"""This computer's disks (``machine.disks``, the 「本机磁盘」 page).

Every physical disk the system reports, scanned or not: model, capacity, the storage media, the
interface and its link (the negotiated rate and the most the device supports: a PCIe 5.0 drive in a
4.0 slot, a USB 3 reader on a USB 2 port), and which volumes live on it. Every volume of the start
page, with the disks under it and what SpaceKnife knows of it from a scan open in this backend or
from its newest snapshot.

The backend reads it all without administrator rights (docs/disks.md); anything a system does not
tell is left empty, never guessed.
"""

from __future__ import annotations

from datetime import datetime
from typing import Literal

from pydantic import Field

from .base import Payload
from .scan import ClassStat, MediaName, VolumeInfo

DiskInterface = Literal[
    "nvme",
    "sata",
    "sas",
    "scsi",
    "ata",
    "usb",
    "sd",
    "thunderbolt",
    "virtual",
    "raid",
    "other",
    "unknown",
]
LinkKind = Literal["pcie", "sata", "usb"]


class DiskLink(Payload):
    """A link: the negotiated rate and, when the system tells it, the most the device supports."""

    kind: LinkKind
    rate_mbps: int | None = Field(
        default=None,
        description="SATA / USB: the negotiated line rate in Mb/s (SATA 1500, 3000, 6000; USB 12, 480, 5000,"
        " 10000, 20000)",
    )
    max_rate_mbps: int | None = Field(
        default=None,
        description="SATA / USB: the most the device (SATA: and its port) supports; None when not known",
    )
    pcie_gen: int | None = Field(default=None, description="PCIe: the negotiated generation, 1-6")
    pcie_width: int | None = Field(default=None, description="PCIe: the negotiated lanes")
    max_pcie_gen: int | None = Field(default=None, description="PCIe: the most the device supports")
    max_pcie_width: int | None = Field(default=None, description="PCIe: the most lanes the device supports")


class DiskPart(Payload):
    """A volume's share of a disk."""

    mount_point: str
    bytes: int | None = Field(default=None, description="How much of the disk it takes; None when not known")


class DiskHealth(Payload):
    """What the disk records about itself: the NVMe SMART / Health log (log page 02h) or the ATA
    Device Statistics log (04h). Fields the disk doesn't keep are None."""

    source: Literal["nvme", "ata"]
    temperature_c: int | None = Field(default=None, description="Now, in °C")
    power_on_hours: int | None = None
    power_cycles: int | None = None
    read_bytes: int | None = Field(default=None, description="Read over the disk's life")
    written_bytes: int | None = Field(default=None, description="Written over the disk's life")
    percentage_used: int | None = Field(
        default=None,
        description="NVMe: the share of its rated endurance used, the vendor's estimate (can pass 100)",
    )
    available_spare: int | None = Field(default=None, description="NVMe: spare blocks left, %")
    spare_threshold: int | None = Field(default=None, description="NVMe: the spare % below which it warns")
    unsafe_shutdowns: int | None = None
    media_errors: int | None = Field(default=None, description="NVMe: unrecovered data integrity errors")
    critical_warning: int | None = Field(default=None, description="NVMe: critical warning bits, 0 = none")
    read_at: datetime | None = Field(
        default=None,
        description="When the record was read with administrator rights (Linux: machine.read_health); "
        "None: read just now. The temperature is always the current one.",
    )


HealthNote = Literal["", "usb", "unsupported", "needs_admin", "unavailable"]


class DiskIo(Payload):
    """Read and written since the system started (the operating system's counters)."""

    read_bytes: int
    written_bytes: int
    since: datetime | None = Field(default=None, description="When the counters started: the system's boot")


class PhysicalDisk(Payload):
    id: str = Field(
        description="The system's name for it: PhysicalDrive3 (Windows), nvme0n1 (Linux), disk2 (macOS)"
    )
    number: int | None = Field(default=None, description="Windows: the disk number of Disk Management")
    model: str = Field(description="As the device reports it; '' when it doesn't")
    vendor: str = ""
    serial: str = ""
    firmware: str = ""
    size: int | None = Field(
        default=None, description="Bytes; None while no medium is in (an empty card reader)"
    )
    media: MediaName = "unknown"
    interface: DiskInterface = "unknown"
    removable: bool = False
    trim: bool | None = Field(
        default=None, description="Whether the disk takes TRIM / UNMAP; None when not known"
    )
    link: DiskLink | None = Field(default=None, description="The disk's own link: PCIe (NVMe), SATA or USB")
    controller_link: DiskLink | None = Field(
        default=None,
        description="The PCIe link of the controller a SATA disk hangs on, when the system tells it: an "
        "add-in card's lanes can be slower than the disk's SATA link",
    )
    system: bool = Field(default=False, description="Holds the system volume")
    parts: list[DiskPart] = Field(
        default_factory=list, description="The volumes on it, in disk order when known"
    )
    health: DiskHealth | None = Field(default=None, description="What the disk records about itself")
    health_note: HealthNote = Field(
        default="",
        description="Why `health` is missing or partial: behind a USB bridge that doesn't pass it on, the "
        "disk keeps no such log, the system needs administrator rights for it, or it has no way to it",
    )
    io: DiskIo | None = Field(default=None, description="Read and written since the system started")


class FolderStat(Payload):
    name: str
    bytes: int


class VolumeScan(Payload):
    """What SpaceKnife knows of a volume from a scan of its root."""

    source: Literal["scan", "snapshot"] = Field(
        description="A scan open in this backend, or the newest snapshot"
    )
    scan_id: str | None = Field(default=None, description="The open scan (its tab can be shown)")
    snapshot: str | None = Field(default=None, description="The snapshot file")
    running: bool = False
    at: datetime | None = Field(
        default=None, description="When the scan finished (or started, while it runs)"
    )
    files: int = 0
    dirs: int | None = None
    bytes_alloc: int = 0
    method: Literal["hard", "mft"] | None = None
    classes: list[ClassStat] = Field(default_factory=list, description="Open scans: the largest file classes")
    folders: list[FolderStat] = Field(
        default_factory=list, description="Open scans: the largest top-level folders"
    )


class SnapshotPoint(Payload):
    """A complete snapshot of a whole volume: how much it held then."""

    at: datetime
    bytes_alloc: int
    files: int


class VolumeOverview(Payload):
    volume: VolumeInfo
    disks: list[str] = Field(
        default_factory=list, description="Ids of the disks it lives on ([]: none known)"
    )
    scan: VolumeScan | None = None
    history: list[SnapshotPoint] = Field(
        default_factory=list,
        description="The complete snapshots of the whole volume, newest first (at most 12): how it grew",
    )


class MachineDisks(Payload):
    disks: list[PhysicalDisk]
    volumes: list[VolumeOverview]
    collected_at: datetime


class ReadHealthParams(Payload):
    disks: list[str] = Field(
        default_factory=list,
        description="Ids of the disks to read; []: every disk whose record needs administrator rights",
    )


class ReadHealthResult(Payload):
    """``read``: the records were read (a disk that did not answer says why in its health_note);
    ``denied``: the user closed the password prompt; ``unavailable``: no way to ask for administrator
    rights here (``message`` says why); ``nothing``: no disk needed it."""

    outcome: Literal["read", "denied", "unavailable", "nothing"]
    message: str = ""
    machine: MachineDisks


__all__ = [
    "DiskHealth",
    "DiskInterface",
    "DiskIo",
    "DiskLink",
    "DiskPart",
    "FolderStat",
    "HealthNote",
    "LinkKind",
    "MachineDisks",
    "PhysicalDisk",
    "ReadHealthParams",
    "ReadHealthResult",
    "SnapshotPoint",
    "VolumeOverview",
    "VolumeScan",
]

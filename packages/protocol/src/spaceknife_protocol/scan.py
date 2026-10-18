"""Scanning, tree queries, volumes and snapshots."""

from __future__ import annotations

from datetime import datetime
from typing import Literal

from pydantic import Field

from .base import Params, Payload
from .io import IoStatus, LoadLevel

ScanStateName = Literal["queued", "scanning", "done", "cancelled", "failed"]
NodeKindName = Literal["file", "dir", "link", "other"]
ScanMethod = Literal["auto", "mft", "hard"]
ScanMethodUsed = Literal["hard", "mft", "snapshot"]

TOPIC_SCAN = "scan"


# --------------------------------------------------------------------------- volumes


MediaName = Literal[
    "nvme_ssd",
    "sata_ssd",
    "ssd",
    "hdd",
    "usb_hdd",
    "usb_ssd",
    "usb_flash",
    "sd_card",
    "network",
    "virtual",
    "unknown",
]


class VolumeInfo(Payload):
    mount_point: str
    device: str
    fs_type: str
    label: str
    total: int
    free: int
    used: int
    cluster_size: int
    is_remote: bool
    is_removable: bool
    serial: str = ""
    media: MediaName = Field(
        default="unknown", description="Storage media (start page cards, IO tuning in M4)"
    )
    is_system: bool = Field(
        default=False, description="The system volume (selected by default on the start page)"
    )
    remote_path: str = Field(
        default="", description="Network volumes: the server path (\\\\server\\share, host:/export)"
    )
    reason: str = Field(default="", description="Why the volume cannot be scanned now ('' = available)")


class VolumeListResult(Payload):
    volumes: list[VolumeInfo]
    elevated: bool = Field(
        default=False,
        description="An MFT scan needs no administrator prompt now (the backend has administrator rights, "
        "or the read-only helper is running)",
    )


class PathCheckParams(Params):
    paths: list[str] = Field(max_length=16, description="Paths as typed; blank entries are ignored")


class PathCheck(Payload):
    path: str = Field(description="As typed")
    normalized: str = Field(description="Absolute, normalised")
    state: Literal["ok", "missing", "not_dir", "denied", "error"]
    message: str = Field(default="", description="What is wrong, ready to show")
    volume: str = Field(default="", description="Mount point of the volume holding the path")
    merged_into: str | None = Field(default=None, description="Inside (or equal to) another path of the list")


class PathCheckResult(Payload):
    items: list[PathCheck]
    roots: list[str] = Field(description="The valid paths to scan, nested ones merged into their ancestors")


# --------------------------------------------------------------------------- scans


class ScanStartParams(Params):
    roots: list[str] = Field(min_length=1, max_length=16, description="Absolute paths; drive roots allowed")
    cross_mounts: bool | None = Field(
        default=None, description="Descend into other filesystems mounted below a root; None = the setting"
    )
    workers: int | None = Field(
        default=None, ge=0, le=64, description="0 = scan in-process (no pool); None = the concurrency setting"
    )
    auto_snapshot: bool | None = Field(
        default=None, description="Save a snapshot automatically when the scan completes; None = the setting"
    )
    method: ScanMethod | None = Field(
        default=None,
        description="auto and mft: the MFT fast scan for a whole local NTFS volume (the read-only helper "
        "asks for administrator rights once, unless the scan.elevate setting is off), else a hard scan; "
        "None = the setting",
    )
    load_level: LoadLevel | None = Field(default=None, description="Disk load limit; None = the setting")
    load_custom: int | None = Field(default=None, ge=5, le=100, description="Percent for load_level=custom")
    ads: bool | None = Field(
        default=None, description="Count NTFS alternate data streams (MFT scans); None = the setting"
    )


class ScanSetLoadParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    level: LoadLevel
    custom: int | None = Field(default=None, ge=5, le=100, description="Percent for level=custom")
    concurrency: int | None = Field(
        default=None, ge=1, le=64, description="Fixed parallelism; None = automatic (the governor's)"
    )


class ScanRootInfo(Payload):
    path: str
    node: int
    whole_volume: bool
    volume: VolumeInfo | None = None


class ScanStatus(Payload):
    scan_id: str
    state: ScanStateName
    roots: list[ScanRootInfo]
    items: int
    files: int
    dirs: int
    bytes_alloc: int
    bytes_size: int
    errors: int
    pending_dirs: int
    current_path: str
    elapsed_s: float
    rate: float = Field(description="Items per second, averaged over the last few seconds")
    byte_rate: float = Field(default=0.0, description="Allocated bytes per second, same window")
    progress: float | None = Field(
        description="0..1 against the volume's used space, or against the item count of the last scan "
        "of the same roots; None if unknown"
    )
    remaining_s: float | None = Field(
        default=None, description="Estimated seconds left; None while estimating"
    )
    expected_items: int | None = Field(default=None, description="Items of the last scan of the same roots")
    started_at: datetime | None = None
    finished_at: datetime | None = None
    denied: list[str] = Field(
        default_factory=list, description="Directories that could not be listed (capped)"
    )
    message: str = ""
    free: int | None = Field(default=None, description="Free bytes (whole-volume scans only)")
    unknown: int | None = Field(
        default=None, description="Used but not scanned bytes (whole-volume scans only)"
    )
    snapshot: str | None = Field(
        default=None, description="Path of the snapshot this scan was saved to/loaded from"
    )
    source: Literal["scan", "snapshot"] = "scan"
    method: ScanMethodUsed = Field(default="hard", description="How the tree was (or is being) read")
    phase: str = Field(
        default="",
        description="What a fast scan is doing: helper (starting the read-only helper, may wait for the "
        "administrator prompt), mft (reading and parsing the $MFT), receive, build; empty otherwise",
    )
    fallback: str = Field(default="", description="Why an MFT scan fell back to a hard scan (shown once)")
    mft_available: bool = Field(
        default=False, description="The target is a whole local NTFS volume: scan.use_mft can switch to MFT"
    )
    paused: bool = False
    io: IoStatus | None = Field(default=None, description="Load state of the disk being scanned (W2)")


class ScanIdParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)


class ScanListResult(Payload):
    scans: list[ScanStatus]


class ScanTopicParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)


class ScanProgressEvent(Payload):
    """``scan.progress``: periodic status of a scan (≤ 4 Hz), plus a final one when it ends."""

    sub: str
    status: ScanStatus


# --------------------------------------------------------------------------- tree


class NodeInfo(Payload):
    id: int
    parent: int
    name: str
    kind: NodeKindName
    depth: int
    size: int = Field(description="Logical bytes (subtree total for directories)")
    alloc: int = Field(description="Allocated bytes on disk (subtree total for directories)")
    files: int = 0
    dirs: int = 0
    child_count: int = 0
    mtime: int = 0
    ctime: int = 0
    atime: int = 0
    ext: str = ""
    flags: list[str] = Field(default_factory=list)


class TreeNodeParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    node: int = Field(ge=0)


class TreeNodeResult(Payload):
    node: NodeInfo
    path: str
    on_disk: bool = Field(
        default=True,
        description="False inside the MFT pseudo folders ($NTFS 元数据, $孤立文件): no such path exists",
    )


class TreeChildrenParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    node: int = Field(ge=0)
    sort: Literal["size", "name"] = "size"
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=200, ge=1, le=5000)


class TreeChildrenResult(Payload):
    parent: NodeInfo
    total: int
    items: list[NodeInfo]


class TreeSummaryParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    node: int = Field(ge=0)
    top: int = Field(default=5, ge=0, le=100, description="How many of the largest files to return")


class TopFile(Payload):
    node: NodeInfo
    rel_parent: str = Field(
        description="Folder of the file relative to the summarised node, ending in a separator"
    )
    tag: Literal["red", "yellow", "green", "blue"] | None = None


class ClassStat(Payload):
    class_id: str
    bytes: int
    files: int


class TreeSummaryResult(Payload):
    """Everything the details panel shows for one node (UI-MAIN-35…43, G11)."""

    node: NodeInfo
    path: str
    parent: NodeInfo | None = None
    latest_mtime: int = Field(description="Newest modification time in the subtree (0 = unknown)")
    top_files: list[TopFile]
    classes: list[ClassStat] = Field(description="Allocated bytes and files per file class, largest first")
    tag: Literal["red", "yellow", "green", "blue"] | None = None
    volume: VolumeInfo | None = Field(
        default=None, description="The volume of the scan root that holds the node"
    )
    on_disk: bool = Field(
        default=True,
        description="False inside the MFT pseudo folders ($NTFS 元数据, $孤立文件): no such path exists",
    )


class TreeFindParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    path: str = Field(min_length=1, max_length=32767)


class TreeFindResult(Payload):
    node: NodeInfo | None = None


# --------------------------------------------------------------------------- snapshots


class SnapshotInfo(Payload):
    path: str
    created_at: datetime
    roots: list[str]
    node_count: int
    bytes_alloc: int = 0
    files: int = 0
    file_bytes: int = Field(default=0, description="Size of the snapshot file itself")
    method: Literal["hard", "mft"] | None = Field(
        default=None, description="How the saved tree was read (None: saved before M4, a hard scan)"
    )


class SnapshotListResult(Payload):
    snapshots: list[SnapshotInfo]


class SnapshotSaveParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    path: str | None = Field(default=None, description="Target file; default = the auto-save folder")


class SnapshotLoadParams(Params):
    path: str = Field(min_length=1, max_length=32767)

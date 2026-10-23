"""Duplicate files (capability ``dupes.v1``, docs/dupes.md, docs/protocol.md §dupes).

A job compares the files of one live scan, or (``cross_volume``) of every open, finished live scan.
Files are "identical" only after their whole contents were read and found equal (BLAKE3-256), or
matched entries of the hash cache that were written that way. ``dupes.plan`` reads nothing;
``dupes.start`` runs the job in the background (``dupes.progress`` at most four times a second,
``dupes.changed`` when results change). Views coloured ``dupes`` get two frame columns: ``dup``
(u1) and ``dupg`` (u4, group id + 1).
"""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload

DupesMode = Literal["all", "inside", "against", "file"]
DupesState = Literal[
    "planning", "identifying", "probing", "verifying", "waiting_scan", "paused", "done", "cancelled", "failed"
]
DupesLoad = Literal["idle", "quiet", "balanced", "max", "custom"]
DupeRole = Literal["keep", "extra", "alias", "clone", "needed", "other_volume", "pinned"]
DupeKeepWhy = Literal["rule", "preferred", "folder", "volume", "needed", "manual", "pinned"]
DupeKeepRule = Literal["oldest", "newest", "shortest"]
DupesSort = Literal["reclaim", "size", "copies", "path"]
DupeNodeState = Literal["none", "pending", "keep", "extra", "shared", "skipped"]

# frame column `dup` (files: state; folders, blocks: tier of the share of extra copies)
DUP_NONE, DUP_PENDING, DUP_KEEP, DUP_EXTRA, DUP_SHARED, DUP_SKIPPED = 0, 1, 2, 3, 4, 5

_ID = Field(min_length=1, max_length=64)


class DupesScope(Params):
    scan_ids: list[str] = Field(min_length=1, max_length=16, description="Live scans compared together")
    mode: DupesMode = "all"
    node: int | None = Field(
        default=None, ge=1, description="inside / against: a folder; file: a file (of scan_ids[0])"
    )
    cross_volume: bool = Field(default=False, description="Compare across every given scan")
    min_size: int = Field(default=1 << 20, ge=1)
    include_system: bool = False
    include_network: bool = False


class DupesPlanDisk(Payload):
    disk: str
    media: str = "unknown"
    scans: list[str] = Field(default_factory=list)


class DupesLast(Payload):
    at: str
    groups: int
    files: int
    reclaim: int


class DupesPlan(Payload):
    candidates: int
    groups: int
    bytes: int = Field(description="Bytes of every candidate (the most a job could read)")
    probe_bytes: int = Field(description="What reading heads and tails costs")
    full_bytes_max: int
    reclaim_max: int = Field(description="The most that could be freed, if every candidate had a twin")
    excluded: dict[str, int] = Field(description="small, cloud, system, network, bins, unique")
    cross_extra: dict[str, int] | None = Field(
        default=None, description="Cross-volume: files and bytes it adds"
    )
    last: DupesLast | None = None


class DupesStartParams(Params):
    scope: DupesScope
    load: DupesLoad | None = None
    load_custom: float | None = Field(default=None, ge=1, le=100)
    budget_bytes: int | None = Field(default=None, ge=0)
    max_seconds: float | None = Field(default=None, gt=0)
    replace: bool = Field(default=False, description="Stop the running job and start this one")


class DupesJobParams(Params):
    job_id: str = _ID


class DupesLoadParams(Params):
    job_id: str = _ID
    level: DupesLoad
    custom: float | None = Field(default=None, ge=1, le=100)


class DupesFunnel(Payload):
    candidates: int = 0
    identified: int = 0
    physical: int = 0
    aliases: int = 0
    clones: int = 0
    probed: int = 0
    verifying: int = 0
    confirmed_groups: int = 0
    confirmed_files: int = 0
    shared_groups: int = 0
    unique: int = 0
    skipped: int = 0


class DupesBytes(Payload):
    read: int = 0
    probe: int = 0
    full: int = 0
    planned_probe: int = 0
    full_upper: int = 0
    cache_hits: int = 0


class DupesLedgerVolume(Payload):
    volume: str
    extra: int = Field(default=0, description="Extra copies (files)")
    reclaim: int = Field(default=0, description="Bytes freed by deleting the extra copies")
    shared: int = Field(
        default=0, description="Bytes of names and clones of one file: deleting frees nothing"
    )
    groups: int = 0
    note: str = ""


class DupesStatus(Payload):
    job_id: str
    state: DupesState
    scan_ids: list[str]
    mode: DupesMode = "all"
    cross_volume: bool = False
    progress: float | None = Field(default=None, description="Share of the groups decided, 0–1")
    funnel: DupesFunnel = Field(default_factory=DupesFunnel)
    bytes: DupesBytes = Field(default_factory=DupesBytes)
    rate: float = Field(default=0, description="Bytes read per second, lately")
    eta_max_s: float | None = Field(
        default=None, description="At most this long still (reading every candidate)"
    )
    load: DupesLoad = "quiet"
    waiting_for: str = Field(default="", description="A scan of this disk runs: the job waits")
    ledger: list[DupesLedgerVolume] = Field(default_factory=list)
    skipped_by_reason: dict[str, int] = Field(default_factory=dict)
    atime_unkept: int = 0
    atime_moved_by_others: int = 0
    groups: int = Field(default=0, description="Verified groups with something to free")
    extra: int = 0
    reclaim: int = 0
    version: int = 0
    error: str = ""
    elapsed_s: float = 0


class DupeMember(Payload):
    scan_id: str
    node: int
    path: str
    volume: str = ""
    mtime: int = 0
    ctime: int = 0
    alloc: int = 0
    role: DupeRole
    why: DupeKeepWhy | None = None


class DupeGroup(Payload):
    id: int
    size: int
    copies: int = Field(description="Physical files with this content")
    names: int = Field(description="Distinct names among them")
    reclaim: int
    shared: int = 0
    state: Literal["confirmed", "shared"]
    volumes: list[str] = Field(default_factory=list)
    cross: bool = False
    keep_why: DupeKeepWhy | None = None
    members: list[DupeMember] = Field(description="The first few (DupesGroupParams pages through all)")
    total_members: int = 0


class DupesGroupsParams(Params):
    job_id: str = _ID
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=100, ge=1, le=500)
    sort: DupesSort = "reclaim"
    cross_only: bool = False
    include_shared: bool = False
    scan_id: str | None = None
    under: int | None = Field(
        default=None, ge=1, description="Only groups with a copy below this folder (of scan_id)"
    )


class DupesGroupsPage(Payload):
    version: int
    total: int
    items: list[DupeGroup]


class DupesGroupParams(Params):
    job_id: str = _ID
    group: int = Field(ge=0)
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=200, ge=1, le=2000)


class DupesFolderParams(Params):
    job_id: str = _ID
    scan_id: str = _ID
    node: int = Field(ge=1)


class DupesOnlyHere(Payload):
    node: int
    path: str
    alloc: int


class DupesFolderInfo(Payload):
    """What of a folder exists elsewhere (§8.4): for the user to judge, never a delete."""

    node: int
    redundant: int = Field(description="Bytes of extra copies below it")
    groups: int = 0
    only_here: int = 0
    only_here_bytes: int = 0
    elsewhere: int = 0
    elsewhere_bytes: int = 0
    unchecked: int = 0
    unchecked_bytes: int = 0
    same_data: int = 0
    same_data_bytes: int = 0
    small: int = 0
    small_bytes: int = 0
    only_here_list: list[DupesOnlyHere] = Field(default_factory=list)


class DupesFoldersParams(Params):
    job_id: str = _ID
    scan_id: str = _ID
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=100, ge=1, le=500)


class DupesFolderRow(Payload):
    node: int
    path: str
    redundant: int
    alloc: int


class DupesFoldersPage(Payload):
    version: int
    total: int
    items: list[DupesFolderRow]


class DupesLocateParams(Params):
    job_id: str = _ID
    view_id: str = _ID
    group: int = Field(ge=0)
    limit: int = Field(default=2000, ge=1, le=2000)


class DupesLocateRow(Payload):
    row: int
    exact: bool = Field(description="False: the copy is inside this folder or block")
    role: DupeRole


class DupesLocateResult(Payload):
    seq: int = Field(description="Frame sequence the rows belong to")
    rows: list[DupesLocateRow]
    truncated: bool = False


class DupesOverride(Payload):
    group: int = Field(ge=0)
    path: str
    role: Literal["keep", "extra"]


class DupesKeepParams(Params):
    job_id: str = _ID
    rule: DupeKeepRule = "oldest"
    prefer: list[str] = Field(default_factory=list, max_length=64)
    folder_consistent: bool = True
    per_volume: bool = True
    overrides: list[DupesOverride] = Field(default_factory=list, max_length=10000)


class DupesBasketParams(Params):
    job_id: str = _ID
    groups: list[int] | None = Field(default=None, description="Only these groups (default: every group)")


class DupesRefused(Payload):
    group: int
    reason: str


class DupesBasketResult(Payload):
    items: list[str] = Field(description="Ids of the basket items made (one per volume)")
    groups: int
    members: int
    freed: int
    refused: list[DupesRefused] = Field(default_factory=list)


class DupeNodeInfo(Payload):
    """A node's place in the results (``tree.node`` with ``dupes.v1``)."""

    state: DupeNodeState
    group: int | None = None
    copies: int = 0
    role: DupeRole | None = None
    why: DupeKeepWhy | None = None
    reclaim: int = 0
    others: list[DupeMember] = Field(default_factory=list, description="Up to three other copies")
    other_volumes: list[str] = Field(default_factory=list)
    redundant: int = Field(default=0, description="Folders: bytes of extra copies below it")
    reason: str = ""


class DupesChangedEvent(Payload):
    job_id: str
    version: int
    reason: Literal["verified", "keep", "invalidated", "cleared"]

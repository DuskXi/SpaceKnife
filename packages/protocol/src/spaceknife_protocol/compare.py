"""Snapshot comparison (M6 W6, docs/manage.md §3).

``compare.open`` merges two trees (a snapshot and the tab's live tree, or two snapshots) in the
background; ``compare.progress`` events follow and the last one carries :class:`CompareInfo`. The
result behaves like a scan: ``view.open {scan_id: compare_id}`` gives a treemap whose areas are the
changes (|target − base|), with two extra frame columns, ``delta`` (signed net change, i8) and
``dstat`` (u1: 0 unchanged, 1 new, 2 deleted, 3 changed).
"""

from __future__ import annotations

from datetime import datetime
from typing import Literal

from pydantic import Field

from .base import Params, Payload
from .scan import SnapshotInfo

CompareDirection = Literal["all", "grow", "shrink"]
ChangeStatus = Literal["same", "new", "deleted", "changed"]
ChangeKind = Literal["file", "dir", "link", "other", "group"]
ComparePhase = Literal["loading", "comparing", "ready", "failed", "cancelled"]

NOW = "now"
DSTAT_SAME, DSTAT_NEW, DSTAT_DELETED, DSTAT_CHANGED = 0, 1, 2, 3

_ID = Field(min_length=1, max_length=64)


class CompareSide(Payload):
    """One end of a comparison: a snapshot file, or the tab's live tree ("now")."""

    kind: Literal["snapshot", "now"]
    path: str | None = Field(default=None, description="The snapshot file (kind 'snapshot')")
    created_at: datetime = Field(description="When the snapshot was taken; for 'now' the moment compared")
    bytes_alloc: int = 0
    node_count: int = 0


class CompareSourcesParams(Params):
    scan_id: str = _ID


class CompareSourcesResult(Payload):
    snapshots: list[SnapshotInfo] = Field(description="Snapshots of the same scan roots, newest first")
    now: CompareSide | None = Field(default=None, description="The live tree, when it can be compared")
    now_note: str = Field(default="", description="Why the live tree cannot be compared yet")


class CompareOpenParams(Params):
    scan_id: str = Field(min_length=1, max_length=64, description="The tab's scan (its roots, its live tree)")
    base: str = Field(min_length=1, max_length=32767, description="A snapshot file, or 'now'")
    target: str = Field(default=NOW, min_length=1, max_length=32767, description="Same as base")


class CompareTask(Payload):
    compare_id: str


class CompareIdParams(Params):
    compare_id: str = _ID


class CompareInfo(Payload):
    compare_id: str
    scan_id: str
    base: CompareSide
    target: CompareSide
    grow: int = Field(description="Allocated bytes added, every file")
    shrink: int = Field(description="Allocated bytes gone, every file (≥ 0)")
    changed: int = Field(description="Items that changed (files and folders)")
    nodes: int = Field(description="Rows of the change tree")
    live: bool = Field(default=False, description="The target is 'now' and follows file changes")
    revision: int = Field(default=0, description="Bumped each time live changes recompute it")


class CompareProgressEvent(Payload):
    compare_id: str
    phase: ComparePhase
    progress: float | None = None
    message: str = ""
    info: CompareInfo | None = Field(default=None, description="Present once ready")


class ChangeItem(Payload):
    """A changed item: a file, a folder, or an aggregate of small items ('group')."""

    node: int = Field(description="Node of the change tree (kind 'group': the folder holding them)")
    path: str
    label: str = Field(description="A short, recognisable name: 「WeChat Files\\…\\Video」")
    kind: ChangeKind
    delta: int = Field(description="Net change of allocated bytes (target − base)")
    base: int = Field(description="Allocated bytes in the base (0 when new)")
    target: int = Field(description="Allocated bytes in the target (0 when deleted)")
    status: ChangeStatus
    new_files: int = Field(default=0, description="Files below it that are new")
    members: int = Field(default=0, description="Kind 'group': changed small items in it")
    reason: str = Field(default="", description="Known cause (insights rules), e.g. 「游戏更新」")
    on_disk: bool = Field(default=True, description="Still on disk (file operations allowed)")


class CompareSummaryParams(Params):
    view_id: str = _ID
    limit: int = Field(default=12, ge=1, le=500)


class CompareSummary(Payload):
    """The sidebar of a compare view, for its zoom root and filter (UI-W6-24…31)."""

    view_id: str
    compare_id: str
    scope: str = Field(description="Path of the zoom root the numbers cover")
    whole: bool = Field(description="The scope is the whole comparison")
    base: CompareSide
    target: CompareSide
    span_s: float = Field(description="target time − base time (negative after a swap)")
    grow: int
    shrink: int
    net: int
    free: int | None = Field(default=None, description="Free bytes of the volume now (live target)")
    daily: float | None = Field(default=None, description="Net bytes per day, when it grows")
    days_to_full: float | None = Field(default=None, description="free ÷ daily (an estimate)")
    grow_items: list[ChangeItem]
    shrink_items: list[ChangeItem]
    grow_count: int = Field(description="Change units that grew (the list shows the first ones)")
    shrink_count: int


class CompareNodeParams(Params):
    view_id: str = _ID
    key: int = Field(description="A block of the view's last frame")
    children: int = Field(default=4, ge=0, le=50)


class CompareNodeResult(Payload):
    """The hover card of a compare view (UI-W6-20…22)."""

    item: ChangeItem
    children: list[ChangeItem] = Field(default_factory=list, description="Its children that changed most")

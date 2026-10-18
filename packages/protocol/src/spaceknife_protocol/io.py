"""Disk load: the per-disk I/O governor's state, the W2 load curve and the ``io`` topic."""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload

TOPIC_IO = "io"

LoadLevel = Literal["max", "balanced", "quiet", "idle", "custom"]


class LoadPoint(Payload):
    """One second of the W2 load curve."""

    t: float = Field(description="Unix time of the end of this second")
    self_pct: float = Field(description="SpaceKnife's own share of the disk load, %")
    total_pct: float = Field(description="Whole-disk load, %")
    active_pct: float = Field(default=0.0, description="Task Manager 'active time' of the whole disk, %")
    yielding: bool = False
    paused: bool = False


class IoStatus(Payload):
    """Disk-load state of one physical disk (W2 curve, status bar UI-MAIN-50)."""

    disk: str
    media: str
    level: LoadLevel
    limit: float = Field(description="Limit on SpaceKnife's own load, % (100 = not limited)")
    self_pct: float
    total_pct: float
    others_pct: float
    active_pct: float = Field(description="Raw busy time (Task Manager 'active time'), %")
    concurrency: int = Field(description="Allowed parallel batches")
    inflight: int = Field(description="Batches in flight now")
    duty: float = Field(description="Share of the allowed parallelism in use, 0..1")
    auto: bool
    yielding: bool
    paused: bool
    measured: bool = Field(description="False: no load data for this disk (open loop)")
    low_io_priority: bool = Field(description="Workers should read at OS low I/O priority")
    q_eff: float | None = None
    history: list[LoadPoint] = Field(default_factory=list)


class IoStatusList(Payload):
    disks: list[IoStatus]


class IoTopicParams(Params):
    disk: str | None = Field(default=None, max_length=256, description="One disk id; None = every disk")


class IoStatusEvent(Payload):
    """``io.load``: a disk's load state, twice a second while someone listens."""

    sub: str
    status: IoStatus


__all__ = [
    "TOPIC_IO",
    "IoStatus",
    "IoStatusEvent",
    "IoStatusList",
    "IoTopicParams",
    "LoadLevel",
    "LoadPoint",
]

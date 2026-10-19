"""System-level methods: info, ping, shutdown."""

from __future__ import annotations

from datetime import datetime

from pydantic import Field

from .base import Params, Payload
from .session import ServerInfo


class PingParams(Params):
    payload: str | None = Field(default=None, max_length=1024)


class PingResult(Payload):
    payload: str | None = None
    server_time: datetime


class PlatformInfo(Payload):
    system: str
    release: str
    machine: str


class SystemInfo(Payload):
    server: ServerInfo
    pid: int
    started_at: datetime
    platform: PlatformInfo
    sessions: int
    full_disk_access: bool | None = Field(
        default=None,
        description="macOS: whether SpaceKnife has Full Disk Access (no consent prompt per protected "
        "place); None elsewhere or when it cannot be told",
    )


class ShutdownResult(Payload):
    accepted: bool

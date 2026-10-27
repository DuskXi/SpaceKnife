"""What the 「本机磁盘」 page knows of each volume from its last complete scan, kept across page
switches, closed tabs and restarts (UI-W9-13).

One JSON file in the data directory, one entry per volume: the scan's time, counts, method and
snapshot, every file class with its size, and the largest top-level folders. An entry is written
when a scan of the whole volume finishes, before its tab closes, and whenever the page reads an
open complete scan; a newer scan of the same volume replaces it. An entry stands only for the
volume it was taken of: its mount point and identity (the volume serial, else its file system and
size) must both match, so another drive on the same letter never shows it. At most ``KEEP``
entries; one whose volume hasn't been seen for ``FORGET`` goes (an unplugged drive keeps its
entry until then). Writes go through a temporary file and an atomic rename.
"""

from __future__ import annotations

import os
import threading
from collections.abc import Iterable
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Literal

from loguru import logger
from pydantic import BaseModel, ConfigDict, Field, ValidationError

from spaceknife_protocol import machine as m
from spaceknife_protocol.scan import ClassStat

from .. import paths
from ..scan.volume import VolumeInfo

VERSION = 1
KEEP = 64
FORGET = timedelta(days=90)
SEEN_EVERY = timedelta(days=1)  # "seen" is refreshed at most this often (no write per page read)


class Kept(BaseModel):
    """One volume's summary from its last complete scan."""

    model_config = ConfigDict(frozen=True, extra="ignore")

    mount: str
    identity: str
    at: datetime  # when the scan finished
    seen: datetime  # when the volume was last present
    files: int = 0
    dirs: int | None = None
    bytes_alloc: int = 0
    method: Literal["hard", "mft"] | None = None
    snapshot: str | None = None
    classes: list[ClassStat] = Field(default_factory=list)  # all of them, largest first
    folders: list[m.FolderStat] = Field(default_factory=list)

    def volume_scan(self, top: int) -> m.VolumeScan:
        """What the page shows of it (the snapshot only while its file is still there)."""
        snap = self.snapshot if self.snapshot and os.path.isfile(self.snapshot) else None
        return m.VolumeScan(
            source="kept",
            snapshot=snap,
            at=self.at,
            files=self.files,
            dirs=self.dirs,
            bytes_alloc=self.bytes_alloc,
            method=self.method,
            classes=self.classes[:top],
            folders=self.folders,
        )


class _File(BaseModel):
    version: int
    volumes: list[Kept]


def identity(v: VolumeInfo) -> str:
    """Which volume it is, beyond its mount point."""
    return f"serial:{v.serial}" if v.serial else f"{v.fs_type.lower()}:{v.total}"


def _key(mount: str, ident: str) -> str:
    return f"{os.path.normcase(os.path.normpath(mount))}|{ident}"


class VolumeSummaries:
    def __init__(self, file: Path | None = None) -> None:
        self.file = file or paths.data_dir() / "volume_summaries.json"
        self._lock = threading.Lock()
        self._entries: dict[str, Kept] = {}
        self._load()

    def _load(self) -> None:
        try:
            data = _File.model_validate_json(self.file.read_bytes())
        except FileNotFoundError:
            return
        except (OSError, ValueError, ValidationError) as exc:
            logger.warning("volume summaries unreadable, starting over: {}", exc)
            return
        if data.version == VERSION:
            self._entries = {_key(e.mount, e.identity): e for e in data.volumes}

    def _save(self) -> None:
        payload = _File(version=VERSION, volumes=sorted(self._entries.values(), key=lambda e: e.mount))
        try:
            self.file.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.file.with_suffix(".tmp")
            tmp.write_text(payload.model_dump_json(), encoding="utf-8")
            os.replace(tmp, self.file)
        except OSError as exc:  # a summary is a convenience: never fail the page for it
            logger.warning("could not save volume summaries: {}", exc)

    def _prune(self, now: datetime) -> None:
        live = [e for e in self._entries.values() if now - e.seen <= FORGET]
        live.sort(key=lambda e: e.seen, reverse=True)
        self._entries = {_key(e.mount, e.identity): e for e in live[:KEEP]}

    def get(self, v: VolumeInfo) -> Kept | None:
        with self._lock:
            return self._entries.get(_key(v.mount_point, identity(v)))

    def put(self, entries: Iterable[Kept]) -> None:
        """Keep these, each unless an entry of a newer scan of the same volume is kept already."""
        with self._lock:
            changed = False
            for e in entries:
                key = _key(e.mount, e.identity)
                old = self._entries.get(key)
                if old is not None and (old.at > e.at or old.model_copy(update={"seen": e.seen}) == e):
                    continue
                self._entries[key] = e
                changed = True
            if changed:
                self._prune(datetime.now(UTC))
                self._save()

    def seen(self, volumes: Iterable[VolumeInfo], now: datetime | None = None) -> None:
        """These volumes are present: their entries stay (refreshed at most once a day)."""
        now = now or datetime.now(UTC)
        with self._lock:
            changed = False
            for v in volumes:
                key = _key(v.mount_point, identity(v))
                e = self._entries.get(key)
                if e is not None and now - e.seen >= SEEN_EVERY:
                    self._entries[key] = e.model_copy(update={"seen": now})
                    changed = True
            before = len(self._entries)
            self._prune(now)
            if changed or len(self._entries) != before:
                self._save()

    def __len__(self) -> int:
        return len(self._entries)


__all__ = ["FORGET", "KEEP", "Kept", "VolumeSummaries", "identity"]

"""``machine.disks``: the 「本机磁盘」 page; ``machine.read_health``: the disks' own records that need
administrator rights.

The disks and the volumes' place on them come from :mod:`spaceknife.disks`; what is known of each
volume comes from a scan of its root open in this backend (counts, and once it is done the largest
file classes and top-level folders) or else from the newest snapshot of it (counts only: a snapshot
is not loaded for this). The headers of the volume's complete snapshots give its history.

On Linux a disk's lifetime counters need root: ``machine.read_health`` (only on the user's click)
starts the elevated helper behind polkit's password prompt and reads the raw pages of each such
disk. The records are kept for the life of the backend, so the page keeps them on later reads;
while the elevated helper still runs (10 idle minutes), those reads ask it again, without a prompt.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
from collections.abc import Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal

import numpy as np
from loguru import logger

from spaceknife_protocol import machine as m
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import MACHINE_DISKS, MACHINE_READ_HEALTH
from spaceknife_protocol.scan import ClassStat

from .. import disks, paths
from ..disks.health import from_pages
from ..helper import HelperError, HelperUnavailable
from ..helper.errors import HelperDenied
from ..scan.scanner import ScanState
from ..scan.volume import VolumeInfo, list_volumes
from ..store import snapshot
from ..store.nodes import Kind
from .compute import Latest
from .scan_handlers import warm_media
from .scans import ScanHandle, saved_method, saved_state, summary_of, volume_model

if TYPE_CHECKING:
    from .app import Server
    from .session import Session

TOP_CLASSES = 5
TOP_FOLDERS = 3
HISTORY = 12  # snapshots of a volume for its growth (the newest 10 are kept, then weekly)


def _key(path: str) -> str:
    return os.path.normcase(os.path.normpath(path))


def _whole_root(handle: ScanHandle) -> str | None:
    """The mount point a scan covers whole (one root, the volume's), else None."""
    if len(handle.roots) != 1:
        return None
    root = handle.roots[0]
    if root.volume is not None and (root.whole_volume or _key(root.path) == _key(root.volume.mount_point)):
        return _key(root.volume.mount_point)
    return None


def _rank(handle: ScanHandle) -> tuple[bool, datetime]:
    """A scan of the disk beats a loaded snapshot; then the newest."""
    started = handle.progress().started_at or datetime.min.replace(tzinfo=UTC)
    return handle.source == "scan", started


def _live(server: Server, handle: ScanHandle) -> m.VolumeScan:
    st = handle.status()
    running = handle.running
    classes: list[ClassStat] = []
    folders: list[m.FolderStat] = []
    if not running:
        node = handle.roots[0].node
        summary = summary_of(server, handle, node, 0)
        classes = [c for c in summary.classes if c.bytes > 0][:TOP_CLASSES]
        store = handle.store
        kids = store.sorted_children(node, "size")
        dirs = kids[store.cols["kind"][kids] == Kind.DIR][:TOP_FOLDERS]
        folders = [
            m.FolderStat(name=store.name(int(i)), bytes=int(store.cols["tot_alloc"][int(i)]))
            for i in np.asarray(dirs)
        ]
    method: Literal["hard", "mft"] | None = None
    if st.method in ("hard", "mft"):
        method = "mft" if st.method == "mft" else "hard"
    return m.VolumeScan(
        source="snapshot" if handle.source == "snapshot" else "scan",
        scan_id=handle.id,
        snapshot=handle.snapshot_path,
        running=running,
        at=st.finished_at or st.started_at,
        files=st.files,
        dirs=st.dirs,
        bytes_alloc=st.bytes_alloc,
        method=method,
        classes=classes,
        folders=folders,
    )


Saved = tuple[m.VolumeScan | None, list[m.SnapshotPoint]]


def saved_scans(mount_point: str, base: Path | None = None) -> Saved:
    """The newest complete snapshot of a whole volume and the history of its complete snapshots,
    newest first (headers only; stopped scans saved by hand are left out: their counts are partial)."""
    folder = (base or paths.snapshots_dir()) / snapshot.root_key([mount_point])
    try:
        files = sorted(folder.glob(f"*{snapshot.SUFFIX}"), key=lambda f: f.name, reverse=True)
    except OSError:
        return None, []
    newest: m.VolumeScan | None = None
    history: list[m.SnapshotPoint] = []
    for file in files:
        if len(history) >= HISTORY:
            break
        try:
            header = snapshot.read_header(file)
        except (OSError, snapshot.SnapshotError, ValueError):
            continue
        stats = header.stats
        if saved_state(stats) != ScanState.DONE:
            continue
        files_n, alloc = int(stats.get("files", 0)), int(stats.get("bytes_alloc", 0))
        if newest is None:
            newest = m.VolumeScan(
                source="snapshot",
                snapshot=str(file),
                at=header.created_at,
                files=files_n,
                dirs=int(stats["dirs"]) if isinstance(stats.get("dirs"), int) else None,
                bytes_alloc=alloc,
                method=saved_method(stats),
            )
        history.append(m.SnapshotPoint(at=header.created_at, bytes_alloc=alloc, files=files_n))
    return newest, history


def overview(
    server: Server,
    volumes: Sequence[VolumeInfo],
    found: disks.Probe,
    saved: dict[str, Saved],
) -> m.MachineDisks:
    """The page's data (on the compute pool: summing an open scan's classes reads its columns)."""
    open_scans: dict[str, ScanHandle] = {}
    for candidate in list(server.scans.scans.values()):
        root = _whole_root(candidate)
        if root is None:
            continue
        best = open_scans.get(root)
        if best is None or _rank(candidate) > _rank(best):
            open_scans[root] = candidate
    out_volumes: list[m.VolumeOverview] = []
    for v in volumes:
        model = volume_model(v)
        if model is None:
            continue
        handle = open_scans.get(_key(v.mount_point))
        newest, history = saved.get(v.mount_point, (None, []))
        scan = _live(server, handle) if handle is not None else newest
        ids = [disk for disk, _size in found.parts.get(v.mount_point, [])]
        out_volumes.append(
            m.VolumeOverview(volume=model, disks=list(dict.fromkeys(ids)), scan=scan, history=history)
        )
    # each disk's volumes, in disk order (those with no known offset last)
    on_disk: dict[str, list[tuple[int, str, int | None]]] = {}
    for mount, parts in found.parts.items():
        start = found.offsets.get(mount)
        for disk, size in parts:
            on_disk.setdefault(disk, []).append((start if start is not None else 1 << 62, mount, size))
    out_disks = [
        d.model_copy(
            update={
                "parts": [
                    m.DiskPart(mount_point=mount, bytes=size)
                    for _start, mount, size in sorted(on_disk.get(d.id, []))
                ]
            }
        )
        for d in found.disks
    ]
    return m.MachineDisks(disks=out_disks, volumes=out_volumes, collected_at=datetime.now(UTC))


# --------------------------------------------------------------------------- records read as root

# (the record or None when the disk didn't answer, the note then, when it was read)
Elevated = tuple[m.DiskHealth | None, m.HealthNote, datetime]


def _record_key(d: m.PhysicalDisk) -> str:
    """A kernel name can move to another disk (hot-plug): the serial pins it."""
    return f"{d.id}|{d.serial}"


def with_record(d: m.PhysicalDisk, read: Elevated | None) -> m.PhysicalDisk:
    """The disk with what an elevated read found: the record (keeping the live temperature), or the
    reason it has none (the bridge or the disk didn't answer)."""
    if read is None:
        return d
    record, note, at = read
    if record is None:
        return d.model_copy(update={"health_note": note})
    live = d.health.temperature_c if d.health is not None else None
    merged = record.model_copy(
        update={"temperature_c": live if live is not None else record.temperature_c, "read_at": at}
    )
    return d.model_copy(update={"health": merged, "health_note": ""})


def failed_note(d: m.PhysicalDisk) -> m.HealthNote:
    return "usb" if d.interface == "usb" else "unsupported"


async def read_records(client: Any, targets: Sequence[m.PhysicalDisk], cache: dict[str, Elevated]) -> None:
    """Ask the elevated helper for each disk's pages; the cache gets the record or the reason.
    HelperUnavailable (the helper is gone) propagates; a disk that doesn't answer doesn't stop the rest."""
    for d in targets:
        at = datetime.now(UTC)
        try:
            pages = await client.disk_health(d.id)
        except HelperUnavailable:
            raise
        except HelperError as exc:
            logger.info("disk {} has no record for the helper: {}", d.id, exc.message)
            cache[_record_key(d)] = (None, failed_note(d), at)
            continue
        record = from_pages(pages)
        cache[_record_key(d)] = (record, "" if record is not None else failed_note(d), at)


def install(server: Server) -> None:
    router = server.router
    latest = Latest()
    elevated: dict[str, Elevated] = {}  # disk -> what root read; for the life of the backend

    async def probe() -> tuple[list[VolumeInfo], disks.Probe]:
        volumes = await asyncio.to_thread(list_volumes)
        await asyncio.to_thread(warm_media, volumes)
        return volumes, await asyncio.to_thread(disks.probe, volumes)

    async def page(
        key: str, session: Session, volumes: list[VolumeInfo], found: disks.Probe
    ) -> m.MachineDisks:
        found.disks = [with_record(d, elevated.get(_record_key(d))) for d in found.disks]

        def snapshots() -> dict[str, Saved]:
            return {v.mount_point: saved_scans(v.mount_point) for v in volumes if not v.reason}

        saved = await asyncio.to_thread(snapshots)
        return await latest.run((key, session.id), overview, server, volumes, found, saved)

    async def refresh(found: disks.Probe) -> None:
        """Read the kept records again while the elevated helper still runs (no prompt)."""
        client = server.scans.helper.current
        if not elevated or client is None or not client.elevated:
            return
        again = [d for d in found.disks if _record_key(d) in elevated and elevated[_record_key(d)][0]]
        with contextlib.suppress(HelperError):
            await read_records(client, again, elevated)

    @router.method(MACHINE_DISKS)
    async def machine_disks(session: Session, _: Empty) -> m.MachineDisks:
        volumes, found = await probe()
        await refresh(found)
        return await page("machine.disks", session, volumes, found)

    @router.method(MACHINE_READ_HEALTH)
    async def machine_read_health(session: Session, params: m.ReadHealthParams) -> m.ReadHealthResult:
        volumes, found = await probe()
        wanted = set(params.disks)
        known = [with_record(d, elevated.get(_record_key(d))) for d in found.disks]  # read once is enough
        targets = [d for d in known if d.health_note == "needs_admin" and (not wanted or d.id in wanted)]
        outcome: Literal["read", "denied", "unavailable", "nothing"] = "nothing"
        message = ""
        if targets:
            try:
                client = await server.scans.helper.get(elevate=True)
                await read_records(client, targets, elevated)
                outcome = "read"
            except HelperDenied as exc:
                outcome, message = "denied", exc.message
            except HelperError as exc:
                outcome, message = "unavailable", exc.message
                logger.info("disk records need administrator rights, not available: {}", exc.message)
        machine = await page("machine.read_health", session, volumes, found)  # never dropped for a refresh
        return m.ReadHealthResult(outcome=outcome, message=message, machine=machine)


__all__ = ["failed_note", "install", "overview", "read_records", "saved_scans", "with_record"]

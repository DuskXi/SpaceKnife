"""``machine.disks``: the 「本机磁盘」 page.

The disks and the volumes' place on them come from :mod:`spaceknife.disks`; what is known of each
volume comes from a scan of its root open in this backend (counts, and once it is done the largest
file classes and top-level folders) or else from the newest snapshot of it (counts only: a snapshot
is not loaded for this).
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Literal

import numpy as np

from spaceknife_protocol import machine as m
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import MACHINE_DISKS
from spaceknife_protocol.scan import ClassStat

from .. import disks, paths
from ..scan.volume import VolumeInfo, list_volumes
from ..store import snapshot
from ..store.nodes import Kind
from .compute import Latest
from .scan_handlers import warm_media
from .scans import ScanHandle, saved_method, summary_of, volume_model

if TYPE_CHECKING:
    from .app import Server
    from .session import Session

TOP_CLASSES = 5
TOP_FOLDERS = 3


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


def newest_snapshot(mount_point: str, base: Path | None = None) -> m.VolumeScan | None:
    """The newest snapshot of a whole volume (its header only)."""
    folder = (base or paths.snapshots_dir()) / snapshot.root_key([mount_point])
    try:
        files = sorted(folder.glob(f"*{snapshot.SUFFIX}"), key=lambda f: f.name, reverse=True)
    except OSError:
        return None
    for file in files:
        try:
            header = snapshot.read_header(file)
        except (OSError, snapshot.SnapshotError, ValueError):
            continue
        stats = header.stats
        return m.VolumeScan(
            source="snapshot",
            snapshot=str(file),
            at=header.created_at,
            files=int(stats.get("files", 0)),
            dirs=int(stats["dirs"]) if isinstance(stats.get("dirs"), int) else None,
            bytes_alloc=int(stats.get("bytes_alloc", 0)),
            method=saved_method(stats),
        )
    return None


def overview(
    server: Server,
    volumes: Sequence[VolumeInfo],
    found: disks.Probe,
    saved: dict[str, m.VolumeScan],
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
        scan = _live(server, handle) if handle is not None else saved.get(v.mount_point)
        ids = [disk for disk, _size in found.parts.get(v.mount_point, [])]
        out_volumes.append(m.VolumeOverview(volume=model, disks=list(dict.fromkeys(ids)), scan=scan))
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


def install(server: Server) -> None:
    router = server.router
    latest = Latest()

    @router.method(MACHINE_DISKS)
    async def machine_disks(session: Session, _: Empty) -> m.MachineDisks:
        volumes = await asyncio.to_thread(list_volumes)
        await asyncio.to_thread(warm_media, volumes)
        found = await asyncio.to_thread(disks.probe, volumes)

        def snapshots() -> dict[str, m.VolumeScan]:
            out = {}
            for v in volumes:
                if not v.reason and (s := newest_snapshot(v.mount_point)) is not None:
                    out[v.mount_point] = s
            return out

        saved = await asyncio.to_thread(snapshots)
        return await latest.run(("machine.disks", session.id), overview, server, volumes, found, saved)


__all__ = ["install", "newest_snapshot", "overview"]

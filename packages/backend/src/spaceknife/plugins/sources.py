"""Scans from a plugin's scan source (``scan.start {source, uri}``): the plugin lists, the host builds.

The tree is not on disk unless the source says so: its root is marked virtual, so revealing, opening
and the delete basket refuse its nodes, there is no monitor and no free space (thin, docs/plugins.md).
"""

from __future__ import annotations

import asyncio
import secrets
import time
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

from loguru import logger

from spaceknife_protocol import scan as p
from spaceknife_protocol.binary import decode_frame
from spaceknife_protocol.errors import ErrorCode

from ..extensions import EXTENSIONS
from ..filter.names import forget
from ..i18n import tr
from ..scan.scanner import RootInfo, ScanProgress, ScanState
from ..server.rpc import RpcError
from ..server.scans import ScanHandle
from ..store.lock import STRUCTURE
from ..store.nodes import NodeStore
from .listing import Collector, build

if TYPE_CHECKING:
    from .host import PluginHost


def _progress(
    state: ScanState, started: datetime, items: int, store: NodeStore | None = None, message: str = ""
) -> ScanProgress:
    c = store.cols if store is not None else None
    done = state != ScanState.SCANNING
    return ScanProgress(
        state=state,
        items=items,
        files=int(c["tot_files"][0]) if c is not None else 0,
        dirs=int(c["tot_dirs"][0]) if c is not None else 0,
        bytes_alloc=int(c["tot_alloc"][0]) if c is not None else 0,
        bytes_size=int(c["tot_size"][0]) if c is not None else 0,
        errors=0,
        pending_dirs=0,
        current_path="",
        elapsed_s=(datetime.now(UTC) - started).total_seconds(),
        rate=0.0,
        progress=1.0 if state == ScanState.DONE else None,
        started_at=started,
        finished_at=datetime.now(UTC) if done else None,
        message=message,
    )


async def start_source(host: PluginHost, params: p.ScanStartParams) -> ScanHandle:
    c = EXTENSIONS.get("scan_source", params.source or "")
    if c is None:
        raise RpcError(ErrorCode.INVALID_PARAMS, tr("没有这个扫描来源：{id}", id=params.source))
    host.enabled(c.owner)
    uri = str(params.uri)
    scans = host.server.scans
    scans._make_room()
    store = NodeStore()
    root = store.add_root(uri)
    started = datetime.now(UTC)
    handle = ScanHandle(
        id="scan-" + secrets.token_hex(4),
        store=store,
        roots=[RootInfo(path=uri, node=root, volume=None, whole_volume=False)],
        source="scan",
        saved_progress=_progress(ScanState.SCANNING, started, 0),
        auto_snapshot=False,
        method="source",
    )
    handle.plugin_source = c.id
    handle.uri = uri
    scans.scans[handle.id] = handle
    collector = Collector()
    last = [0.0]

    def chunk(value: Any, frames: list[bytes]) -> None:
        for f in frames:
            collector.add_batch(decode_frame(f)[1])
        now = time.monotonic()
        if now - last[0] > 0.5:
            last[0] = now
            handle.saved_progress = _progress(ScanState.SCANNING, started, collector.count)
            handle.publish()

    async def run() -> None:
        try:
            value, _ = await host.call(
                c.owner, "source", c.local, {"uri": uri}, on_chunk=chunk, timeout=600.0
            )
            name = str((value or {}).get("root") or uri)
            built = await asyncio.to_thread(build, name, collector.root, virtual=not c.spec.on_disk)
            async with STRUCTURE.write():
                handle.store.adopt(built)
                forget(handle.store)
            handle.roots = [RootInfo(path=name, node=1, volume=None, whole_volume=False)]
            handle.saved_progress = _progress(ScanState.DONE, started, len(handle.store) - 1, handle.store)
            scans._event("done", handle)
        except RpcError as exc:
            logger.warning("scan source {} failed: {}", c.id, exc.message)
            handle.saved_progress = _progress(ScanState.FAILED, started, collector.count, message=exc.message)
        except asyncio.CancelledError:
            handle.saved_progress = _progress(ScanState.CANCELLED, started, collector.count)
            raise
        finally:
            handle.publish()

    handle.task = asyncio.create_task(run(), name=f"source-{handle.id}")
    scans._event("started", handle)
    return handle


__all__ = ["start_source"]

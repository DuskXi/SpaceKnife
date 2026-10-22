"""Scans from a plugin's scan source (``scan.start {source, uri}``): the plugin lists, the host builds.

The tree is not on disk unless the source says so: its root is marked virtual, so revealing, opening
and the delete basket refuse its nodes, and there is no free space (thin, docs/plugins.md).

Such a scan has no platform watcher. Its live side is a :class:`SourceMonitor`: a rescan
(``scan.rescan``, F5) lists the whole source again and puts the new tree in place, views keeping
their place by path; when the source declares ``watch = true`` and the plugin has a ``watcher`` for
it, the watcher's changes do the same once things are quiet — up to ``RERUN_MAX_ITEMS`` items; a
bigger tree only says in the status bar that it needs a rescan.
"""

from __future__ import annotations

import asyncio
import contextlib
import secrets
import time
from collections.abc import Callable
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

import numpy as np
from loguru import logger

from spaceknife_protocol import scan as p
from spaceknife_protocol.binary import decode_frame
from spaceknife_protocol.errors import ErrorCode

from ..extensions import EXTENSIONS
from ..extensions.registry import Contribution
from ..filter.names import forget
from ..i18n import _, tr
from ..scan.scanner import RootInfo, ScanProgress, ScanState
from ..server.rpc import RpcError
from ..server.scans import ScanHandle
from ..store.lock import STRUCTURE
from ..store.nodes import NodeStore
from .listing import Collector, build, path_remap

if TYPE_CHECKING:
    from .host import PluginHost

RERUN_MAX_ITEMS = 100_000  # a watcher's changes list the source again up to this many items
QUIET_S = 1.0  # changes wait this long without new ones before the source runs again
RETRY_S = (5.0, 30.0, 120.0)  # a watcher that stopped is started again after these pauses
LIST_TIMEOUT_S = 600.0


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


def _source(source_id: str | None) -> Contribution:
    c = EXTENSIONS.get("scan_source", source_id or "")
    if c is None:
        raise RpcError(ErrorCode.INVALID_PARAMS, tr("没有这个扫描来源：{id}", id=source_id))
    return c


async def _list(
    host: PluginHost, c: Contribution, uri: str, on_count: Callable[[int], None] | None = None
) -> tuple[str, NodeStore]:
    """Run the source once: (the root's display name, the tree built from what it listed)."""
    collector = Collector()

    def chunk(value: Any, frames: list[bytes]) -> None:
        for f in frames:
            collector.add_batch(decode_frame(f)[1])
        if on_count is not None:
            on_count(collector.count)

    value, _ = await host.call(
        c.owner, "source", c.local, {"uri": uri}, on_chunk=chunk, timeout=LIST_TIMEOUT_S
    )
    name = str((value or {}).get("root") or uri)
    built = await asyncio.to_thread(build, name, collector.root, virtual=not c.spec.on_disk)
    return name, built


async def start_source(host: PluginHost, params: p.ScanStartParams) -> ScanHandle:
    c = _source(params.source)
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
    last = [0.0]
    seen = [0]

    def counted(count: int) -> None:
        seen[0] = count
        now = time.monotonic()
        if now - last[0] > 0.5:
            last[0] = now
            handle.saved_progress = _progress(ScanState.SCANNING, started, count)
            handle.publish()

    async def run() -> None:
        try:
            name, built = await _list(host, c, uri, counted)
            async with STRUCTURE.write():
                handle.store.adopt(built)
                forget(handle.store)
            handle.roots = [RootInfo(path=name, node=1, volume=None, whole_volume=False)]
            handle.saved_progress = _progress(ScanState.DONE, started, len(handle.store) - 1, handle.store)
            scans._event("done", handle)
            if scans.monitors is not None:
                await scans.monitors.start(handle)  # the plugin's watcher, when the source has one
        except RpcError as exc:
            logger.warning("scan source {} failed: {}", c.id, exc.message)
            handle.saved_progress = _progress(ScanState.FAILED, started, seen[0], message=exc.message)
        except asyncio.CancelledError:
            handle.saved_progress = _progress(ScanState.CANCELLED, started, seen[0])
            raise
        finally:
            handle.publish()

    handle.task = asyncio.create_task(run(), name=f"source-{handle.id}")
    scans._event("started", handle)
    return handle


async def relist(host: PluginHost, handle: ScanHandle) -> int:
    """List the source of ``handle`` again and put the new tree in place; returns its item count.

    Views keep their zoom, selection and history by path (a node that is gone falls back to the
    nearest folder still there); comparisons and the delete basket follow as after any change.
    """
    c = _source(handle.plugin_source)
    host.enabled(c.owner)
    uri = str(handle.uri)
    name, built = await _list(host, c, uri)
    store = handle.store
    views = host.server.views
    version = store.version
    remap = await asyncio.to_thread(path_remap, store, built)  # nothing else changes a source's tree
    async with STRUCTURE.write():
        if store.version != version:  # listed again meanwhile (two rescans at once): match again
            remap = path_remap(store, built)
        chains = views.chains(handle.id)
        store.adopt(built)
        forget(store)
        views.remap(handle.id, chains, lambda ids: _apply(remap, ids))
    handle.roots = [RootInfo(path=name, node=1, volume=None, whole_volume=False)]
    pr = handle.saved_progress
    started = pr.started_at if pr is not None and pr.started_at is not None else datetime.now(UTC)
    handle.saved_progress = _progress(ScanState.DONE, started, len(store) - 1, store)
    views.flash(handle.id, {})  # new frames; comparisons and the basket follow
    handle.publish()
    host.server.scans._event("done", handle)
    return len(store) - 1


def _apply(remap: np.ndarray, ids: np.ndarray) -> np.ndarray:
    ids = np.asarray(ids, dtype=np.int64)
    out = np.full(len(ids), -1, dtype=np.int64)
    ok = (ids >= 0) & (ids < len(remap))
    out[ok] = remap[ids[ok]]
    return out


def watcher_of(source_id: str) -> Contribution | None:
    """The ``watcher`` contribution of a source (same plugin), when the source declares ``watch``."""
    src = EXTENSIONS.get("scan_source", source_id)
    if src is None or not getattr(src.spec, "watch", False):
        return None
    local = source_id.rsplit(".", 1)[-1]
    for w in EXTENSIONS.of("watcher"):
        if w.owner == src.owner and getattr(w.spec, "source", None) in (local, source_id):
            return w
    return None


class SourceMonitor:
    """The live side of a scan from a plugin's source (``handle.monitor``): rescans and the
    plugin's watcher. Stands in for :class:`~spaceknife.server.monitors.ScanMonitor`."""

    def __init__(self, host: PluginHost, handle: ScanHandle, *, watch: bool = True) -> None:
        self.host = host
        self.handle = handle
        self.watcher = watcher_of(handle.plugin_source or "") if watch else None
        self.state: str = "starting"
        self.detail = ""
        self.events = 0
        self.changes = 0
        self.overflows = 0
        self.pending = 0
        self.last_apply_ms = 0.0
        self.reruns = 0
        self._due = False
        self._last_event = 0.0
        self._wake = asyncio.Event()
        self._tasks: list[asyncio.Task[None]] = []
        self._stopped = False

    # ------------------------------------------------------------- lifecycle (as ScanMonitor)

    async def start(self) -> None:
        self._tasks.append(asyncio.create_task(self._reruns(), name=f"source-relist-{self.handle.id}"))
        if self.watcher is not None:
            self._tasks.append(asyncio.create_task(self._watch(), name=f"source-watch-{self.handle.id}"))
        self.state = "running"

    async def stop(self) -> None:
        self._stopped = True
        for task in self._tasks:
            task.cancel()
        for task in self._tasks:
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await task
        self._tasks.clear()

    def rescan(self, path: str) -> None:
        """List the whole source again, right away (a source has no folder-level relisting)."""
        self._due = True
        self._last_event = 0.0
        self._wake.set()

    def kick(self) -> None:
        self._wake.set()

    def rebuilt(self) -> None:
        return None

    @property
    def fed(self) -> bool:
        return True

    def status(self) -> p.MonitorStatus:
        state = self.state if self.state in ("starting", "running", "degraded", "failed") else "running"
        return p.MonitorStatus(
            state=state,  # type: ignore[arg-type]
            detail=self.detail,
            watcher=self.watcher.id if self.watcher is not None else "",
            events=self.events,
            changes=self.changes,
            overflows=self.overflows,
            pending=self.pending,
            last_apply_ms=self.last_apply_ms,
        )

    # ------------------------------------------------------------- the plugin's watcher

    def _on_changes(self, value: Any, frames: list[bytes]) -> None:
        batch = value if isinstance(value, list) else []
        if not batch:
            return
        self.events += len(batch)
        self.overflows += sum(1 for c in batch if isinstance(c, dict) and c.get("kind") == "overflow")
        self._last_event = time.monotonic()
        if len(self.handle.store) - 1 > RERUN_MAX_ITEMS:
            self.state, self.detail = "degraded", _("来源的内容有变化，需要重新扫描")
            self.pending += len(batch)
            self.handle.publish()
            return
        self.pending += len(batch)
        self._due = True
        self._wake.set()

    async def _watch(self) -> None:
        assert self.watcher is not None
        w = self.watcher
        failures = 0
        while not self._stopped:
            try:
                await self.host.call(
                    w.owner, "watch", w.local, {"uri": str(self.handle.uri)}, on_chunk=self._on_changes
                )
                failures = 0  # the watcher ended by itself: start it again after a pause
            except asyncio.CancelledError:
                raise
            except RpcError as exc:
                failures += 1
                logger.bind(source=f"plugin:{w.owner}").warning("watcher {} stopped: {}", w.id, exc.message)
                if exc.code in (ErrorCode.PLUGIN_DISABLED, ErrorCode.METHOD_NOT_FOUND):
                    self.state, self.detail = (
                        "failed",
                        tr("监控器 {id} 已停止：{why}", id=w.id, why=exc.message),
                    )
                    self.handle.publish()
                    return
                self.state, self.detail = (
                    "degraded",
                    tr("监控器 {id} 已停止：{why}", id=w.id, why=exc.message),
                )
                self.handle.publish()
            await asyncio.sleep(RETRY_S[min(failures, len(RETRY_S) - 1)] if failures else RETRY_S[0])

    # ------------------------------------------------------------- listing again

    async def _reruns(self) -> None:
        while not self._stopped:
            await self._wake.wait()
            self._wake.clear()
            if not self._due:
                continue
            quiet = QUIET_S - (time.monotonic() - self._last_event)
            if quiet > 0:
                await asyncio.sleep(quiet)
                if time.monotonic() - self._last_event < QUIET_S:
                    self._wake.set()  # more changes came: wait for quiet again
                    continue
            self._due = False
            if self.handle.running:
                self._due = True
                await asyncio.sleep(QUIET_S)
                self._wake.set()
                continue
            t0 = time.perf_counter()
            waiting = self.pending
            try:
                await relist(self.host, self.handle)
            except RpcError as exc:
                logger.warning(
                    "scan source {} could not list again: {}", self.handle.plugin_source, exc.message
                )
                self.state, self.detail = "degraded", tr("无法重新读取来源：{why}", why=exc.message)
                self.handle.publish()
                continue
            self.last_apply_ms = (time.perf_counter() - t0) * 1000
            self.reruns += 1
            self.changes += max(waiting, 1)
            self.pending = max(0, self.pending - waiting)
            if self.state != "failed":
                self.state, self.detail = "running", ""
            self.handle.publish()


def make_monitor(host: PluginHost) -> Callable[[ScanHandle, bool], SourceMonitor]:
    """What the monitor manager starts for a source's scan (``watch``: live monitoring is on)."""

    def make(handle: ScanHandle, watch: bool) -> SourceMonitor:
        return SourceMonitor(host, handle, watch=watch)

    return make


__all__ = ["RERUN_MAX_ITEMS", "SourceMonitor", "make_monitor", "relist", "start_source", "watcher_of"]

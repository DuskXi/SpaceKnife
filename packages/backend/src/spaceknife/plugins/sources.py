"""Scans from a plugin's scan source (``scan.start {source, uri}``): the plugin lists, the host builds.

The tree is not on disk unless the source says so: its root is marked virtual, so revealing, opening
and the delete basket refuse its nodes, and there is no free space (thin, docs/plugins.md).

While it lists, the source's ``ctx.progress`` is the scan's progress, and what it listed so far is
built aside and put in place now and then (``_Lister``: after a second, then each time the listing
has doubled); a failure keeps its reason as the scan's message (docs/plugin-api.md 「api-source」).

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
from collections.abc import Awaitable, Callable
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
WAIT_BACK_S = 1.0  # a watcher whose plugin went off (or reloads) looks this often whether it is back
# how a restart ends a watcher's call: its process went first, or the plugin answered the host's
# cancel first (which one depends on timing: under load the answer often wins)
RESTARTED = frozenset({ErrorCode.PLUGIN_UNAVAILABLE, ErrorCode.REQUEST_CANCELLED})
LIST_TIMEOUT_S = 600.0
# What came so far is shown while a source lists: the first time after PARTIAL_FIRST_S, then each
# time the listing has doubled (at least PARTIAL_EVERY_S apart), or every PARTIAL_SLOW_S while it is
# small (a slow source: a network share). Each build costs as much as the final one; doubling keeps
# all of them together below one more final build.
PARTIAL_FIRST_S = 1.0
PARTIAL_EVERY_S = 1.0
PARTIAL_SLOW_S = 5.0
PARTIAL_SMALL = 50_000


def _progress(
    state: ScanState,
    started: datetime,
    items: int,
    store: NodeStore | None = None,
    message: str = "",
    fraction: float | None = None,
) -> ScanProgress:
    c = store.cols if store is not None and len(store) else None
    done = state != ScanState.SCANNING
    elapsed = (datetime.now(UTC) - started).total_seconds()
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
        elapsed_s=elapsed,
        rate=items / elapsed if elapsed > 0 and not done else 0.0,
        progress=1.0 if state == ScanState.DONE else fraction,
        started_at=started,
        finished_at=datetime.now(UTC) if done else None,
        message=message,
    )


class _Lister:
    """One run of a source: the batches into a :class:`Collector`, the source's ``ctx.progress``
    into the scan's progress, and (``on_partial``) the tree of what came so far, built aside now
    and then while the listing goes on. While such a build reads the collector, new batches wait."""

    def __init__(
        self,
        uri: str,
        virtual: bool,
        measure: str,
        on_partial: Callable[[NodeStore], Awaitable[None]] | None,
    ) -> None:
        self.uri = uri
        self.virtual = virtual
        self.measure = measure
        self.on_partial = on_partial
        self.collector = Collector()
        self.backlog: list[bytes] = []
        self.building: asyncio.Task[None] | None = None
        self.started = time.monotonic()
        self.last_t = self.started
        self.last_n = 0

    def chunk(self, frames: list[bytes]) -> None:
        if self.building is not None and not self.building.done():
            self.backlog.extend(frames)  # a build reads the collector right now
        else:
            for f in frames:
                self.collector.add_batch(decode_frame(f)[1])
        self._maybe_partial()

    def _maybe_partial(self) -> None:
        if self.on_partial is None or (self.building is not None and not self.building.done()):
            return
        n, now = self.collector.count, time.monotonic()
        if n <= self.last_n or now - self.started < PARTIAL_FIRST_S or now - self.last_t < PARTIAL_EVERY_S:
            return
        if n < 2 * self.last_n and not (n <= PARTIAL_SMALL and now - self.last_t >= PARTIAL_SLOW_S):
            return
        self.building = asyncio.get_running_loop().create_task(self._partial(n), name="source-partial")

    async def _partial(self, n: int) -> None:
        assert self.on_partial is not None
        try:
            built = await asyncio.to_thread(
                build, self.uri, self.collector.root, virtual=self.virtual, measure=self.measure
            )
            self.last_n, self.last_t = n, time.monotonic()
            await self.on_partial(built)
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.exception("showing what a scan source listed so far failed")
        finally:
            waiting, self.backlog = self.backlog, []
            for f in waiting:
                self.collector.add_batch(decode_frame(f)[1])

    async def finish(self) -> None:
        """Wait for a build that is running (its waiting batches go in after it)."""
        if self.building is not None:
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await self.building
        for f in self.backlog:
            self.collector.add_batch(decode_frame(f)[1])
        self.backlog = []

    def cancel(self) -> None:
        if self.building is not None:
            self.building.cancel()


def _source(source_id: str | None) -> Contribution:
    c = EXTENSIONS.get("scan_source", source_id or "")
    if c is None:
        raise RpcError(ErrorCode.INVALID_PARAMS, tr("没有这个扫描来源：{id}", id=source_id))
    return c


async def _list(
    host: PluginHost,
    c: Contribution,
    uri: str,
    scan_id: str,
    on_count: Callable[[int], None] | None = None,
    on_wait: Callable[[], None] | None = None,
    on_progress: Callable[[float, float, str], None] | None = None,
    on_partial: Callable[[NodeStore], Awaitable[None]] | None = None,
) -> tuple[str, NodeStore]:
    """Run the source once for the scan ``scan_id``: (the root's display name, the tree built from
    what it listed); the source's ``ctx.tree`` is none (it is listing it).

    ``on_progress(done, total, text)``: the source's ``ctx.progress``; ``on_partial(store)``: the
    tree of what came so far, now and then while it lists (named after the URI until the end)."""
    measure = "size" if getattr(c.spec, "size", "alloc") == "size" else "alloc"
    lister = _Lister(uri, not c.spec.on_disk, measure, on_partial)

    def chunk(value: Any, frames: list[bytes]) -> None:
        lister.chunk(frames)
        if on_count is not None:
            on_count(lister.collector.count)

    try:
        value, _ = await host.call(
            c.owner,
            "source",
            c.local,
            {"uri": uri},
            where=host.places.listing(scan_id),
            on_chunk=chunk,
            timeout=LIST_TIMEOUT_S,
            on_wait=on_wait,
            on_progress=on_progress,
        )
        await lister.finish()
    finally:
        lister.cancel()
    twice = int((value or {}).get("duplicates") or 0)
    if twice:
        logger.bind(source=f"plugin:{c.owner}").warning(
            "scan source {}: {} entries name a path listed before; the last of each stays (their bytes "
            "are not added up)",
            c.id,
            twice,
        )
    name = str((value or {}).get("root") or uri)
    built = await asyncio.to_thread(
        build, name, lister.collector.root, virtual=not c.spec.on_disk, measure=measure
    )
    return name, built


def _by_path(old: NodeStore, new: NodeStore) -> Callable[[np.ndarray], np.ndarray]:
    """Node ids of ``old`` -> the nodes of ``new`` at the same path (-1: not there), worked out only
    for the ids asked (a view's zoom, selection and history): the tree of a source still listing is
    put in place often, a whole :func:`path_remap` each time would cost as much as the build."""
    names: dict[int, dict[str, int]] = {}
    roots = dict(zip(old.roots(), new.roots(), strict=False))

    def one(node: int) -> int:
        if node == 0:
            return 0
        chain: list[int] = []
        p = node
        while p > 0 and p not in roots:
            chain.append(p)
            p = int(old.cols["parent"][p])
        if p <= 0:
            return -1
        at = roots[p]
        for x in reversed(chain):
            kids = names.get(at)
            if kids is None:
                kids = names[at] = {new.name(ch): ch for ch in new.children(at)}
            got = kids.get(old.name(x))
            if got is None:
                return -1
            at = got
        return at

    def apply(ids: np.ndarray) -> np.ndarray:
        arr = np.asarray(ids, dtype=np.int64)
        n = len(old)
        return np.array([one(int(i)) if 0 <= i < n else -1 for i in arr.tolist()], dtype=np.int64)

    return apply


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
    said: list[Any] = [None, ""]  # the source's ctx.progress: fraction (None: cannot say), text
    views = host.server.views

    def show(force: bool = False) -> None:
        now = time.monotonic()
        if force or now - last[0] > 0.5:
            last[0] = now
            shown = handle.store if len(handle.store) > 2 else None
            handle.saved_progress = _progress(
                ScanState.SCANNING, started, seen[0], shown, message=said[1], fraction=said[0]
            )
            handle.publish()

    def counted(count: int) -> None:
        seen[0] = count
        show()

    def progressed(done: float, total: float, text: str) -> None:
        said[0] = min(1.0, max(0.0, done / total)) if total > 0 else None
        said[1] = text
        show()

    def waiting() -> None:  # every call slot of the plugin is busy: the status bar says so
        said[1] = tr("在等插件 {id}：它正忙", id=c.owner)
        show(force=True)

    async def put(built: NodeStore) -> None:
        """A tree replaces what the scan shows (what came so far, or the final one): views keep
        their zoom, selection and history by path."""
        async with STRUCTURE.write():
            old = handle.store.fixed()
            chains = views.chains(handle.id)
            handle.store.adopt(built)
            forget(handle.store)
            views.remap(handle.id, chains, _by_path(old, handle.store))

    async def partial(built: NodeStore) -> None:
        await put(built)
        show(force=True)

    async def run() -> None:
        try:
            name, built = await _list(host, c, uri, handle.id, counted, waiting, progressed, partial)
            await put(built)
            handle.roots = [RootInfo(path=name, node=1, volume=None, whole_volume=False)]
            handle.saved_progress = _progress(ScanState.DONE, started, len(handle.store) - 1, handle.store)
            scans._event("done", handle)
            if scans.monitors is not None:
                await scans.monitors.start(handle)  # the plugin's watcher, when the source has one
        except RpcError as exc:
            logger.warning("scan source {} failed: {}", c.id, exc.message)
            handle.saved_progress = _progress(
                ScanState.FAILED, started, seen[0], handle.store, message=exc.message
            )
        except asyncio.CancelledError:
            handle.saved_progress = _progress(ScanState.CANCELLED, started, seen[0], handle.store)
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
    name, built = await _list(host, c, uri, handle.id)
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

    async def _back(self, owner: str) -> Any:
        """Wait until the watcher's plugin is enabled and declares it again (``plugin dev`` reloaded
        it, the user turned it back on); its contribution then, or None once this monitor stops."""
        while not self._stopped:
            await asyncio.sleep(WAIT_BACK_S)
            rec = self.host.records.get(owner)
            if rec is None or rec.state != "enabled":
                continue
            w = watcher_of(self.handle.plugin_source or "")
            if w is not None:
                return w
        return None

    async def _watch(self) -> None:
        assert self.watcher is not None
        w = self.watcher
        failures = 0
        while not self._stopped:
            began = time.monotonic()
            try:
                await self.host.call(
                    w.owner,
                    "watch",
                    w.local,
                    {"uri": str(self.handle.uri)},
                    where=self.host.places.watch(self.handle.id),  # ctx.tree: the scan it watches
                    on_chunk=self._on_changes,
                )
                failures = 0  # the watcher ended by itself: start it again after a pause
            except asyncio.CancelledError:
                raise
            except RpcError as exc:
                if time.monotonic() - began >= RETRY_S[0]:
                    failures = 0  # it watched for a while: a new stop (the next plugin dev reload)
                failures += 1
                logger.bind(source=f"plugin:{w.owner}").warning("watcher {} stopped: {}", w.id, exc.message)
                if exc.code in (ErrorCode.PLUGIN_DISABLED, ErrorCode.METHOD_NOT_FOUND):
                    self.state, self.detail = (
                        "failed",
                        tr("监控器 {id} 已停止：{why}", id=w.id, why=exc.message),
                    )
                    self.handle.publish()
                    # the plugin is off or reloading: watch again once it is back
                    back = await self._back(w.owner)
                    if back is None:
                        return
                    self.watcher = w = back
                    self.state, self.detail = "running", ""
                    self.handle.publish()
                    self.rescan("")  # what changed while nobody watched
                    failures = 0
                    continue
                if exc.code in RESTARTED and failures == 1:
                    # a restart (plugin dev reloading it) ended the call: watch again soon, still
                    # running, and list again what changed while nobody watched
                    await asyncio.sleep(WAIT_BACK_S)
                    self.rescan("")
                    continue
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
                if self.state != "failed":  # the watcher's plugin is off: that says more
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

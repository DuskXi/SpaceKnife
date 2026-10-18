"""Live change monitoring of open scans (M5 file change hooks): docs/monitor.md §5.

One :class:`ScanMonitor` per scan (not for opened snapshots): a platform watcher per scan root
reports where something changed; batches are coalesced (``scan.coalesce_ms`` of quiet, at most
``MAX_WAIT_S``), then

- a file whose size or times changed is updated in place (``store.mutate.update_stats``);
- a folder whose entries changed is listed again — one level, with the scan's own listing code —
  and folded in by ``store.mutate.relist`` (deepest folders first, so a parent's relist carries a
  child's fresh range along); new folders are scanned incrementally;
- a lost batch (``overflow``) or an event storm marks a subtree dirty; it is listed again folder by
  folder once ``QUIET_RESCAN_S`` passed without events.

Changes run under ``STRUCTURE.write()`` on the event loop; views remap their node ids and flash
what changed. While a hard scan still runs, folders with children waiting in its queue are
deferred (their ids are in the queue); while an MFT scan reads, everything waits for its tree.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import threading
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal

import numpy as np
from loguru import logger

from spaceknife_protocol import scan as p

from ..events import emit, printable
from ..filter.names import forget
from ..monitor import Change, Watcher, create_watcher
from ..scan import worker
from ..scan.volume import volume_for
from ..scan.worker import SliceOptions, SliceResult
from ..store import mutate
from ..store.lock import STRUCTURE
from ..store.lookup import PathIndex
from ..store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore
from .compute import compute

if TYPE_CHECKING:
    from .scans import ScanHandle, ScanManager
    from .views import ViewManager

MAX_WAIT_S = 1.0  # a steady stream of events is still applied this often
STORM_FOLDERS = 2000  # more folders to list again in one batch: rescan their common subtree instead
STORM_EVENTS = 20_000
QUIET_RESCAN_S = 2.0  # dirty subtrees wait for this long without events
LIST_BATCH = 64  # folders listed per round of a subtree rescan
FREE_REFRESH_S = 1.0
SCAN_TICK_S = 0.25  # while a scan runs: hand new folders to the watchers this often


@dataclass(slots=True)
class _Applied:
    flashes: dict[int, int]  # node -> change of allocation (new ids)
    pending: list[int]  # new folders to scan
    kept_folders: list[int]  # folders whose rows moved (subtree rescans continue below them)
    changes: int


class _Remap:
    """Old ids to new ones over the relists of one batch (each relist moves rows at most once)."""

    def __init__(self, store: NodeStore) -> None:
        self.store = store
        self.steps: list[tuple[np.ndarray, np.ndarray]] = []

    def add(self, old: np.ndarray, new: np.ndarray) -> None:
        if len(old):
            order = np.argsort(old)
            self.steps.append((old[order], new[order]))

    def __call__(self, ids: np.ndarray) -> np.ndarray:
        out = np.asarray(ids, dtype=np.int64).copy()
        for old, new in self.steps:
            at = np.searchsorted(old, out)
            hit = (at < len(old)) & (old[np.minimum(at, len(old) - 1)] == out)
            out[hit] = new[at[hit]]
        n = len(self.store)
        live = (out >= 0) & (out < n)
        gone = np.zeros(len(out), dtype=bool)
        gone[live] = (self.store.cols["flags"][out[live]] & np.uint32(Flag.GONE)) != 0
        out[gone | (out >= n)] = -1
        return out

    def __bool__(self) -> bool:
        return bool(self.steps)


class ScanMonitor:
    def __init__(self, owner: MonitorManager, handle: ScanHandle) -> None:
        self.owner = owner
        self.handle = handle
        self.index = PathIndex(handle.store)
        self.watchers: list[tuple[str, Watcher]] = []  # (root path, watcher)
        self._lock = threading.Lock()
        self._queue: list[Change] = []
        self._loop = asyncio.get_running_loop()
        self._wake = asyncio.Event()
        self.task: asyncio.Task[None] | None = None
        self.deferred: set[str] = set()  # folders whose children still wait in the scan's queue
        self.trees: set[str] = set()  # subtrees to list again once quiet
        self.events = 0
        self.changes = 0
        self.overflows = 0
        self.last_apply_ms = 0.0
        self._last_event = 0.0
        self._fed = 0  # rows below this were handed to the watchers
        self._free_at = 0.0
        self.failed = ""

    # ------------------------------------------------------------- lifecycle

    async def start(self) -> None:
        for root in self.handle.roots:
            watcher = await self._start_watcher(root.path, self._watcher_for(root.path, root.volume))
            if watcher is not None:
                self.watchers.append((root.path, watcher))
        self.task = self._loop.create_task(self._run(), name=f"monitor-{self.handle.id}")

    async def _start_watcher(self, path: str, watcher: Watcher) -> Watcher | None:
        """Start ``watcher`` off the loop (FSEvents may take a second); a native watcher that fails
        at once (no change notification on this file system) gives way to polling."""
        try:
            await asyncio.to_thread(watcher.start, self._sink)
        except Exception as exc:
            logger.warning("cannot watch {} with {}: {}", path, watcher.name, exc)
            if watcher.name == "poll":
                self.failed = f"无法监视 {printable(path)}：{exc}"
                return None
            return await self._start_watcher(path, self._poller(path))
        if watcher.name != "poll" and await self._fails_at_once(watcher):
            await asyncio.to_thread(watcher.stop)
            return await self._start_watcher(path, self._poller(path))
        return watcher

    @staticmethod
    async def _fails_at_once(watcher: Watcher) -> bool:
        for _ in range(10):  # the first read is issued on the watcher's thread: give it a moment
            state = watcher.status().state
            if state != "starting":
                return state == "failed"
            await asyncio.sleep(0.05)
        return False

    def _poller(self, path: str) -> Watcher:
        from ..monitor.poll import PollWatcher

        return PollWatcher(path, poll_interval=self.owner.poll_interval)

    def _watcher_for(self, path: str, volume: Any) -> Watcher:
        if os.name != "nt" and volume is not None and (volume.is_remote or volume.note):
            # NFS, SMB, WSL's 9P drives: the kernel never hears of other machines' (or Windows')
            # changes, so per-folder notifications would stay silent: poll instead
            return self._poller(path)
        return create_watcher(path, poll_interval=self.owner.poll_interval)

    def kick(self) -> None:
        """Look at the tree now (a scan just finished: its last folders still need watches)."""
        self._wake.set()

    @property
    def fed(self) -> bool:
        """Every folder of the tree was handed to the watchers."""
        return self._fed >= len(self.handle.store)

    def rebuilt(self) -> None:
        """The whole tree was replaced (an MFT scan finished): every id is new."""
        self.index.forget()
        self._fed = 0

    async def stop(self) -> None:
        for _, watcher in self.watchers:
            await asyncio.to_thread(watcher.stop)
        self.watchers.clear()
        if self.task is not None:
            self.task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await self.task
            self.task = None

    def _sink(self, batch: list[Change]) -> None:  # watcher threads
        with self._lock:
            self._queue.extend(batch)
        self._loop.call_soon_threadsafe(self._wake.set)

    # ------------------------------------------------------------- status

    def status(self) -> p.MonitorStatus:
        states = [w.status() for _, w in self.watchers]
        state: Literal["starting", "running", "degraded", "failed"]
        if self.failed and not states:
            state, detail = "failed", self.failed
        elif any(s.state == "failed" for s in states):
            state, detail = "failed", next(s.detail for s in states if s.state == "failed")
        elif self.failed or any(s.state == "degraded" for s in states):
            state = "degraded"
            detail = self.failed or next(s.detail for s in states if s.state == "degraded")
        elif states and all(s.state == "running" for s in states):
            state, detail = "running", ""
        else:
            state, detail = "starting", ""
        with self._lock:
            queued = len(self._queue)
        return p.MonitorStatus(
            state=state,
            detail=detail,
            watcher=",".join(sorted({w.name for _, w in self.watchers})),
            watched_dirs=sum(s.watched_dirs for s in states)
            if all(s.watched_dirs >= 0 for s in states)
            else -1,
            polled_dirs=sum(s.polled_dirs for s in states),
            events=self.events,
            changes=self.changes,
            overflows=self.overflows + sum(s.overflows for s in states),
            pending=queued + len(self.deferred) + len(self.trees),
            last_apply_ms=round(self.last_apply_ms, 1),
        )

    # ------------------------------------------------------------- the loop

    async def _run(self) -> None:
        while True:
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(self._wake.wait(), self._timeout())
            self._wake.clear()
            first = time.monotonic()
            while time.monotonic() - first < MAX_WAIT_S:  # coalesce: wait for a quiet moment
                await asyncio.sleep(self.owner.coalesce_s())
                if not self._wake.is_set():
                    break
                self._wake.clear()
            with self._lock:
                batch, self._queue = self._queue, []
            try:
                await self._feed()
                if self.handle.method == "mft" and self.handle.running:
                    with self._lock:  # the tree arrives at once at the end: keep everything for it
                        self._queue[:0] = batch
                    continue
                await self._process(batch)
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception("applying file changes to {} failed", self.handle.id)

    def _timeout(self) -> float | None:
        if self.trees:
            return max(0.05, QUIET_RESCAN_S - (time.monotonic() - self._last_event))
        if self.handle.running or self.deferred:
            return SCAN_TICK_S
        return None

    async def _feed(self) -> None:
        """Hand folders that entered the tree to per-folder watchers (inotify, polling)."""
        store = self.handle.store
        n = len(store)
        if self._fed >= n:
            return
        wanted = [(root, w) for root, w in self.watchers if w.name != "rdcw"]
        start, self._fed = self._fed, n
        if not wanted:
            return

        def folders() -> list[str]:
            c = store.cols
            ids = np.arange(start, n, dtype=np.int64)
            skip = np.uint32(Flag.GONE | Flag.MOUNT)  # mount points are never entered by the scan
            ids = ids[(c["kind"][start:n] == Kind.DIR) & ((c["flags"][start:n] & skip) == 0)]
            ids = ids[np.argsort(c["depth"][ids], kind="stable")]  # shallow first
            return [store.path(int(i)) for i in ids]

        paths = await compute(folders)
        for root, watcher in wanted:
            prefix = root if root.endswith(os.sep) else root + os.sep
            mine = [q for q in paths if q == root or q.startswith(prefix)]
            if mine:
                watcher.add_dirs(mine)

    # ------------------------------------------------------------- one batch

    async def _process(self, batch: list[Change]) -> None:
        store = self.handle.store
        now = time.monotonic()
        if batch:
            self.events += len(batch)
            self._last_event = now
        stats: dict[str, Change] = {}
        folders: set[str] = set(self.deferred)
        self.deferred.clear()
        for ch in batch:
            if ch.kind == "overflow":
                self.overflows += 1
                self.trees.add(ch.path)
            elif ch.kind == "modified" and not ch.is_dir:
                stats[ch.path] = ch
            elif ch.kind == "dir":
                folders.add(ch.path)
            else:
                folders.add(os.path.dirname(ch.path))
                if ch.old_path:
                    folders.add(os.path.dirname(ch.old_path))
        if len(folders) > STORM_FOLDERS or len(batch) > STORM_EVENTS:
            self.trees.update(_common_roots(folders | {os.path.dirname(q) for q in stats}))
            emit(
                "monitor",
                f"文件变化过多（{len(batch):,} 条），稍后重新扫描受影响的文件夹",
                scan_id=self.handle.id,
            )
            folders.clear()
            stats.clear()

        t0 = time.perf_counter()
        changes = 0
        flashes: dict[int, int] = {}
        if stats:
            got = await self._apply_stats(stats, folders)
            flashes.update(got)
            changes += len(got)
        targets = self._targets(folders)
        quiet = now - self._last_event >= QUIET_RESCAN_S or (not batch and bool(self.trees))
        if self.trees and quiet and not self.handle.running:
            for path in sorted(self.trees):
                node, _ = self.index.locate(path)
                if node is not None and store.cols["kind"][node] == Kind.DIR:
                    targets[node] = store.path(node)
            rescan = True
            self.trees.clear()
        else:
            rescan = False
        while targets:
            applied = await self._relist(targets)
            flashes.update(applied.flashes)
            changes += applied.changes
            await self._scan_new(applied.pending)
            # a subtree rescan goes on below every folder that stayed
            targets = {k: store.path(k) for k in applied.kept_folders} if rescan else {}
            targets = {k: v for k, v in targets.items() if mutate.can_relist(store, k)}
        if changes or flashes:
            self.changes += changes
            self.last_apply_ms = (time.perf_counter() - t0) * 1000
            self.owner.views.flash(self.handle.id, flashes)
            await self._refresh_free(now)
            self.handle.publish()
            await self._maybe_compact()

    def _targets(self, folders: set[str]) -> dict[int, str]:
        """Folders to list again, by node: missing ones resolve to their deepest known ancestor;
        folders not listed yet are left to the scan; folders with children in the scan's queue wait."""
        store = self.handle.store
        c = store.cols
        out: dict[int, str] = {}
        for path in folders:
            node, deepest = self.index.locate(path)
            folder = node if node is not None and c["kind"][node] == Kind.DIR else deepest
            if folder is None or c["kind"][folder] != Kind.DIR:
                continue
            if c["flags"][folder] & np.uint32(Flag.PENDING):
                continue  # the scan has not listed it yet: it will read it as it is now
            if not mutate.can_relist(store, folder):
                self.deferred.add(store.path(folder))
                continue
            out[folder] = store.path(folder)
        return out

    async def _apply_stats(self, stats: dict[str, Change], folders: set[str]) -> dict[int, int]:
        """Size and time changes of known files, in place; unknown paths make their folder dirty."""
        store = self.handle.store
        c = store.cols
        nodes: list[int] = []
        for path in stats:
            node, _ = self.index.locate(path)
            if node is None or c["kind"][node] != Kind.FILE:
                folders.add(os.path.dirname(path))
            else:
                nodes.append(node)
        if not nodes:
            return {}
        known = [stats[store.path(n)] for n in nodes]
        if os.name == "nt":
            # no allocation in the event (compressed or sparse file): its folder's listing has it
            for i, ch in enumerate(known):
                if ch.size is not None and ch.alloc is None:
                    folders.add(os.path.dirname(ch.path))
                    known[i] = Change("modified", ch.path)
            if self.handle.method == "hard":  # keep to the hard scan's rule (resident, clusters)
                cloud = (c["flags"][np.array(nodes, dtype=np.int64)] & np.uint32(Flag.CLOUD)) != 0
                known = [self._hard_alloc(ch, bool(k)) for ch, k in zip(known, cloud.tolist(), strict=True)]
        missing = [i for i, ch in enumerate(known) if ch.size is None and os.name != "nt"]
        if missing:  # the watcher did not say: ask the file system
            looked = await asyncio.to_thread(_stat_many, [known[i].path for i in missing], self._cluster())
            for i, got in zip(missing, looked, strict=True):
                known[i] = got if got is not None else known[i]
        keep = [i for i, ch in enumerate(known) if ch.size is not None and ch.alloc is not None]
        ids = np.array([nodes[i] for i in keep], dtype=np.int64)
        size = np.array([known[i].size for i in keep], dtype=np.int64)
        alloc = np.array([known[i].alloc for i in keep], dtype=np.int64)
        mtime = np.array([known[i].mtime or 0 for i in keep], dtype=np.int64)
        async with STRUCTURE.write():
            live = (c["kind"][ids] == Kind.FILE) & ((c["flags"][ids] & np.uint32(Flag.GONE)) == 0)
            ids, size, alloc, mtime = ids[live], size[live], alloc[live], mtime[live]
            delta = mutate.update_stats(store, ids, size, alloc, mtime)
        return {int(i): int(d) for i, d in zip(ids.tolist(), delta.tolist(), strict=True) if d}

    async def _relist(self, targets: dict[int, str]) -> _Applied:
        """List ``targets`` again (in threads), then fold every listing in, deepest folder first."""
        store = self.handle.store
        listed: dict[int, SliceResult] = {}
        items = sorted(targets.items(), key=lambda kv: -int(store.cols["depth"][kv[0]]))
        for at in range(0, len(items), LIST_BATCH):
            chunk = items[at : at + LIST_BATCH]
            await self._permit()
            results = await asyncio.gather(*(self._list_one(node, path) for node, path in chunk))
            listed.update(
                {node: res for (node, _), res in zip(chunk, results, strict=True) if res is not None}
            )
        out = _Applied({}, [], [], 0)
        async with STRUCTURE.write():
            chains = self.owner.views.chains(self.handle.id)
            remap = _Remap(store)
            c = store.cols
            for node, path in items:
                res = listed.get(node)
                if res is None or not mutate.can_relist(store, node) or store.path(node) != path:
                    continue
                remap_ext = np.array([store.ext_id(e) for e in res.ext_table], dtype=np.uint16)
                ext = remap_ext[res.ext_ids] if len(res.ext_ids) else res.ext_ids
                rel = mutate.relist(
                    store, node, res.name_blob, res.name_lens, {k: res.cols[k] for k in ENTRY_COLUMNS}, ext
                )
                remap.add(rel.moved_from, rel.moved_to)
                out.changes += len(rel.added) + len(rel.removed) + len(rel.grown)
                for i in rel.added.tolist():
                    out.flashes[i] = int(c["alloc"][i])
                for i, d in zip(rel.grown.tolist(), rel.grown_by.tolist(), strict=True):
                    out.flashes[i] = d
                if len(rel.removed):
                    out.flashes[node] = out.flashes.get(node, 0) - int(rel.removed_alloc.sum())
                out.pending.extend(rel.pending.tolist())
                kept = rel.moved_to[store.cols["kind"][rel.moved_to] == Kind.DIR]
                out.kept_folders.extend(kept.tolist())
            if remap:
                moved = remap(np.array(list(out.flashes), dtype=np.int64))
                out.flashes = {
                    int(m): d for m, d in zip(moved.tolist(), out.flashes.values(), strict=True) if m >= 0
                }
                out.kept_folders = [
                    int(m) for m in remap(np.array(out.kept_folders, dtype=np.int64)) if m >= 0
                ]
                out.pending = [int(m) for m in remap(np.array(out.pending, dtype=np.int64)) if m >= 0]
                self.owner.views.remap(self.handle.id, chains, remap)
        return out

    async def _list_one(self, node: int, path: str) -> SliceResult | None:
        root_idx = self._root_index(path)
        if root_idx is None:
            return None
        opts = SliceOptions(roots=self._root_opts(), budget_s=0.0)  # this folder only
        res = await self._slice([((0, node, 0), path, root_idx)], opts)
        if len(res.refs) != 1 or res.errors:
            return None  # vanished (its parent lists it as gone) or no longer readable
        return res

    async def _scan_new(self, folders: list[int]) -> None:
        """Scan folders that appeared (like the scan itself: listings in threads, ingest here)."""
        store = self.handle.store
        queue: list[worker.Task] = []
        for node in folders:
            path = store.path(node)
            idx = self._root_index(path)
            if idx is not None:
                queue.append(((0, node, 0), path, idx))
        opts = SliceOptions(roots=self._root_opts(), budget_s=0.05)
        while queue:
            batch, queue = queue[:LIST_BATCH], queue[LIST_BATCH:]
            await self._permit()
            res = await self._slice(batch, opts)
            remap_ext = np.array([store.ext_id(e) for e in res.ext_table], dtype=np.uint16)
            ext = remap_ext[res.ext_ids] if len(res.ext_ids) else res.ext_ids
            starts = store.ingest_slice(
                res.refs,
                res.bounds,
                res.name_blob,
                res.name_lens,
                {k: res.cols[k] for k in ENTRY_COLUMNS},
                ext,
            ).tolist()

            def resolve(ref: tuple[int, int, int], starts: list[int] = starts) -> int:
                kind, a, b = ref
                return a if kind == 0 else starts[a] + b

            for ref, _path, _msg in res.errors:
                node = resolve(ref)
                store.mark(node, Flag.ACCESS_DENIED)
                store.clear(node, Flag.PENDING)
            for ref, path, idx in res.frontier:
                queue.append(((0, resolve(ref), 0), path, idx))

    async def _slice(self, tasks: list[worker.Task], opts: SliceOptions) -> SliceResult:
        slicer = self.owner.scans.slicer()
        if slicer is None:
            return await asyncio.to_thread(worker.scan_slice, tasks, opts)
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self.owner.scans.thread_executor(), slicer, tasks, opts)

    async def _permit(self) -> None:
        gov = self.handle.governor
        if gov is not None and self.handle.running:
            permit = await gov.permit()
            permit.release()

    def _root_opts(self) -> tuple[worker.RootOptions, ...]:
        scanner = self.handle.scanner
        if scanner is not None:
            return scanner.root_options
        return tuple(worker.RootOptions() for _ in self.handle.roots)

    def _root_index(self, path: str) -> int | None:
        norm = os.path.normcase(path)
        for i, root in enumerate(self.handle.roots):
            r = os.path.normcase(root.path)
            if norm == r or norm.startswith(r if r.endswith(os.sep) else r + os.sep):
                return i
        return None

    def _hard_alloc(self, ch: Change, cloud: bool) -> Change:
        """The allocation a hard scan would report for this size (Windows estimates it: a cloud
        placeholder occupies nothing, files up to ``resident_max`` live in the MFT record, the rest
        round up to whole clusters)."""
        if ch.size is None or ch.alloc is None:
            return ch
        if cloud:
            return Change("modified", ch.path, size=ch.size, alloc=0, mtime=ch.mtime)
        idx = self._root_index(ch.path)
        opts = self._root_opts()[idx] if idx is not None else worker.RootOptions()
        cluster = opts.cluster_size or 4096
        alloc = 0 if ch.size <= opts.resident_max else -(-ch.size // cluster) * cluster
        return Change("modified", ch.path, size=ch.size, alloc=alloc, mtime=ch.mtime)

    def _cluster(self) -> int:
        vol = self.handle.roots[0].volume
        return vol.cluster_size if vol is not None else 4096

    async def _refresh_free(self, now: float) -> None:
        """Free space moves with the changes (status bar, unknown space): read it again, rarely."""
        if now - self._free_at < FREE_REFRESH_S:
            return
        self._free_at = now
        for root in self.handle.roots:
            if root.volume is not None and root.whole_volume:
                with contextlib.suppress(OSError):
                    root.volume = await asyncio.to_thread(volume_for, root.path)

    async def _maybe_compact(self) -> None:
        store = self.handle.store
        if self.handle.running or not mutate.needs_compaction(store):
            return
        version = store.version
        fresh, remap = await compute(mutate.compact, store)
        async with STRUCTURE.write():
            if store.version != version:
                return  # changed meanwhile: try again after the next batch
            chains = self.owner.views.chains(self.handle.id)
            store.adopt(fresh)
            forget(store)
            self.index.forget()
            self._fed = len(store)
            self.owner.views.remap(self.handle.id, chains, lambda ids: remap[np.asarray(ids, dtype=np.int64)])
        logger.info("scan {}: compacted after changes, {:,} rows", self.handle.id, len(store))


def _common_roots(paths: set[str]) -> set[str]:
    """The common folder of ``paths`` per drive / file system root."""
    groups: dict[str, list[str]] = {}
    for q in paths:
        groups.setdefault(os.path.splitdrive(q)[0] or os.sep, []).append(q)
    out = set()
    for qs in groups.values():
        try:
            out.add(os.path.commonpath(qs))
        except ValueError:
            out.update(qs)
    return out


def _stat_many(paths: list[str], cluster: int) -> list[Change | None]:
    out: list[Change | None] = []
    for q in paths:
        try:
            st = os.stat(q)
        except OSError:
            out.append(None)
            continue
        blocks = getattr(st, "st_blocks", None)
        alloc = blocks * 512 if blocks is not None else -(-st.st_size // cluster) * cluster
        out.append(Change("modified", q, size=st.st_size, alloc=alloc, mtime=int(st.st_mtime)))
    return out


class MonitorManager:
    """Starts and stops the monitors of open scans; owns their settings."""

    def __init__(self, scans: ScanManager, views: ViewManager) -> None:
        self.scans = scans
        self.views = views
        self.monitors: dict[str, ScanMonitor] = {}
        self.poll_interval = 30.0

    def enabled(self) -> bool:
        st = self.scans.settings
        return st is None or bool(st.get("scan.monitor"))

    def coalesce_s(self) -> float:
        st = self.scans.settings
        ms = int(st.get("scan.coalesce_ms")) if st is not None else 150
        return max(0.02, ms / 1000)

    async def start(self, handle: ScanHandle) -> None:
        if handle.source != "scan" or handle.id in self.monitors or not self.enabled():
            return
        mon = ScanMonitor(self, handle)
        await mon.start()
        self.monitors[handle.id] = mon
        handle.monitor = mon

    async def stop(self, scan_id: str) -> None:
        mon = self.monitors.pop(scan_id, None)
        if mon is not None:
            mon.handle.monitor = None
            await mon.stop()

    async def close(self) -> None:
        for scan_id in list(self.monitors):
            await self.stop(scan_id)

    async def apply_settings(self) -> None:
        """``scan.monitor`` switched: start or stop monitoring every open scan."""
        if self.enabled():
            for handle in list(self.scans.scans.values()):
                await self.start(handle)
        else:
            await self.close()
            for handle in list(self.scans.scans.values()):
                handle.publish()


__all__ = ["MonitorManager", "ScanMonitor"]

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
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Literal

import numpy as np
from loguru import logger

from spaceknife_protocol import scan as p
from spaceknife_protocol.errors import ErrorCode

from ..events import emit, printable
from ..filter.names import forget
from ..i18n import tr
from ..monitor import Change, Watcher, create_watcher
from ..scan import worker
from ..scan.scanner import verify_names
from ..scan.volume import volume_for
from ..scan.worker import SliceOptions, SliceResult
from ..store import mutate
from ..store.lock import STRUCTURE
from ..store.lookup import PathIndex
from ..store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore
from .compute import compute
from .rpc import RpcError

if TYPE_CHECKING:
    from .scans import ScanHandle, ScanManager
    from .views import ViewManager

MAX_WAIT_S = 1.0  # a steady stream of events is still applied this often
STORM_FOLDERS = 2000  # more folders to list again in one batch: rescan their common subtree instead
STORM_EVENTS = 20_000
QUIET_RESCAN_S = 2.0  # dirty subtrees wait for this long without events
LIST_BATCH = 64  # folders listed per round of a subtree rescan
FREE_REFRESH_S = 1.0
USN_PERIOD_S = 30.0  # checkpoints of the USN journal move on this often
USN_MAX_ROWS = 1_000_000  # more records to replay than this: list the subtree again instead
REF_MASK = (1 << 48) - 1  # an NTFS file reference's record number
SCAN_TICK_S = 0.25  # while a scan runs: hand new folders to the watchers this often


@dataclass(slots=True)
class _Applied:
    flashes: dict[int, int]  # node -> change of allocation (new ids)
    pending: list[int]  # new folders to scan
    kept_folders: list[int]  # folders whose rows moved (subtree rescans continue below them)
    changes: int
    touched: list[int] = field(default_factory=list)  # files whose size or mtime changed (new ids)


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
    def __init__(self, owner: MonitorManager, handle: ScanHandle, *, watch: bool = True) -> None:
        self.owner = owner
        self.handle = handle
        self.watch = watch  # False: no watchers, only rescans asked for (live monitoring is off)
        self.index = PathIndex(handle.store)
        self.watchers: list[tuple[str, Watcher]] = []  # (root path, watcher)
        self._lock = threading.Lock()
        self._queue: list[Change] = []
        self._loop = asyncio.get_running_loop()
        self._wake = asyncio.Event()
        self.task: asyncio.Task[None] | None = None
        self.usn_task: asyncio.Task[None] | None = None  # the first change-journal checkpoint
        self.deferred: set[str] = set()  # folders whose children still wait in the scan's queue
        self.trees: set[str] = set()  # subtrees to list again once quiet
        self.events = 0
        self.changes = 0
        self.overflows = 0
        self.last_apply_ms = 0.0
        self._last_event = 0.0
        self._fed = 0  # rows below this were handed to the watchers
        self._free_at = 0.0
        self._free_due = False  # a change came within FREE_REFRESH_S of the last read
        self.failed = ""
        # Windows: per volume (journal id, safe checkpoint, newer checkpoint) of the USN journal, to
        # replay what an overflow lost; the safe one lags one period behind (events still buffered)
        self.usn: dict[str, tuple[int, int, int]] = {}
        self._usn_at = 0.0
        self._ino: tuple[int, np.ndarray, np.ndarray] | None = None  # (rows, sorted low-48 ino, node)

    # ------------------------------------------------------------- lifecycle

    async def start(self) -> None:
        for root in self.handle.roots if self.watch else []:
            watcher = await self._start_watcher(root.path, self._watcher_for(root.path, root.volume))
            if watcher is not None:
                self.watchers.append((root.path, watcher))
        self.task = self._loop.create_task(self._run(), name=f"monitor-{self.handle.id}")
        self.kick()  # a small scan may be done already: its folders need their watches now

    async def _start_watcher(self, path: str, watcher: Watcher) -> Watcher | None:
        """Start ``watcher`` off the loop (FSEvents may take a second); a native watcher that fails
        at once (no change notification on this file system) gives way to polling."""
        try:
            await asyncio.to_thread(watcher.start, self._sink)
        except Exception as exc:
            logger.warning("cannot watch {} with {}: {}", path, watcher.name, exc)
            if watcher.name == "poll":
                self.failed = tr("无法监视 {path}：{error}", path=printable(path), error=exc)
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

    def rescan(self, path: str) -> None:
        """List ``path`` and everything below it again, right away."""
        self.trees.add(path)
        self._last_event = 0.0  # no quiet period to wait for: asked for by the user
        self.kick()

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
        for task in (self.task, self.usn_task):
            if task is not None:
                task.cancel()
                with contextlib.suppress(asyncio.CancelledError, Exception):
                    await task
        self.task = self.usn_task = None

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
                now = time.monotonic()
                if self._free_due and now - self._free_at >= FREE_REFRESH_S and await self._refresh_free(now):
                    self.owner.views.volumes_changed(self.handle.id)
                    self.handle.publish()
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception("applying file changes to {} failed", self.handle.id)

    def _timeout(self) -> float | None:
        if self.trees:
            wait: float | None = max(0.05, QUIET_RESCAN_S - (time.monotonic() - self._last_event))
        elif self.handle.running or self.deferred or not self.fed:
            wait = SCAN_TICK_S
        else:
            wait = None
        if self._free_due:  # wake up to read the free space once the second is over
            due = max(0.05, FREE_REFRESH_S - (time.monotonic() - self._free_at))
            wait = due if wait is None else min(wait, due)
        return wait

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
        lost: set[str] = set()
        folders: set[str] = set(self.deferred)
        self.deferred.clear()
        for ch in batch:
            if ch.kind == "overflow":
                self.overflows += 1
                lost.add(ch.path)
            elif ch.kind == "modified" and not ch.is_dir:
                stats[ch.path] = ch
            elif ch.kind == "dir":
                folders.add(ch.path)
            else:
                folders.add(os.path.dirname(ch.path))
                if ch.old_path:
                    folders.add(os.path.dirname(ch.old_path))
        if lost:
            replayed = await self._replay_usn(lost)
            if replayed is None:
                self.trees.update(lost)  # no journal to replay from: list the subtrees again
            else:
                folders |= replayed
        await self._usn_tick(now)
        if len(folders) > STORM_FOLDERS or len(batch) > STORM_EVENTS:
            self.trees.update(_common_roots(folders | {os.path.dirname(q) for q in stats}))
            emit(
                "monitor",
                tr("文件变化过多（{count} 条），稍后重新扫描受影响的文件夹", count=f"{len(batch):,}"),
                scan_id=self.handle.id,
            )
            folders.clear()
            stats.clear()

        t0 = time.perf_counter()
        changes = 0
        flashes: dict[int, int] = {}
        if stats:
            got, touched = await self._apply_stats(stats, folders)
            flashes.update(got)
            changes += len(got)
            # every file reported modified, a same-size rewrite too: duplicate-file results drop it
            self.owner.views.files_changed(self.handle.id, touched)
        targets = self._targets(folders)
        quiet = now - self._last_event >= QUIET_RESCAN_S or (not batch and bool(self.trees))
        if self.trees and quiet and not self.handle.running:
            for path in sorted(self.trees):
                node, _ = self.index.locate(path)
                if node is not None and store.cols["kind"][node] == Kind.DIR and not self.index.unnamed(node):
                    targets[node] = store.path(node)
            rescan = True
            self.trees.clear()
        else:
            rescan = False
        while targets:
            applied = await self._relist(targets)
            flashes.update(applied.flashes)
            changes += applied.changes
            self.owner.views.files_changed(self.handle.id, applied.touched)
            await self._scan_new(applied.pending)
            # a subtree rescan goes on below every folder that stayed
            targets = {k: store.path(k) for k in applied.kept_folders} if rescan else {}
            targets = {k: v for k, v in targets.items() if mutate.can_relist(store, k)}
        if changes or flashes:
            self.changes += changes
            self.last_apply_ms = (time.perf_counter() - t0) * 1000
            await self._refresh_free(now)  # first: the new frames draw free and unknown space from it
            self.owner.views.flash(self.handle.id, flashes)
            self.handle.publish()
            await self._maybe_compact()

    def _targets(self, folders: set[str]) -> dict[int, str]:
        """Folders to list again, by node: missing ones resolve to their deepest known ancestor;
        folders not listed yet are left to the scan; folders with children in the scan's queue wait;
        folders whose path could open another entry of the very same name stay as scanned."""
        store = self.handle.store
        c = store.cols
        out: dict[int, str] = {}
        for path in folders:
            node, deepest = self.index.locate(path)
            folder = node if node is not None and c["kind"][node] == Kind.DIR else deepest
            if folder is None or c["kind"][folder] != Kind.DIR or self.index.unnamed(folder):
                continue
            if c["flags"][folder] & np.uint32(Flag.PENDING):
                continue  # the scan has not listed it yet: it will read it as it is now
            if not mutate.can_relist(store, folder):
                self.deferred.add(store.path(folder))
                continue
            out[folder] = store.path(folder)
        return out

    async def _apply_stats(
        self, stats: dict[str, Change], folders: set[str]
    ) -> tuple[dict[int, int], list[int]]:
        """Size and time changes of known files, in place; unknown paths make their folder dirty.
        Returns the change of allocation of each file it moved, and every file it updated (a
        same-size rewrite included)."""
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
            return {}, []
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
        touched = sorted(set(nodes))  # every file reported modified, whatever its size did
        return {int(i): int(d) for i, d in zip(ids.tolist(), delta.tolist(), strict=True) if d}, touched

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
                # new subfolders no path can open (the very same name twice, a `\` in it): not
                # scanned by their path, which would list another one, or nothing
                unreadable = {
                    int(rel.start) + int(j)
                    for (kind, at, j), _path, _msg in res.errors
                    if (kind, at) == (1, 0)
                }
                for row in unreadable & set(rel.pending.tolist()):
                    store.mark(row, Flag.ACCESS_DENIED)
                    store.clear(row, Flag.PENDING)
                remap.add(rel.moved_from, rel.moved_to)
                out.changes += len(rel.added) + len(rel.removed) + len(rel.grown)
                for i in rel.added.tolist():
                    out.flashes[i] = int(c["alloc"][i])
                for i, d in zip(rel.grown.tolist(), rel.grown_by.tolist(), strict=True):
                    out.flashes[i] = d
                out.touched.extend(rel.rewritten.tolist())
                if len(rel.removed):
                    out.flashes[node] = out.flashes.get(node, 0) - int(rel.removed_alloc.sum())
                out.pending.extend(i for i in rel.pending.tolist() if i not in unreadable)
                kept = rel.moved_to[store.cols["kind"][rel.moved_to] == Kind.DIR]
                # a name the folder holds twice, or one with a separator, is listed by its path as
                # another folder: those stay as scanned (the scan opened each by its file id), and so
                # does what lies below them
                unnamed = _unnamed(res)
                out.kept_folders.extend(k for k in kept.tolist() if store.name_bytes(k) not in unnamed)
            if remap:
                moved = remap(np.array(list(out.flashes), dtype=np.int64))
                out.flashes = {
                    int(m): d for m, d in zip(moved.tolist(), out.flashes.values(), strict=True) if m >= 0
                }
                out.kept_folders = [
                    int(m) for m in remap(np.array(out.kept_folders, dtype=np.int64)) if m >= 0
                ]
                out.pending = [int(m) for m in remap(np.array(out.pending, dtype=np.int64)) if m >= 0]
                out.touched = [int(m) for m in remap(np.array(out.touched, dtype=np.int64)) if m >= 0]
                self.owner.views.remap(self.handle.id, chains, remap)
        return out

    # ------------------------------------------------------------- USN journal (Windows)

    def _usn_volumes(self) -> list[str]:
        """``X:`` of every local NTFS volume under the roots."""
        out = []
        for root in self.handle.roots:
            v = root.volume
            if (
                v is not None
                and not v.is_remote
                and v.fs_type.upper() == "NTFS"
                and v.mount_point[1:2] == ":"
            ):
                out.append(v.mount_point[:2].upper())
        return sorted(set(out))

    async def _usn_client(self) -> Any:
        """The elevated helper if it is already running (an MFT scan started it); never started
        just for this. The checkpoint queries every USN_PERIOD_S keep it from idling out."""
        if os.name != "nt":
            return None
        current = self.owner.scans.helper.current
        if current is None or not current.alive or not current.elevated:
            return None
        return current

    async def start_usn(self) -> None:
        """Take a checkpoint of each volume's change journal (when an overflow needs replaying)."""
        client = await self._usn_client()
        if client is None:
            return
        for vol in self._usn_volumes():
            try:
                j = await client.query_usn(vol)
            except Exception as exc:
                logger.info("no USN journal on {}: {}", vol, exc)
                continue
            jid, nxt = int(j["journal_id"]), int(j["next_usn"])
            self.usn[vol] = (jid, nxt, nxt)
        self._usn_at = time.monotonic()

    async def _usn_tick(self, now: float) -> None:
        """Move the checkpoints on (the safe one to the previous newer one), every USN_PERIOD_S."""
        if not self.usn or now - self._usn_at < USN_PERIOD_S:
            return
        self._usn_at = now
        client = await self._usn_client()
        if client is None:
            return
        for vol, (jid, _safe, newer) in list(self.usn.items()):
            try:
                j = await client.query_usn(vol)
            except Exception:
                del self.usn[vol]
                continue
            if int(j["journal_id"]) != jid:
                self.usn[vol] = (int(j["journal_id"]), int(j["next_usn"]), int(j["next_usn"]))
            else:
                self.usn[vol] = (jid, newer, int(j["next_usn"]))

    async def _replay_usn(self, lost: set[str]) -> set[str] | None:
        """Folders touched since the safe checkpoint of the volumes holding ``lost``, from the
        journal; None when that is not possible (no journal, no rights, too many records)."""
        vols = {os.path.splitdrive(q)[0].upper() for q in lost}
        if not vols or not vols <= set(self.usn):
            return None
        client = await self._usn_client()
        if client is None:
            return None
        parents: list[np.ndarray] = []
        for vol in sorted(vols):
            jid, safe, _ = self.usn[vol]
            try:
                got = await client.read_usn(vol, safe, jid, max_rows=USN_MAX_ROWS)
            except Exception as exc:
                logger.info("USN replay on {} failed: {}", vol, exc)
                return None
            if got.get("truncated"):
                return None
            parents.append(np.asarray(got["parent_frn"], dtype=np.uint64))
        frn = np.unique(np.concatenate(parents)) if parents else np.empty(0, dtype=np.uint64)
        nodes = self._nodes_of_frn(frn)
        if nodes is None:
            return None  # the tree has no file ids (scanned without the native lister)
        store = self.handle.store
        emit(
            "monitor",
            tr("变化通知溢出：按 USN 日志补齐 {count} 个文件夹", count=f"{len(nodes):,}"),
            scan_id=self.handle.id,
        )
        return {store.path(int(n)) for n in nodes}

    def _nodes_of_frn(self, frn: np.ndarray) -> np.ndarray | None:
        """Live folders whose file id (record number: the low 48 bits) is in ``frn``; None when the
        tree carries no file ids at all."""
        store = self.handle.store
        n = len(store)
        if self._ino is None or self._ino[0] != n:
            c = store.cols
            live = (c["kind"][:n] == Kind.DIR) & ((c["flags"][:n] & np.uint32(Flag.GONE)) == 0)
            ids = np.flatnonzero(live)
            key = c["ino"][:n][ids].astype(np.uint64) & np.uint64(REF_MASK)
            known = key != 0  # 0: no id (the Python lister on Windows does not read them)
            ids, key = ids[known], key[known]
            order = np.argsort(key, kind="stable")
            self._ino = (n, key[order], ids[order])
        _, keys, ids = self._ino
        if not len(keys):
            return None
        want = frn.astype(np.uint64) & np.uint64(REF_MASK)
        at = np.searchsorted(keys, want)
        hit = (
            (at < len(keys)) & (keys[np.minimum(at, max(len(keys) - 1, 0))] == want) if len(keys) else at < 0
        )
        found: np.ndarray = ids[at[hit]]
        return found

    async def _list_one(self, node: int, path: str) -> SliceResult | None:
        root_idx = self._root_index(path)
        if root_idx is None:
            return None
        opts = SliceOptions(roots=self._root_opts(), budget_s=0.0)  # this folder only
        task = (0, node, 0)
        res = await self._slice([(task, path, root_idx)], opts)
        if len(res.refs) != 1 or tuple(int(x) for x in res.refs[0]) != task:
            return None  # vanished (its parent lists it as gone) or no longer readable
        if any(tuple(ref) == task for ref, _path, _msg in res.errors):
            return None  # a folder a path cannot reach (a case-only sibling): stays as scanned
        # errors of its subfolders (names no path can open) are theirs: the listing stands, and
        # _relist marks them
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

            def resolve(ref: tuple[int, int, int], starts: list[int] = starts) -> int | None:
                """None: the folder left the tree while it was read (store.ingest_slice)."""
                kind, a, b = ref
                if kind == 0:
                    return None if store.cols["flags"][a] & np.uint32(Flag.GONE) else a
                return None if starts[a] < 0 else starts[a] + b

            for ref, _path, _msg in res.errors:
                if (target := resolve(ref)) is not None:
                    store.mark(target, Flag.ACCESS_DENIED)
                    store.clear(target, Flag.PENDING)
            for ref, path, idx in res.frontier:
                if (target := resolve(ref)) is not None:
                    queue.append(((0, target, 0), path, idx))

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
        # each root's path and name check still apply: a relist must not list a case-only
        # sibling's contents under the other's name
        return tuple(
            worker.RootOptions(path=r.path, verify_names=verify_names(r.volume)) for r in self.handle.roots
        )

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

    async def _refresh_free(self, now: float) -> bool:
        """Free space moves with the changes (status bar, free and unknown space of the views): read
        it again at most once every FREE_REFRESH_S. A change within that time is read once it is up
        (``_free_due``, the loop wakes for it), so the last change of a burst is not missed. True when
        a volume's free or used space changed."""
        if now - self._free_at < FREE_REFRESH_S:
            self._free_due = True
            return False
        self._free_at, self._free_due = now, False
        changed = False
        for root in self.handle.roots:
            if root.volume is not None and root.whole_volume:
                with contextlib.suppress(OSError):
                    fresh = await asyncio.to_thread(volume_for, root.path)
                    changed |= (fresh.free, fresh.used) != (root.volume.free, root.volume.used)
                    root.volume = fresh
        return changed

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


_SEPARATORS = tuple(s.encode() for s in (os.sep, os.altsep) if s)


def _unnamed(res: SliceResult) -> set[bytes]:
    """Names of the listing of ``res`` (one folder) no path stands for alone: held more than once,
    or with a separator in them (``PathIndex.unnamed``)."""
    seen: set[bytes] = set()
    out: set[bytes] = set()
    at = 0
    for n in res.name_lens.tolist():
        name = res.name_blob[at : at + n]
        at += n
        if name in seen or any(sep in name for sep in _SEPARATORS):
            out.add(name)
        seen.add(name)
    return out


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
        self.passive: dict[str, ScanMonitor] = {}  # rescans of scans that are not watched
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
        # The checkpoint asks the elevated helper, which answers one request at a time and may be
        # reading this very scan's MFT for half a minute: scan.start (and the window waiting for it)
        # must not wait for that. Until it is taken an overflow rescans instead of replaying.
        mon.usn_task = asyncio.get_running_loop().create_task(mon.start_usn(), name=f"usn-{handle.id}")

    async def stop(self, scan_id: str) -> None:
        passive = self.passive.pop(scan_id, None)
        if passive is not None:
            await passive.stop()
        mon = self.monitors.pop(scan_id, None)
        if mon is not None:
            mon.handle.monitor = None
            await mon.stop()

    async def close(self) -> None:
        for scan_id in {*self.monitors, *self.passive}:
            await self.stop(scan_id)

    async def rescan(self, handle: ScanHandle, node: int) -> None:
        """Read a folder again with everything below it (F5 「重新扫描当前视图」); with live
        monitoring off a monitor without watchers does it. Node 0, the top of a scan of several
        roots, reads every root again."""
        if handle.running:
            raise RpcError(ErrorCode.SCAN_BUSY, tr("扫描进行中，完成后再重新扫描"))
        mon = self.monitors.get(handle.id) or self.passive.get(handle.id)
        if mon is None:
            mon = ScanMonitor(self, handle, watch=False)
            await mon.start()
            self.passive[handle.id] = mon
        for n in [r.node for r in handle.roots] if node == 0 else [node]:
            mon.rescan(handle.store.path(n))

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

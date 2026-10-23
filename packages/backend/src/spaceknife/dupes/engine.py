"""The duplicate-file job (docs/dupes.md §2, §9): S0 → S1 → S1b → S2 → S3 → results.

A :class:`Job` runs on the event loop and dispatches every IO batch to a per-disk
:class:`DiskQueue` (its own threads, the disk's IO governor, one native reader). Groups of equal
size are taken in waves, largest possible reclaim first: one S1 batch identifies a wave's members
(metadata only), then each group goes on by itself:

1. merge names of one physical file (identity compared exactly), note hard links beyond the
   scans, drop cloud placeholders and names that cannot be used;
2. S1b on volumes that share blocks: complete equal block maps are "the same data" (never read);
3. the hash cache separates (quick and prefix digests) and joins (full digests, strong trust only);
4. S2: one data open, head and tail; the quick digests split the group;
5. S3: full BLAKE3 in steps, compared at 1/4/16/64/256 MiB and every 256 MiB, a member stopping as
   soon as nothing can still match it; groups of more than 32 go to the end member by member;
6. what is identical (whole content read, or joined from trusted cache entries) goes to the
   results sink (``results.Results``), which applies the keep rules.

Pause stops asking for permits, waits for the batches in flight and closes every handle (hashers
and offsets stay); resume reopens with the identity and stamps checked. Cancel waits for the
batches in flight, keeps what was learnt in the cache and leaves the rest undecided. A scan
starting on one of the job's disks makes that queue wait until it ends (the job never pauses the
governor). The governor level is set per disk while the job runs and put back afterwards.
"""

from __future__ import annotations

import asyncio
import contextlib
import heapq
import os
import sys
import threading
import time
from collections.abc import Awaitable, Callable, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal

import numpy as np
from loguru import logger

from ..io.priority import set_thread_low_io_priority
from ..ops.paths import current_rules, match_rules
from ..ops.trash import win32_trims
from ..scan.volume import VolumeInfo, volume_for
from ..store.lock import STRUCTURE
from . import codes as C
from .cache import Stamp, VolumeCache, join_ok, split_ok
from .identity import Volume, clonable, is_network, trust_of, volume_key
from .reads import Ops, Reader, default_ops
from .select import G_ACTIVE, G_DONE, PathCache, Scope, Selection, name_usable, select

if TYPE_CHECKING:
    from ..io.governor import IoGovernor
    from ..io.registry import IoRegistry

State = Literal[
    "planning", "identifying", "probing", "verifying", "waiting_scan", "paused", "done", "cancelled", "failed"
]
Level = Literal["max", "balanced", "quiet", "idle", "custom"]

S1_BATCH = 256
S2_BATCH = 32
WAVE_MEMBERS = 256
WAVE_GROUPS = 64
GROUPS_PER_DISK = 8
BATCH_S = {"hdd": 0.1, "usb_hdd": 0.1, "sd_card": 0.1}
STEP_BYTES = 4 << 20  # per member and batch (HDD 64 MiB)
STEP_BYTES_HDD = 64 << 20
SMALL_WHOLE = {"hdd": 1 << 20, "usb_hdd": 1 << 20}
THREADS = {
    "nvme_ssd": 8,
    "sata_ssd": 4,
    "ssd": 4,
    "virtual": 4,
    "hdd": 2,
    "usb_hdd": 2,
    "usb_flash": 2,
    "sd_card": 2,
}
ROTATIONAL = {"hdd", "usb_hdd", "sd_card"}
ATIME_FIRST, ATIME_EVERY = 32, 64
CACHE_SAVE_S = 60.0


class Stopped(Exception):
    """The job was cancelled or ran out of its byte or time budget."""


@dataclass(slots=True)
class JobOptions:
    load: Level = "quiet"
    load_custom: float | None = None
    keep_atime: Literal["skip", "read"] = "skip"
    budget_bytes: int | None = None  # stop once this many content bytes were read
    max_seconds: float | None = None
    use_cache: bool = True
    cache_root: Path | None = None
    racy_s: float = C.RACY_S
    unbuffered: bool = True
    stop_on_atime_moved: bool = False  # Verify: stop reading a volume once others moved access times
    threads: int | None = None
    # test seams: called with (job, group id) between S3 batches of a group / after S1 of a wave
    between: Callable[[Job, int], None] | None = None


@dataclass(slots=True)
class Counters:
    permits: int = 0
    io_calls: int = 0
    meta_ops: int = 0
    extent_ops: int = 0
    data_opens: int = 0
    reopens: int = 0
    bytes_probe: int = 0
    bytes_full: int = 0
    cache_hits: int = 0
    cache_joined: int = 0
    cache_separated: int = 0
    atime_unkept: int = 0
    atime_checked: int = 0
    atime_moved: int = 0
    racy: int = 0
    max_open: int = 0

    def as_dict(self) -> dict[str, int]:
        return {k: getattr(self, k) for k in self.__slots__}


@dataclass(slots=True)
class Funnel:
    candidates: int = 0
    identified: int = 0
    physical: int = 0
    aliases: int = 0
    clones: int = 0
    probed: int = 0
    after_probe: int = 0
    verifying: int = 0
    confirmed_groups: int = 0
    confirmed_files: int = 0
    shared_groups: int = 0
    unique: int = 0
    skipped: int = 0

    def as_dict(self) -> dict[str, int]:
        return {k: getattr(self, k) for k in self.__slots__}


class DiskQueue:
    """The IO of one physical disk: its governor, threads and reader."""

    def __init__(
        self,
        key: str,
        media: str,
        governor: IoGovernor | None,
        reader: Reader,
        threads: int,
        *,
        name: str = "",
    ) -> None:
        self.key = key
        self.name = name or key
        self.media = media
        self.governor = governor
        self.reader = reader
        self.pool = ThreadPoolExecutor(
            max_workers=max(1, threads), thread_name_prefix=f"spaceknife-dupes-{key[-12:]}"
        )
        self.batch_s = BATCH_S.get(media, 0.03)
        self.step_bytes = STEP_BYTES_HDD if media in ROTATIONAL else STEP_BYTES
        self.small_whole = SMALL_WHOLE.get(media, 128 << 10)
        self.rotational = media in ROTATIONAL
        self.saved_level: tuple[str, float | None] | None = None
        self.set_level: str | None = None
        self.waiting = False
        self.stopped = False  # content reads stopped (access times moved by others)

    def close(self) -> None:
        self.pool.shutdown(wait=False, cancel_futures=True)


@dataclass(slots=True)
class Unit:
    """One physical file of a group: its representative row and the other names / clones."""

    rep: int  # index into the group's member arrays
    names: list[int] = field(default_factory=list)  # all members (rep first)
    clones: list[int] = field(default_factory=list)  # representatives of units merged as clones
    flags: int = 0
    known: bytes | None = None  # a full digest (fresh, or a cache entry that may join)
    quick: int | None = None
    prefix: tuple[int, bytes] | None = None  # cached prefix (may separate)
    fresh_quick: bool = False
    probed: bool = False
    digest: bytes | None = None
    split: tuple[int, bytes] | None = None  # this run: the last checkpoint where its set split
    interim: tuple[int, bytes] | None = None  # this run: the last checkpoint reached
    racy: bool = False
    external: bool = False  # more hard links than names in the scans: may only be kept
    reason: int = 0
    queue: DiskQueue | None = None


class Job:
    def __init__(
        self,
        scope: Scope,
        *,
        options: JobOptions | None = None,
        ops: Ops | None = None,
        registry: IoRegistry | None = None,
        scan_busy: Callable[[str], bool] | None = None,
        sink: Any = None,
        on_progress: Callable[[Job], None] | None = None,
        compute: Callable[..., Awaitable[Any]] | None = None,
    ) -> None:
        self.scope = scope
        self.opts = options or JobOptions()
        self.ops: Ops = ops or default_ops()
        self.registry = registry
        self.scan_busy = scan_busy
        self.sink = sink
        self.on_progress = on_progress
        self._compute = compute
        self.state: State = "planning"
        self.error = ""
        self.sel: Selection | None = None
        self.counters = Counters()
        self.funnel = Funnel()
        self.skipped_by_reason: dict[str, int] = {}
        self.vols: list[Volume] = []
        self._vol_of_dev: dict[int, int] = {}
        self.queues: dict[str, DiskQueue] = {}
        self._root_queue: dict[tuple[int, int], DiskQueue] = {}
        self._caches: dict[int, VolumeCache | None] = {}
        self._paths = [PathCache(s.store) for s in scope.scans]
        self._running = asyncio.Event()
        self._running.set()
        self._cancelled = False
        self._stop_reason = ""
        self._inflight: set[asyncio.Future[Any]] = set()
        self._t0 = 0.0
        self._wall0 = 0
        self._cpu0 = 0.0
        self.elapsed = 0.0
        self.cpu_s = 0.0
        self._atime_seen: dict[int, int] = {}
        self._last_cache_save = 0.0
        self._racy_units: list[tuple[int, str, Stamp, int]] = []  # (queue-less) re-checked at the end
        self.current = ""
        self.done_groups = 0
        self._group_sem: asyncio.Semaphore | None = None
        self.waiting_for = ""
        self.peak_rss = 0
        self.rss0 = 0
        self.urgent: list[int] = []  # groups to take first (a file's own group)
        self._vol_lock = threading.Lock()

    # ------------------------------------------------------------------ control
    @property
    def paused(self) -> bool:
        return not self._running.is_set()

    @property
    def cancelled(self) -> bool:
        return self._cancelled

    async def pause(self) -> None:
        if self.state in ("done", "cancelled", "failed"):
            return
        self._running.clear()
        await self._drain()
        for q in self.queues.values():
            await asyncio.get_running_loop().run_in_executor(q.pool, q.reader.suspend, None)
        if not self._cancelled:
            self.state = "paused"
        self._emit()

    def resume(self) -> None:
        if self.state == "paused":
            self.state = "verifying"
        self._running.set()
        self._emit()

    async def cancel(self) -> None:
        self._cancelled = True
        self._stop_reason = self._stop_reason or "cancelled"
        self._running.set()
        await self._drain()

    async def _drain(self) -> None:
        while self._inflight:
            await asyncio.wait(list(self._inflight))

    def open_handles(self) -> int:
        return sum(q.reader.open_handles() for q in self.queues.values())

    def _check(self) -> None:
        if self._cancelled:
            raise Stopped(self._stop_reason)
        o = self.opts
        if (
            o.budget_bytes is not None
            and self.counters.bytes_full + self.counters.bytes_probe >= o.budget_bytes
        ):
            self._stop_reason = "budget"
            raise Stopped("budget")
        if o.max_seconds is not None and time.monotonic() - self._t0 >= o.max_seconds:
            self._stop_reason = "time"
            raise Stopped("time")

    async def _gate(self, q: DiskQueue) -> None:
        while True:
            self._check()
            if not self._running.is_set():
                await self._running.wait()
                continue
            if self.scan_busy is not None and self.scan_busy(q.key):
                q.waiting = True
                self.waiting_for = q.name
                prev = self.state
                self.state = "waiting_scan"
                self._emit()
                await self._wait_scan(q)
                q.waiting = False
                self.waiting_for = ""
                self.state = prev if prev != "waiting_scan" else "verifying"
                self._apply_level(q)
                continue
            return

    async def _wait_scan(self, q: DiskQueue) -> None:
        """Until no scan uses ``q``'s disk any more (polled: scans have no end event here)."""
        busy = self.scan_busy
        assert busy is not None
        poll = asyncio.Event()
        while busy(q.key) and not self._cancelled:
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(poll.wait(), 0.25)

    async def _run[T](self, q: DiskQueue, fn: Callable[..., T], *args: Any) -> T:
        """One batch on ``q``: through the gate, under a governor permit, on the queue's threads."""
        await self._gate(q)
        loop = asyncio.get_running_loop()
        idle = q.governor is not None and q.governor.level == "idle"
        if q.governor is not None:
            permit = await q.governor.permit()
            self.counters.permits += 1
            fut = loop.run_in_executor(q.pool, _with_priority, idle, fn, *args)
            permit.bind(fut)
        else:
            self.counters.permits += 1
            fut = loop.run_in_executor(q.pool, _with_priority, False, fn, *args)
        self.counters.io_calls += 1
        self._inflight.add(fut)
        try:
            return await asyncio.shield(fut)
        finally:
            self._inflight.discard(fut)
            self.counters.max_open = max(self.counters.max_open, self.open_handles())

    # ------------------------------------------------------------------ queues and volumes
    async def _queue_for(self, si: int, ri: int) -> DiskQueue:
        key = (si, ri)
        q = self._root_queue.get(key)
        if q is not None:
            return q
        root = self.scope.scans[si].roots[ri]
        gov = None
        disk_key = "local"
        media = "unknown"
        name = root.volume.mount_point if root.volume is not None else root.path
        if self.registry is not None and self.registry.running:
            gov = await self.registry.governor_for_path(root.path)
            disk_key = str(gov.disk.id)
            media = str(gov.disk.media)
        elif root.volume is not None:
            from ..scan.media import media_of

            media = await asyncio.to_thread(
                media_of, root.volume.mount_point, root.volume.device, root.volume.is_remote
            )
        q = self.queues.get(disk_key)
        if q is None:
            threads = self.opts.threads or THREADS.get(media, 4)
            if gov is not None:
                from ..io.governor import PROFILES

                cap = PROFILES.get(media, PROFILES["unknown"]).cap
                if cap:
                    threads = min(threads, cap)
            reader = self.ops.reader(
                unbuffered=self.opts.unbuffered,
                keep_atime=self.opts.keep_atime,
                small_whole=SMALL_WHOLE.get(media, 128 << 10),
                lease_s=2.0,
                chunk=C.CHUNK,
            )
            q = DiskQueue(disk_key, media, gov, reader, threads, name=name)
            self.queues[disk_key] = q
            self._apply_level(q)
        self._root_queue[key] = q
        return q

    def _apply_level(self, q: DiskQueue) -> None:
        gov = q.governor
        if gov is None or (self.scan_busy is not None and self.scan_busy(q.key)):
            return
        if q.saved_level is None:
            q.saved_level = (gov.level, getattr(gov.core, "custom", None))
        gov.set_level(self.opts.load, self.opts.load_custom)
        q.set_level = self.opts.load

    def _restore_levels(self) -> None:
        for q in self.queues.values():
            gov = q.governor
            if gov is None or q.saved_level is None:
                continue
            if gov.level == q.set_level and not (self.scan_busy is not None and self.scan_busy(q.key)):
                level, custom = q.saved_level
                gov.set_level(level, custom)  # type: ignore[arg-type]

    def set_load(self, level: Level, custom: float | None = None) -> None:
        self.opts.load = level
        self.opts.load_custom = custom
        for q in self.queues.values():
            self._apply_level(q)

    def _volume(self, dev: int, path: str, root_vol: VolumeInfo | None) -> int:
        """The job's index of the volume S1 reported ``dev`` for (created on first sight)."""
        with self._vol_lock:
            return self._volume_locked(dev, path, root_vol)

    def _volume_locked(self, dev: int, path: str, root_vol: VolumeInfo | None) -> int:
        ix = self._vol_of_dev.get(dev)
        if ix is not None:
            return ix
        info = root_vol
        if info is None or (info.dev and sys.platform != "win32" and info.dev != dev):
            try:
                info = volume_for(path)
            except OSError:
                info = root_vol
        mount = info.mount_point if info is not None else os.path.dirname(path)
        native = self.ops.alg == C.ALG_BLAKE3
        v = Volume(
            index=len(self.vols),
            dev=dev,
            key=volume_key(mount, dev),
            fs_type=(info.fs_type if info is not None else ""),
            trust=trust_of(info, native=native),
            clonable=clonable(info),
            network=is_network(info),
            mount=mount,
            label=info.label if info is not None else "",
            sample_path=path,
            removable=bool(info and info.is_removable),
        )
        if len(self.vols) >= 255:
            raise RuntimeError("too many volumes in one job")
        self.vols.append(v)
        self._vol_of_dev[dev] = v.index
        if self.opts.use_cache:
            try:
                self._caches[v.index] = VolumeCache.load(
                    v.key, self.ops.alg, C.PROBE_PARAMS, v.trust == "strong", self.opts.cache_root
                )
            except OSError:
                self._caches[v.index] = None
        else:
            self._caches[v.index] = None
        return v.index

    def cache_of(self, vol: int) -> VolumeCache | None:
        return self._caches.get(vol)

    # ------------------------------------------------------------------ run
    async def run(self) -> None:
        self._t0 = time.monotonic()
        self._wall0 = time.time_ns()
        self._cpu0 = time.process_time()
        self.rss0 = _rss()
        try:
            await self._run_all()
            if self._cancelled:
                self.state = "cancelled"
            else:
                self.state = "done"
        except Stopped:
            self.state = "cancelled" if self._stop_reason == "cancelled" else "done"
        except Exception as exc:
            logger.exception("dupes job failed")
            self.state = "failed"
            self.error = str(exc)
        finally:
            await self._drain()
            for q in self.queues.values():
                with contextlib.suppress(Exception):
                    await asyncio.get_running_loop().run_in_executor(q.pool, q.reader.drop, None)
            self._mark_stopped()
            await self._recheck_racy()
            await asyncio.to_thread(self._save_caches)
            self._restore_levels()
            self._collect_reader_counters()
            for q in self.queues.values():
                q.close()
            self.elapsed = time.monotonic() - self._t0
            self.cpu_s = time.process_time() - self._cpu0
            self.peak_rss = max(self.peak_rss, _rss())
            if self.sink is not None:
                self.sink.finish(self)
            self._emit(force=True)

    async def _select(self) -> Selection:
        if self._compute is not None:
            sel: Selection = await self._compute(select, self.scope)
            return sel

        def under_lock() -> Selection:
            with STRUCTURE.read():
                return select(self.scope)

        return await asyncio.to_thread(under_lock)

    async def _run_all(self) -> None:
        self.state = "planning"
        sel = await self._select()
        self.sel = sel
        self.funnel.candidates = len(sel)
        if self.sink is not None:
            self.sink.start(self)
        self._emit()
        groups = sel.groups
        order = np.argsort(-((groups["count"].astype(np.int64) - 1) * groups["size"]), kind="stable")
        heap: list[tuple[int, int]] = [(i, int(g)) for i, g in enumerate(order)]
        heapq.heapify(heap)
        n_disks = max(1, len({self._root_disk_hint(s, r) for s, r in self._roots_in_use(sel)}))
        self._group_sem = asyncio.Semaphore(GROUPS_PER_DISK * n_disks)
        tasks: set[asyncio.Task[None]] = set()
        self.state = "identifying"
        try:
            while heap or self.urgent:
                self._check()
                wave: list[int] = []
                members = 0
                while self.urgent and len(wave) < WAVE_GROUPS:
                    g = self.urgent.pop(0)
                    if groups["state"][g] == 0:
                        wave.append(g)
                        members += int(groups["count"][g])
                while heap and len(wave) < WAVE_GROUPS and members < WAVE_MEMBERS:
                    _, g = heapq.heappop(heap)
                    if groups["state"][g] != 0:
                        continue
                    wave.append(g)
                    members += int(groups["count"][g])
                if not wave:
                    continue
                for g in wave:
                    groups["state"][g] = G_ACTIVE
                ident = await self._identify(wave)
                if self.opts.between is not None:
                    self.opts.between(self, -1)
                for g in wave:
                    assert self._group_sem is not None
                    await self._group_sem.acquire()
                    self._check()
                    t = asyncio.create_task(self._group_task(g, ident.pop(g)), name=f"dupes-group-{g}")
                    tasks.add(t)
                    t.add_done_callback(tasks.discard)
                # surface failures early
                for t in [t for t in tasks if t.done()]:
                    t.result()
                self.state = "verifying"
                self._emit()
                self._maybe_save()
            while tasks:
                done, _ = await asyncio.wait(list(tasks), return_when=asyncio.FIRST_EXCEPTION)
                for t in done:
                    t.result()
        except BaseException:
            for t in tasks:
                t.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            raise

    def _roots_in_use(self, sel: Selection) -> set[tuple[int, int]]:
        if not len(sel.cand):
            return set()
        pairs = np.unique(sel.cand["scan"].astype(np.int64) * 256 + sel.root_of.astype(np.int64))
        return {(int(p) // 256, int(p) % 256) for p in pairs}

    def _root_disk_hint(self, si: int, ri: int) -> str:
        r = self.scope.scans[si].roots[ri]
        return r.volume.device if r.volume is not None else r.path

    # ------------------------------------------------------------------ S1
    async def _identify(self, wave: list[int]) -> dict[int, dict[str, Any]]:
        """S1 of every member of a wave of groups, per disk queue in batches of 256."""
        sel = self.sel
        assert sel is not None
        rows = np.concatenate(
            [np.arange(sel.groups["first"][g], sel.groups["first"][g] + sel.groups["count"][g]) for g in wave]
        )
        by_q: dict[DiskQueue, list[int]] = {}
        for r in rows.tolist():
            q = await self._queue_for(int(sel.cand["scan"][r]), int(sel.root_of[r]))
            by_q.setdefault(q, []).append(r)
        out: dict[int, dict[str, Any]] = {}
        jobs = []
        for q, rs in by_q.items():
            for a in range(0, len(rs), S1_BATCH):
                jobs.append(self._run(q, self._s1_batch, rs[a : a + S1_BATCH]))
        results = await asyncio.gather(*jobs)
        merged: dict[int, tuple[str, dict[str, Any]]] = {}
        for res in results:
            for r, path, row in res:
                merged[r] = (path, row)
        for g in wave:
            first, count = int(sel.groups["first"][g]), int(sel.groups["count"][g])
            out[g] = {
                "rows": list(range(first, first + count)),
                "info": [merged[r] for r in range(first, first + count)],
            }
        self.counters.meta_ops += len(rows)
        self.funnel.identified += len(rows)
        return out

    def _s1_batch(self, rows: list[int]) -> list[tuple[int, str, dict[str, Any]]]:
        """On a queue thread: paths (under the structure lock), then metadata of each name."""
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        paths: list[str] = []
        with STRUCTURE.read():
            for r in rows:
                si = int(cand["scan"][r])
                paths.append(self._paths[si].path(int(cand["node"][r])))
        usable = [name_usable(p) for p in paths]
        call = [p for p, ok in zip(paths, usable, strict=True) if ok]
        want_usn = self.ops.has_usn
        ident = self.ops.identity(call, want_usn) if call else {}
        out: list[tuple[int, str, dict[str, Any]]] = []
        j = 0
        for r, p, ok in zip(rows, paths, usable, strict=True):
            if not ok:
                out.append((r, p, {"reason": C.UNNAMEABLE}))
                continue
            row = {k: v[j].item() for k, v in ident.items()}
            j += 1
            if row["reason"] == C.OK:
                si = int(cand["scan"][r])
                root = self.scope.scans[si].roots[int(sel.root_of[r])]
                row["vol"] = self._volume(int(row["dev"]), p, root.volume)
            out.append((r, p, row))
        return out

    # ------------------------------------------------------------------ one group
    async def _group_task(self, g: int, data: dict[str, Any]) -> None:
        try:
            await self._group(g, data)
        except Stopped:
            pass
        finally:
            sel = self.sel
            assert sel is not None
            sel.groups["state"][g] = G_DONE
            self.done_groups += 1
            assert self._group_sem is not None
            self._group_sem.release()
            self._emit()

    def _skip(self, r: int, reason: int) -> None:
        sel = self.sel
        assert sel is not None
        sel.cand["state"][r] = C.ST_SKIPPED
        sel.cand["reason"][r] = reason
        name = C.REASON_NAMES.get(reason, "read_error")
        self.skipped_by_reason[name] = self.skipped_by_reason.get(name, 0) + 1
        self.funnel.skipped += 1

    async def _group(self, g: int, data: dict[str, Any]) -> None:
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        size = int(sel.groups["size"][g])
        rows: list[int] = data["rows"]
        info: list[tuple[str, dict[str, Any]]] = data["info"]
        rules = current_rules()
        # 1. usable members, merged by identity
        live: list[int] = []  # positions in rows
        for i, (path, row) in enumerate(info):
            r = rows[i]
            reason = int(row.get("reason", C.READ_ERROR))
            if reason == C.OK and int(row["size"]) != size:
                reason = C.CHANGED  # the size changed since the scan: next run
            if reason != C.OK:
                self._skip(r, reason)
                continue
            cand["fid_lo"][r] = row["fid_lo"]
            cand["fid_hi"][r] = row["fid_hi"]
            cand["nlink"][r] = min(int(row["nlink"]), 65535)
            cand["vol"][r] = row["vol"]
            fl = 0
            if self.vols[int(row["vol"])].network:
                fl |= C.F_NETWORK
            if win32_trims(path):
                fl |= C.F_PINNED
            if match_rules(path, rules) is not None:
                fl |= C.F_NEEDED
            cand["flags"][r] = fl
            live.append(i)
        units: dict[tuple[int, int, int], Unit] = {}
        for i in live:
            r = rows[i]
            key = (int(cand["vol"][r]), int(cand["fid_hi"][r]), int(cand["fid_lo"][r]))
            u = units.get(key)
            if u is None:
                units[key] = Unit(rep=i, names=[i])
            else:
                u.names.append(i)
                cand["state"][r] = C.ST_ALIAS
                self.funnel.aliases += 1
        ulist = list(units.values())
        for u in ulist:
            r = rows[u.rep]
            nl = int(info[u.rep][1]["nlink"])
            if nl > len(u.names):
                u.external = True  # other names outside the scans: deleting this one frees nothing
            for i in u.names:
                u.flags |= int(cand["flags"][rows[i]])
        self.funnel.physical += len(ulist)
        # 2. block maps on volumes that share blocks
        ulist = await self._clones(g, rows, info, ulist)
        if len(ulist) < 2:
            if any(len(u.names) > 1 or u.clones for u in ulist):
                self._record(g, size, None, ulist, rows, info, shared=True)
            self._finish_rows(rows)
            return
        # 3. the cache
        self._from_cache(rows, info, ulist)
        # 4. probe what the cache could not settle
        need_probe = [u for u in ulist if u.quick is None]
        await self._probe(g, size, rows, info, need_probe)
        ulist = [u for u in ulist if u.reason == 0]
        parts = _partition(ulist, lambda u: u.quick)
        for part in parts:
            if len(part) < 2:
                for u in part:
                    self._unique(rows, info, u, size)
                continue
            part = self._separate_by_prefix(rows, info, part, size)
            if len(part) < 2:
                for u in part:
                    self._unique(rows, info, u, size)
                continue
            readers = [u for u in part if u.known is None]
            if readers:
                fresh = [u for u in readers if not u.probed]
                if fresh:
                    await self._probe(g, size, rows, info, fresh)
                    part = [u for u in part if u.reason == 0]
                    # a re-probe may have shown a different quick digest: partition again
                    sub = _partition(part, lambda u: u.quick)
                    part = []
                    for s in sub:
                        if len(s) < 2:
                            for u in s:
                                self._unique(rows, info, u, size)
                        else:
                            part.extend(s)
                    if len(part) < 2:
                        continue
                await self._verify(g, size, rows, info, part)
            self._conclude(g, size, rows, info, [u for u in part if u.reason == 0])
        await self._sample_atime(rows, info, ulist)

    async def _clones(self, g: int, rows: list[int], info: list[Any], ulist: list[Unit]) -> list[Unit]:
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        by_vol: dict[int, list[Unit]] = {}
        for u in ulist:
            v = int(cand["vol"][rows[u.rep]])
            if self.vols[v].clonable:
                by_vol.setdefault(v, []).append(u)
        out = list(ulist)
        for v, us in by_vol.items():
            if len(us) < 2:
                continue
            q = await self._queue_for(int(cand["scan"][rows[us[0].rep]]), int(sel.root_of[rows[us[0].rep]]))
            paths = [info[u.rep][0] for u in us]
            res = await self._run(q, self.ops.extents, paths)
            self.counters.extent_ops += len(paths)
            if res is None:
                self.vols[v].notes.append("may_share")
                continue
            hashes = res["hash"]
            by_map: dict[bytes, list[Unit]] = {}
            for j, u in enumerate(us):
                if int(res["reason"][j]) != C.OK or not int(res["complete"][j]) or not int(res["extents"][j]):
                    continue
                by_map.setdefault(bytes(hashes[16 * j : 16 * j + 16]), []).append(u)
            for same in by_map.values():
                if len(same) < 2:
                    continue
                head = same[0]
                for u in same[1:]:
                    head.clones.append(u.rep)
                    head.names.extend(u.names)
                    head.flags |= u.flags
                    for i in u.names:
                        cand["state"][rows[i]] = C.ST_CLONE
                    self.funnel.clones += 1
                    out.remove(u)
        return out

    def _stamp(self, row: dict[str, Any]) -> Stamp:
        return Stamp(int(row["size"]), int(row["mtime"]), int(row["change"]), int(row.get("usn", 0)))

    def _from_cache(self, rows: list[int], info: list[Any], ulist: list[Unit]) -> None:
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        by_vol: dict[int, list[Unit]] = {}
        for u in ulist:
            by_vol.setdefault(int(cand["vol"][rows[u.rep]]), []).append(u)
        for v, us in by_vol.items():
            vc = self.cache_of(v)
            if vc is None or not len(vc):
                continue
            hi = np.array([int(cand["fid_hi"][rows[u.rep]]) for u in us], dtype=np.uint64)
            lo = np.array([int(cand["fid_lo"][rows[u.rep]]) for u in us], dtype=np.uint64)
            hits = vc.lookup(hi, lo)
            vc.touch(hits.tolist())
            for u, h in zip(us, hits.tolist(), strict=True):
                if h < 0:
                    continue
                e = vc.entry(h)
                st = self._stamp(info[u.rep][1])
                if not split_ok(e, st):
                    continue
                self.counters.cache_hits += 1
                cand["flags"][rows[u.rep]] |= C.F_CACHE_HIT
                flags = int(e["flags"])
                if flags & 8:  # E_QUICK
                    u.quick = int(e["quick"])
                if flags & 4:  # E_PREFIX
                    u.prefix = (int(e["p_off"]), bytes(e["p_dig"]))
                if join_ok(vc, e, st, want_usn=st.usn != 0):
                    u.known = bytes(e["full"])

    async def _probe(self, g: int, size: int, rows: list[int], info: list[Any], units: list[Unit]) -> None:
        """S2 for these units (batches of 32 per queue)."""
        if not units:
            return
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        by_q: dict[DiskQueue, list[Unit]] = {}
        for u in units:
            r = rows[u.rep]
            q = await self._queue_for(int(cand["scan"][r]), int(sel.root_of[r]))
            if q.stopped:
                u.reason = C.STOPPED
                self._skip(r, C.STOPPED)
                continue
            by_q.setdefault(q, []).append(u)
        calls = []
        big = len(units) > C.LARGE_GROUP  # S3 takes them 32 at a time: no handle waits for it
        for q, us in by_q.items():
            for a in range(0, len(us), S2_BATCH):
                calls.append(self._probe_batch(q, size, rows, info, us[a : a + S2_BATCH], big))
        await asyncio.gather(*calls)
        self.funnel.probed += len(units)

    async def _probe_batch(
        self, q: DiskQueue, size: int, rows: list[int], info: list[Any], us: list[Unit], big: bool
    ) -> None:
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        n = len(us)
        keys = np.array([rows[u.rep] for u in us], dtype=np.uint64)
        paths = [info[u.rep][0] for u in us]
        dev = np.array([int(info[u.rep][1]["dev"]) for u in us], dtype=np.uint64)
        lo = np.array([int(info[u.rep][1]["fid_lo"]) for u in us], dtype=np.uint64)
        hi = np.array([int(info[u.rep][1]["fid_hi"]) for u in us], dtype=np.uint64)
        sz = np.full(n, size, dtype=np.int64)
        attrs = np.array([int(info[u.rep][1].get("attrs", 0)) for u in us], dtype=np.uint32)
        hand_off = not q.rotational and not big
        wall = time.time_ns()
        res = await self._run(q, q.reader.probe, keys, paths, dev, lo, hi, sz, attrs, hand_off)
        self.counters.data_opens += n
        racy_ns = int(self.opts.racy_s * 1e9)
        for j, u in enumerate(us):
            r = rows[u.rep]
            reason = int(res["reason"][j])
            self.counters.bytes_probe += int(res["bytes"][j])
            if int(res["atime_unkept"][j]):
                self.counters.atime_unkept += 1
            if reason != C.OK:
                u.reason = reason
                for i in u.names:
                    self._skip(rows[i], reason)
                continue
            u.quick = int(res["quick"][j])
            u.fresh_quick = True
            u.probed = True
            u.queue = q
            cand["quick"][r] = u.quick & ((1 << 64) - 1)
            cand["state"][r] = C.ST_PROBED
            m, c = int(res["mtime"][j]), int(res["change"][j])
            if max(m, c) >= wall - racy_ns:
                u.racy = True
                self.counters.racy += 1
            if int(res["has_full"][j]):
                u.digest = bytes(res["full"][32 * j : 32 * j + 32])
                u.known = u.digest

    def _separate_by_prefix(
        self, rows: list[int], info: list[Any], part: list[Unit], size: int
    ) -> list[Unit]:
        """Drop the units the cache's prefix digests tell apart from every other unit."""
        if not any(u.prefix for u in part):
            return part
        keep: list[Unit] = []
        for u in part:
            if u.known is not None or u.prefix is None:
                keep.append(u)
                continue
            apart = all(
                v.prefix is not None and v.prefix[0] == u.prefix[0] and v.prefix[1] != u.prefix[1]
                for v in part
                if v is not u
            )
            if apart:
                self.counters.cache_separated += 1
                self._unique(rows, info, u, size)
            else:
                keep.append(u)
        return keep

    async def _verify(self, g: int, size: int, rows: list[int], info: list[Any], part: list[Unit]) -> None:
        """S3 over the units of one quick-digest partition that have no known digest."""
        known = [u for u in part if u.known is not None]
        work: list[tuple[list[Unit], int]] = [([u for u in part if u.known is None], 0)]
        cps = C.checkpoints(size)
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        self.funnel.verifying += len(work[0][0])
        for u in work[0][0]:
            cand["state"][rows[u.rep]] = C.ST_VERIFYING
        while work:
            active, ci = work.pop()
            active = [u for u in active if u.reason == 0]
            if not active:
                continue
            cp = cps[ci]
            to_end = cp == size or (len(active) > C.LARGE_GROUP and ci > 0)
            target = size if to_end else cp
            await self._step_all(g, rows, active, target, big=len(active) > C.LARGE_GROUP)
            active = [u for u in active if u.reason == 0]
            if to_end:
                continue  # digests set: compared in _conclude
            # split at this checkpoint
            by: dict[bytes, list[Unit]] = {}
            for u in active:
                by.setdefault(u.interim[1] if u.interim else b"", []).append(u)
            if len(by) > 1:  # remembered: a later run tells these apart without reading
                for u in active:
                    u.split = u.interim
            for dig, us in by.items():
                partners = [k for k in known if not (k.prefix and k.prefix[0] == cp and k.prefix[1] != dig)]
                if len(us) == 1 and not partners:
                    self._drop_reader(rows, us[0])
                    self._unique(rows, info, us[0], size)
                    continue
                if len(us) == 1 and partners:
                    work.append((us, len(cps) - 1))  # read to the end to compare with known digests
                else:
                    work.append((us, ci + 1))

    async def _step_all(self, g: int, rows: list[int], active: list[Unit], target: int, *, big: bool) -> None:
        """Steps every unit to ``target`` (per queue, in batches; big sets 32 at a time, closing
        handles in between so that no more than 32 are open)."""
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        by_q: dict[DiskQueue, list[Unit]] = {}
        for u in active:
            r = rows[u.rep]
            q = await self._queue_for(int(cand["scan"][r]), int(sel.root_of[r]))
            by_q.setdefault(q, []).append(u)

        async def run_queue(q: DiskQueue, us: list[Unit]) -> None:
            chunks = [us[a : a + C.LARGE_GROUP] for a in range(0, len(us), C.LARGE_GROUP)] if big else [us]
            for chunk in chunks:
                pending = list(chunk)
                while pending:
                    if q.stopped:
                        for u in pending:
                            u.reason = C.STOPPED
                            self._skip(rows[u.rep], C.STOPPED)
                        return
                    keys = np.array([rows[u.rep] for u in pending], dtype=np.uint64)
                    until = np.full(len(pending), target, dtype=np.uint64)
                    max_bytes = min(32 << 20, q.step_bytes * len(pending))
                    res = await self._run(q, q.reader.step, keys, until, max_bytes, q.batch_s)
                    if self.opts.between is not None:
                        self.opts.between(self, g)
                    nxt: list[Unit] = []
                    for j, u in enumerate(pending):
                        self.counters.bytes_full += int(res["bytes"][j])
                        self.counters.reopens += int(res["reopens"][j])
                        reason = int(res["reason"][j])
                        if reason != C.OK:
                            u.reason = reason
                            for i in u.names:
                                self._skip(rows[i], reason)
                            continue
                        at = int(res["at"][j])
                        if at == 2:
                            u.digest = bytes(res["fin"][32 * j : 32 * j + 32])
                        elif at == 1:
                            u.interim = (target, bytes(res["interim"][16 * j : 16 * j + 16]))
                        else:
                            nxt.append(u)
                    pending = nxt
                if big:
                    keys_l = [rows[u.rep] for u in chunk if u.reason == 0 and u.digest is None]
                    if keys_l:
                        await asyncio.get_running_loop().run_in_executor(q.pool, q.reader.suspend, keys_l)

        await asyncio.gather(*(run_queue(q, us) for q, us in by_q.items()))

    def _drop_reader(self, rows: list[int], u: Unit) -> None:
        """Forget a member that needs no more reads (closes its handle, on its queue's thread)."""
        q = u.queue
        if q is None:
            return
        u.queue = None
        fut = asyncio.get_running_loop().run_in_executor(q.pool, q.reader.drop, [rows[u.rep]])
        self._inflight.add(fut)
        fut.add_done_callback(self._inflight.discard)

    def _unique(self, rows: list[int], info: list[Any], u: Unit, size: int) -> None:
        sel = self.sel
        assert sel is not None
        for i in u.names:
            sel.cand["state"][rows[i]] = C.ST_UNIQUE
        self.funnel.unique += 1
        if u.probed and u.digest is None:
            self._drop_reader(rows, u)
        self._cache_put(rows, info, u)

    def _conclude(self, g: int, size: int, rows: list[int], info: list[Any], part: list[Unit]) -> None:
        by: dict[bytes, list[Unit]] = {}
        for u in part:
            d = u.digest if u.digest is not None else u.known
            if d is None:
                continue
            by.setdefault(d, []).append(u)
        for d, us in by.items():
            for u in us:
                self._cache_put(rows, info, u)
            if len(us) < 2:
                for u in us:
                    self._unique(rows, info, u, size)
                continue
            if all(u.known is not None and u.digest is None for u in us):
                self.counters.cache_joined += len(us)
            for u in us:
                for i in u.names:
                    self.sel.cand["state"][rows[i]] = C.ST_CONFIRMED  # type: ignore[union-attr]
            self._record(g, size, d, us, rows, info, shared=False)
            if any(u.racy for u in us):
                for u in us:
                    self._racy_units.append((g, info[u.rep][0], self._stamp(info[u.rep][1]), rows[u.rep]))

    def _cache_put(self, rows: list[int], info: list[Any], u: Unit) -> None:
        if not u.fresh_quick and u.digest is None and u.split is None:
            return
        sel = self.sel
        assert sel is not None
        r = rows[u.rep]
        vc = self.cache_of(int(sel.cand["vol"][r]))
        if vc is None:
            return
        row = info[u.rep][1]
        full = u.digest if u.digest is not None else None
        if full is None and u.known is not None and not u.probed:
            return  # already in the cache as it is
        prefix = None
        if u.split is not None and u.split[0] < int(row["size"]):
            prefix = (u.split[0], u.split[1])
        elif u.prefix is not None and full is None:
            prefix = u.prefix
        vc.put(
            int(row["fid_hi"]),
            int(row["fid_lo"]),
            self._stamp(row),
            quick=u.quick,
            full=full,
            prefix=prefix,
            racy=u.racy,
        )

    def _record(
        self,
        g: int,
        size: int,
        digest: bytes | None,
        units: list[Unit],
        rows: list[int],
        info: list[Any],
        *,
        shared: bool,
    ) -> None:
        sel = self.sel
        assert sel is not None
        if shared:
            self.funnel.shared_groups += 1
        else:
            self.funnel.confirmed_groups += 1
            self.funnel.confirmed_files += len(units)
        if self.sink is None:
            return
        cand = sel.cand
        members: list[dict[str, Any]] = []
        for ui, u in enumerate(units):
            for k, i in enumerate(u.names):
                r = rows[i]
                row = info[i][1]
                kind = "rep" if k == 0 else ("clone" if i in u.clones else "alias")
                members.append(
                    {
                        "unit": ui,
                        "kind": kind,
                        "scan": int(cand["scan"][r]),
                        "node": int(cand["node"][r]),
                        "vol": int(cand["vol"][r]),
                        "fid_lo": int(row["fid_lo"]),
                        "fid_hi": int(row["fid_hi"]),
                        "mtime": int(row["mtime"]),
                        "change": int(row["change"]),
                        "usn": int(row.get("usn", 0)),
                        "btime": int(row.get("btime", 0)),
                        "flags": int(cand["flags"][r]) | (u.flags if k == 0 else 0),
                        "path": info[i][0],
                        "external": u.external,
                    }
                )
        self.sink.add_group(self, g, size, digest, members, shared=shared)

    def _finish_rows(self, rows: list[int]) -> None:
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        for r in rows:
            if cand["state"][r] in (C.ST_SELECTED, C.ST_PROBED, C.ST_VERIFYING):
                cand["state"][r] = C.ST_UNIQUE

    async def _sample_atime(self, rows: list[int], info: list[Any], ulist: list[Unit]) -> None:
        """After the reads of a group: the access time of a sample of the files read, again."""
        sel = self.sel
        assert sel is not None
        cand = sel.cand
        picks: dict[DiskQueue, list[int]] = {}
        for u in ulist:
            if not u.probed:
                continue
            v = int(cand["vol"][rows[u.rep]])
            n = self._atime_seen.get(v, 0) + 1
            self._atime_seen[v] = n
            if n <= ATIME_FIRST or n % ATIME_EVERY == 0:
                r = rows[u.rep]
                q = await self._queue_for(int(cand["scan"][r]), int(sel.root_of[r]))
                picks.setdefault(q, []).append(u.rep)
        for q, reps in picks.items():
            paths = [info[i][0] for i in reps]
            res = await self._run(q, self.ops.identity, paths, False)
            self.counters.meta_ops += len(paths)
            for j, i in enumerate(reps):
                if int(res["reason"][j]) != C.OK:
                    continue
                self.counters.atime_checked += 1
                if int(res["atime"][j]) != int(info[i][1]["atime"]):
                    self.counters.atime_moved += 1
                    if self.opts.stop_on_atime_moved:
                        q.stopped = True

    # ------------------------------------------------------------------ the end
    def _mark_stopped(self) -> None:
        sel = self.sel
        if sel is None:
            return
        undecided = np.isin(sel.cand["state"], (C.ST_SELECTED, C.ST_PROBED, C.ST_VERIFYING))
        undecided &= sel.groups["state"][np.maximum(sel.cand["group"], 0)] != G_DONE
        if self._stop_reason and undecided.any():
            n = int(undecided.sum())
            self.skipped_by_reason["stopped"] = self.skipped_by_reason.get("stopped", 0) + n

    async def _recheck_racy(self) -> None:
        """Files modified shortly before they were read: their stamps once more at the end."""
        if not self._racy_units or self.sink is None:
            return
        paths = [p for _, p, _, _ in self._racy_units]
        try:
            res = await asyncio.to_thread(self.ops.identity, paths, self.ops.has_usn)
        except OSError:
            return
        changed: list[int] = []
        for j, (_g, _p, st, r) in enumerate(self._racy_units):
            now = Stamp(int(res["size"][j]), int(res["mtime"][j]), int(res["change"][j]), int(res["usn"][j]))
            if int(res["reason"][j]) != C.OK or (now.size, now.mtime, now.change) != (
                st.size,
                st.mtime,
                st.change,
            ):
                changed.append(r)
        if changed:
            self.sink.invalidate_rows(self, changed)

    def _maybe_save(self) -> None:
        if time.monotonic() - self._last_cache_save >= CACHE_SAVE_S and self._last_cache_save:
            self._last_cache_save = time.monotonic()
            asyncio.get_running_loop().run_in_executor(None, self._save_caches)
        elif not self._last_cache_save:
            self._last_cache_save = time.monotonic()

    def _save_caches(self) -> None:
        for vc in self._caches.values():
            if vc is not None and vc.dirty:
                try:
                    vc.save()
                except OSError as exc:
                    logger.warning("dupes cache not saved: {}", exc)

    def _collect_reader_counters(self) -> None:
        pass

    # ------------------------------------------------------------------ status
    def _emit(self, force: bool = False) -> None:
        self.peak_rss = max(self.peak_rss, _rss())
        if self.on_progress is not None:
            self.on_progress(self)

    def progress(self) -> float | None:
        sel = self.sel
        if sel is None or not len(sel.groups):
            return 1.0 if self.state == "done" else None
        return self.done_groups / len(sel.groups)

    def reader_counters(self) -> dict[str, int]:
        out: dict[str, int] = {}
        for q in self.queues.values():
            for k, v in q.reader.counters().items():
                out[k] = out.get(k, 0) + int(v)
        return out


def _with_priority[T](idle: bool, fn: Callable[..., T], *args: Any) -> T:
    if not idle:
        return fn(*args)
    set_thread_low_io_priority(True)
    try:
        return fn(*args)
    finally:
        set_thread_low_io_priority(False)


def _partition(units: Sequence[Unit], key: Callable[[Unit], Any]) -> list[list[Unit]]:
    by: dict[Any, list[Unit]] = {}
    for u in units:
        by.setdefault(key(u), []).append(u)
    return list(by.values())


def _rss() -> int:
    """Peak resident set of this process so far, bytes (0 where unknown)."""
    try:
        if sys.platform == "win32":  # pragma: no cover - Windows
            import ctypes
            from ctypes import wintypes

            class PMC(ctypes.Structure):
                _fields_ = (
                    ("cb", wintypes.DWORD),
                    ("PageFaultCount", wintypes.DWORD),
                    ("PeakWorkingSetSize", ctypes.c_size_t),
                    ("WorkingSetSize", ctypes.c_size_t),
                    ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                    ("PagefileUsage", ctypes.c_size_t),
                    ("PeakPagefileUsage", ctypes.c_size_t),
                )

            pmc = PMC()
            pmc.cb = ctypes.sizeof(PMC)
            ctypes.windll.psapi.GetProcessMemoryInfo(  # type: ignore[attr-defined,unused-ignore]
                ctypes.windll.kernel32.GetCurrentProcess(),  # type: ignore[attr-defined,unused-ignore]
                ctypes.byref(pmc),
                pmc.cb,
            )
            return int(pmc.PeakWorkingSetSize)
        import resource

        peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        return int(peak) * (1 if sys.platform == "darwin" else 1024)
    except Exception:
        return 0

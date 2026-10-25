"""Scan coordinator: hands directories to workers, folds their results into the node store.

The coordinator is the store's single writer and runs on the asyncio loop. A batch of directories
is listed by a *slicer*: the Python worker in a spawn-context process pool (Python 3.12 threads
would slow stat-heavy listing down: GIL convoy), or the native walker on plain threads (it releases
the GIL). ``executor=None`` lists in one thread (small scans, tests).

With an I/O governor (:mod:`spaceknife.io`) every batch waits for a permit: the governor bounds
the batches in flight and paces them so the disk load stays under the limit. Pausing stops the
dispatch; batches in flight finish (within their time budget), and the scan continues from its
queue on resume.
"""

from __future__ import annotations

import asyncio
import contextlib
import multiprocessing
import os
import sys
import time
from collections import deque
from collections.abc import Callable
from concurrent.futures import Executor, ProcessPoolExecutor
from concurrent.futures.process import BrokenProcessPool
from dataclasses import dataclass, field
from datetime import UTC, datetime
from enum import StrEnum
from typing import TYPE_CHECKING, Any

import numpy as np
from loguru import logger

from ..privacy import inside, protected_folders
from ..store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore
from . import worker
from .volume import VolumeInfo, is_volume_root, volume_for, volume_group_twins
from .worker import RootOptions, SliceOptions, SliceResult, Task

if TYPE_CHECKING:
    from ..io import IoGovernor, Permit

MAX_RECORDED_DENIED = 200


class ScanError(Exception):
    pass


class ScanState(StrEnum):
    QUEUED = "queued"
    SCANNING = "scanning"
    DONE = "done"
    CANCELLED = "cancelled"
    FAILED = "failed"


@dataclass(slots=True)
class RootInfo:
    path: str
    node: int
    volume: VolumeInfo | None
    whole_volume: bool


RATE_WINDOW_S = 4.0  # sliding window of the item / byte rates
ESTIMATE_AFTER_S = 3.0  # no remaining-time estimate before this much data
BYTES_CEILING = 0.95  # past this share of the used space a byte-based progress is a guess (progress())


@dataclass(slots=True)
class ScanProgress:
    state: ScanState
    items: int
    files: int
    dirs: int
    bytes_alloc: int
    bytes_size: int
    errors: int
    pending_dirs: int
    current_path: str
    elapsed_s: float  # every run of the scan (stopped, then continued), pauses included
    rate: float
    progress: float | None
    started_at: datetime | None
    finished_at: datetime | None
    denied: list[str] = field(default_factory=list)
    message: str = ""
    byte_rate: float = 0.0  # allocated bytes per second (same window as ``rate``)
    remaining_s: float | None = None
    expected_items: int | None = None
    paused: bool = False


def _report(on_progress: Callable[[ScanProgress], None], progress: ScanProgress) -> None:
    """Progress reporting is best effort: a failing callback never fails or aborts the scan."""
    try:
        on_progress(progress)
    except Exception:
        logger.exception("scan progress callback failed")


def default_workers() -> int:
    return max(2, min(8, (os.cpu_count() or 4) - 1))


NATIVE_BATCH = 256  # directories per native call (one listing thread each)
NATIVE_BUDGET_S = 0.05  # seconds a native call lists before it returns what it has


def native_threads() -> int:
    """Listing threads for the native walker: one per CPU, at most 16 (beyond that NTFS's locks only
    add contention). On macOS twice the CPUs: APFS listings wait in the kernel often enough that the
    extra threads keep the CPUs busy (GitHub's macOS runners, 3-CPU arm64 and 4-CPU Intel: +5 % to
    +15 %; many more starve the main thread that folds the results; docs/packaging.md).
    SPACEKNIFE_SCAN_THREADS sets it (1–64) for measurements."""
    forced = os.environ.get("SPACEKNIFE_SCAN_THREADS", "")
    if forced.isdigit() and int(forced) > 0:
        return min(64, int(forced))
    cpus = os.cpu_count() or 4
    if sys.platform == "darwin":
        return max(4, min(16, 2 * cpus))
    return max(2, min(16, cpus))


@dataclass(slots=True)
class ScanProfile:
    """Where a scan's time went (logged when it ends; ``spaceknife scan --profile`` prints it)."""

    wall_s: float  # scanning, start to end, without the time it was paused
    threads: int  # listing slices allowed in parallel
    busy_s: float  # summed time the slices took in their threads (listing: system calls)
    ingest_s: float  # main thread: folding the slices into the tree
    limited_s: float  # main thread: waiting for the disk-load limit to allow another slice
    slices: int
    items: int  # entries this run added to the tree

    @property
    def busy(self) -> float:
        """Share of the listing threads' time spent listing (low: they wait for work or the limit)."""
        return self.busy_s / (max(self.wall_s, 1e-9) * max(1, self.threads))

    @property
    def ingest(self) -> float:
        """Share of the scan the main thread spent folding results (near 100 %: it is the bottleneck)."""
        return self.ingest_s / max(self.wall_s, 1e-9)

    @property
    def limited(self) -> float:
        return self.limited_s / max(self.wall_s, 1e-9)

    def text(self) -> str:
        bulk = bulk_listing()
        return (
            f"{self.items:,} items in {self.wall_s:.1f}s ({self.items / max(self.wall_s, 1e-9):,.0f}/s); "
            f"listing threads busy {self.busy:.0%} of {self.threads}; "
            f"main thread folding {self.ingest:.0%}; "
            f"held by the disk-load limit {self.limited:.0%}; "
            f"{self.slices:,} slices" + (f"; getattrlistbulk {bulk}" if bulk else "")
        )


def bulk_listing() -> str | None:
    """macOS: whether the native walker lists with getattrlistbulk (\"on\", \"off: why\", \"untested\")."""
    if sys.platform != "darwin":
        return None
    from .. import native

    mod = native.module()
    state = getattr(mod, "bulk_listing", None) if mod is not None else None
    return str(state()) if state is not None else None


Slicer = Callable[[list[Task], SliceOptions], SliceResult]


def link_set() -> Any:
    """Where a scan keeps the (dev, ino) of hard-linked files it has counted: the native LinkSet
    (``mark`` runs the loop of ``Scanner._fold`` over a whole slice) or, without the extension, a
    set of tuples. The Python loop took about a fifth of a warm scan of a Linux system whose uv
    cache and environments share 160,000 files by hard links (0.5 µs per linked file)."""
    from .. import native

    mod = native.module()
    if mod is not None and native.has("LinkSet"):
        return mod.LinkSet()
    return set()


def native_slicer() -> Slicer | None:
    """The native walker as a slicer (one listing thread per call), or None without the extension."""
    from .. import native

    mod = native.module()
    if mod is None or not native.has("scan_slice"):
        return None
    scan_slice = mod.scan_slice
    # builds before the check of case-only siblings reject these keys (docs/native.md 2.2)
    names = "verify_names" in getattr(mod, "ROOT_KEYS", ())
    listed = "listed_alloc" in getattr(mod, "ROOT_KEYS", ())
    known = getattr(mod, "OPT_KEYS", ())
    switches = [k for k in ("skip_final", "dot_check", "keep_where_updated") if k in known]

    def run(tasks: list[Task], opts: SliceOptions) -> SliceResult:
        roots = [
            {
                "dev": r.dev,
                "cluster_size": r.cluster_size,
                "resident_max": r.resident_max,
                "cross_mounts": r.cross_mounts,
                **({"skip": list(r.skip)} if r.skip else {}),  # macOS only; older builds lack the keys
                **({"protect": list(r.protect)} if r.protect else {}),
                **({"path": r.path, "verify_names": r.verify_names} if names else {}),
                **({"listed_alloc": r.listed_alloc} if listed else {}),
            }
            for r in opts.roots
        ]
        raw = scan_slice(
            tasks,
            {
                "roots": roots,
                "budget_s": opts.budget_s,
                "max_entries": opts.max_entries,
                "threads": 1,
                **{k: getattr(opts, k) for k in switches},  # SliceOptions: from the environment
            },
        )
        return SliceResult(**raw)

    return run


def verify_names(volume: VolumeInfo | None) -> bool:
    """Whether listings of this volume check folders by the names the file system stores
    (``RootOptions.verify_names``): Windows on WinBtrfs, where trees Linux made can hold names
    that differ only in case, and an open by name takes the first entry that matches in any case."""
    return sys.platform == "win32" and volume is not None and volume.fs_type.lower() == "btrfs"


def listed_alloc(volume: VolumeInfo | None) -> str:
    """``RootOptions.listed_alloc`` for a root on ``volume``: files take their size on disk from
    their listing entries on local NTFS, ReFS and WinBtrfs volumes (where the listing was measured
    against the file system's own count); elsewhere (FAT, exFAT, network shares, other systems), or
    with SPACEKNIFE_WIN_LISTED_ALLOC=0, it is estimated from the size as before."""
    if sys.platform != "win32" or volume is None or volume.is_remote:
        return ""
    if not worker._env_switch("SPACEKNIFE_WIN_LISTED_ALLOC", True):
        return ""
    return {"ntfs": "NTFS", "refs": "ReFS", "btrfs": "Btrfs"}.get(volume.fs_type.lower(), "")


def make_pool(workers: int | None = None) -> ProcessPoolExecutor:
    """Spawn-context pool (never fork: the backend is multi-threaded)."""
    return ProcessPoolExecutor(
        max_workers=workers or default_workers(),
        mp_context=multiprocessing.get_context("spawn"),
        initializer=worker.warm_up,
    )


def normalise_root(path: str) -> str:
    path = os.path.abspath(os.path.expanduser(path))
    if sys.platform == "win32" and len(path) == 2 and path[1] == ":":
        path += "\\"
    return path


class Scanner:
    def __init__(
        self,
        roots: list[str],
        *,
        executor: Executor | None = None,
        concurrency: int | None = None,
        cross_mounts: bool = False,
        budget_s: float = 0.03,
        slicer: Slicer | None = None,
        batch_tasks: int = 32,
        governor: IoGovernor | None = None,
        count_links_each: bool = False,
        skip_protected: bool = False,
    ) -> None:
        if not roots:
            raise ScanError("no scan roots given")
        self.store = NodeStore()
        self.state = ScanState.QUEUED
        self.roots: list[RootInfo] = []
        self._executor = executor
        self._own_thread_executor = executor is None
        self.concurrency = concurrency or (
            getattr(executor, "_max_workers", None) or 1 if executor is not None else 1
        )
        self.budget_s = budget_s
        self.slicer: Slicer = slicer or worker.scan_slice
        self.batch_tasks = max(1, batch_tasks)
        self.governor = governor
        self.count_links_each = count_links_each  # every hard link counts (the 「每个链接都计」 setting)
        self._running = asyncio.Event()
        self._running.set()
        self._cancelled = False
        self._seen_links = link_set()  # (dev, ino) of hard-linked files already counted
        self._queue: deque[Task] = deque()
        self._errors = 0
        self._denied: list[str] = []
        self.denied_reasons: list[str] = []  # OS message per recorded denied path (log panel)
        self._current = ""
        self._started: float | None = None  # this run's start (perf_counter); None before the first
        self._finished: float | None = None  # this run's end; None while it runs
        # earlier runs of this scan (stopped, then 「继续扫描」): elapsed_s adds them, not the waits
        self._before_s = 0.0
        self._held_s = 0.0  # the hard runs' time while an MFT read stands in for them
        self.started_at: datetime | None = None  # the scan's first start, kept by later runs
        self.finished_at: datetime | None = None
        self.message = ""
        self.expected_items: int | None = None  # from the last snapshot of the same roots (UI-W2-15)
        self._samples: deque[tuple[float, int, int]] = deque()  # (time, items, allocated bytes)
        self._busy_s = self._ingest_s = self._limited_s = self._paused_s = 0.0  # ScanProfile
        self._paused_since: float | None = None
        self._folded = 0  # entries added to the tree by this run
        self._slices = 0

        root_opts: list[RootOptions] = []
        normalised: list[str] = []
        # macOS without Full Disk Access, when the user chose so: places that would each ask
        protected = protected_folders() if skip_protected else ()
        for raw in roots:
            path = normalise_root(raw)
            if any(path == r or path.startswith(r.rstrip("\\/") + os.sep) for r in normalised):
                continue  # nested inside another root: already covered
            normalised.append(path)
        for path in normalised:
            try:
                st = os.stat(path)
            except OSError as exc:
                raise ScanError(f"cannot access {path}: {exc.strerror or exc}") from exc
            if not os.path.isdir(path):
                raise ScanError(f"not a directory: {path}")
            try:
                volume: VolumeInfo | None = volume_for(path)
            except OSError:
                volume = None
            node = self.store.add_root(path)
            self.store.set_root_stats(
                node,
                mtime=int(max(0, st.st_mtime)),
                ctime=int(max(0, getattr(st, "st_birthtime", 0.0))),
                atime=int(max(0, st.st_atime)),
            )
            whole = volume is not None and is_volume_root(path, volume)
            self.roots.append(RootInfo(path=path, node=node, volume=volume, whole_volume=whole))
            ntfs = sys.platform == "win32" and volume is not None and volume.fs_type.upper() == "NTFS"
            root_opts.append(
                RootOptions(
                    dev=st.st_dev,
                    cluster_size=volume.cluster_size if volume else 4096,
                    resident_max=512 if ntfs else 0,
                    cross_mounts=cross_mounts,
                    skip=() if cross_mounts else volume_group_twins(path),
                    protect=tuple(p for p in protected if inside(p, path)),
                    path=path,
                    verify_names=verify_names(volume),
                    listed_alloc=listed_alloc(volume),
                )
            )
            self._queue.append(((0, node, 0), path, len(root_opts) - 1))
        self._root_opts = tuple(root_opts)

    @property
    def has_queue(self) -> bool:
        """Folders are left to read (a stopped scan can go on with them)."""
        return bool(self._queue)

    @property
    def root_options(self) -> tuple[RootOptions, ...]:
        """Listing options per scan root (file change monitoring lists folders the same way)."""
        return self._root_opts

    # ------------------------------------------------------------------ control

    def cancel(self) -> None:
        self._cancelled = True
        self._unpaused()
        self._running.set()  # a paused scan stops too

    @property
    def cancelled(self) -> bool:
        return self._cancelled

    def uncancel(self) -> None:
        """Forget a cancel that only stopped the hard scan to switch to another method."""
        self._cancelled = False

    @property
    def paused(self) -> bool:
        return not self._running.is_set()

    # the tree comes from elsewhere (an MFT scan adopted into the store): timing and state only

    def begin_external(self) -> None:
        """The read starts; its tree replaces whatever a hard part listed, and so does its time
        (scan.use_mft: the next 「约 N 秒」 estimate comes from it). The hard part's time is held
        for :meth:`reset_external`."""
        self.state = ScanState.SCANNING
        self._held_s = self._before_s + self._run_s()
        self._before_s, self._started, self._paused_s = 0.0, None, 0.0
        self._next_run()

    def finish_external(self, state: ScanState) -> None:
        self.state = state
        self._finished = time.perf_counter()
        self.finished_at = datetime.now(UTC)
        self._queue.clear()
        self._current = ""

    def reset_external(self) -> None:
        """The external read failed: :meth:`run` scans as usual, going on with the hard part's time
        (not the attempt's: the tree keeps nothing of it)."""
        self.state = ScanState.QUEUED
        self._before_s, self._held_s = self._held_s, 0.0
        self._started = self._finished = None
        self.finished_at = None

    def _run_s(self) -> float:
        """This run so far, the time it was paused included (``elapsed_s``; the profile leaves it out)."""
        if self._started is None:
            return 0.0
        return (self._finished or time.perf_counter()) - self._started

    def _next_run(self) -> None:
        """A run starts now: the scan keeps its first start (``started_at``), and an earlier run (a
        stopped scan going on) counts in ``_before_s``."""
        self._before_s += self._run_s()
        self._started = time.perf_counter()
        self._finished = None
        self.finished_at = None
        if self.started_at is None:
            self.started_at = datetime.now(UTC)

    def pause(self) -> None:
        """Stop dispatching batches; those in flight finish within their time budget (W2)."""
        if self._running.is_set() and self.state in (ScanState.QUEUED, ScanState.SCANNING):
            self._running.clear()
            self._paused_since = time.perf_counter()
            if self.governor is not None:
                self.governor.pause(owner=self)  # also a clean baseline window: nothing of ours in flight

    def _unpaused(self) -> None:
        if self._paused_since is not None:
            self._paused_s += time.perf_counter() - self._paused_since
            self._paused_since = None

    def resume(self) -> None:
        if not self._running.is_set():
            self._unpaused()
            self._running.set()
            if self.governor is not None:
                self.governor.resume(owner=self)

    async def run(
        self,
        on_progress: Callable[[ScanProgress], None] | None = None,
        progress_interval: float = 0.25,
    ) -> ScanProgress:
        loop = asyncio.get_running_loop()
        self.state = ScanState.SCANNING
        self._next_run()
        self._busy_s = self._ingest_s = self._limited_s = self._paused_s = 0.0  # this run's profile
        self._slices = self._folded = 0
        self._samples.clear()  # the rates come from this run's listing, not across a stop
        if self._paused_since is not None:  # paused while queued
            self._paused_since = self._started
        executor = self._executor
        thread_pool = None
        if executor is None:
            from concurrent.futures import ThreadPoolExecutor

            thread_pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="spaceknife-scan")
            executor = thread_pool
        inflight: set[asyncio.Future[SliceResult]] = set()
        permit_task: asyncio.Future[Permit] | None = None
        resumed: asyncio.Task[bool] | None = None
        gov = self.governor
        last_report = 0.0
        asked = 0.0

        def dispatch(permit: Permit | None) -> None:
            parallel = gov.concurrency if gov is not None else self.concurrency
            share = max(1, min(self.batch_tasks, len(self._queue) // max(1, parallel)))
            batch = [self._queue.popleft() for _ in range(min(share, len(self._queue)))]
            budget = min(0.01, self.budget_s) if len(self._queue) < parallel else self.budget_s
            opts = SliceOptions(roots=self._root_opts, budget_s=budget)
            fut = loop.run_in_executor(executor, self.slicer, batch, opts)
            if permit is not None:
                permit.bind(fut)
            inflight.add(fut)

        try:
            while (self._queue or inflight) and not self._cancelled:
                running = self._running.is_set()
                if gov is None:
                    while running and self._queue and len(inflight) < self.concurrency:
                        dispatch(None)
                elif running and self._queue and permit_task is None and len(inflight) < self.concurrency:
                    permit_task = asyncio.ensure_future(gov.permit())  # paced by the disk-load limit
                    asked = time.perf_counter()
                elif not running and permit_task is not None:
                    permit_task.cancel()  # paused: the governor takes a waiting request back
                    with contextlib.suppress(asyncio.CancelledError):
                        await permit_task
                    permit_task = None
                waits: set[asyncio.Future[Any]] = set(inflight)
                if permit_task is not None:
                    waits.add(permit_task)
                if not running:
                    if resumed is None:
                        resumed = asyncio.ensure_future(self._running.wait())
                    waits.add(resumed)
                if not waits:
                    break
                done, _ = await asyncio.wait(waits, return_when=asyncio.FIRST_COMPLETED)
                landed: list[SliceResult] = []
                for fut in done:
                    if fut is permit_task:
                        permit_task = None
                        if not inflight:  # nothing listing meanwhile: the limit held the scan
                            self._limited_s += time.perf_counter() - asked
                        permit = fut.result()
                        if self._queue and self._running.is_set() and not self._cancelled:
                            dispatch(permit)
                        else:
                            permit.release()
                    elif fut is resumed:
                        resumed = None
                    else:
                        inflight.discard(fut)
                        landed.append(fut.result())
                if landed:
                    self._apply(*landed)
                now = time.perf_counter()
                if on_progress is not None and now - last_report >= progress_interval:
                    last_report = now
                    _report(on_progress, self.progress())
            if inflight:  # cancelled: let in-flight slices land so the partial tree stays consistent
                for fut in asyncio.as_completed(inflight):
                    with contextlib.suppress(Exception):
                        self._apply(await fut)
            self.state = ScanState.CANCELLED if self._cancelled else ScanState.DONE
        except BrokenProcessPool as exc:
            self.state = ScanState.FAILED
            self.message = f"scan worker died: {exc}"
            logger.error(self.message)
        except Exception as exc:
            self.state = ScanState.FAILED
            self.message = f"{type(exc).__name__}: {exc}"
            logger.exception("scan failed")
        finally:
            if permit_task is not None:
                if not permit_task.done():
                    permit_task.cancel()  # the governor takes the request back
                elif not permit_task.cancelled() and permit_task.exception() is None:
                    permit_task.result().release()  # granted but never used
            if resumed is not None and not resumed.done():
                resumed.cancel()
            if self.paused and gov is not None:
                gov.resume(owner=self)
            self._unpaused()
            self._running.set()
            self._finished = time.perf_counter()
            self.finished_at = datetime.now(UTC)
            if thread_pool is not None:
                thread_pool.shutdown(wait=False, cancel_futures=True)
        result = self.progress()
        if on_progress is not None:
            _report(on_progress, result)
        logger.info(
            "scan {}: {:,} items, {:,} files, {:.1f} GiB in {:.2f}s ({:,.0f} items/s), {} errors",
            self.state.value,
            result.items,
            result.files,
            result.bytes_alloc / 2**30,
            result.elapsed_s,
            result.rate,
            result.errors,
        )
        logger.info("scan profile: {}", self.profile().text())
        return result

    # ------------------------------------------------------------------ folding results

    def _apply(self, *results: SliceResult) -> None:
        """Fold slice results into the tree: several that came in together as one (fewer, larger
        calls into the store; the main thread folding is what limits a fast scan)."""
        t0 = time.perf_counter()
        n0 = len(self.store)
        try:
            self._fold(list(results))
        finally:
            self._folded += len(self.store) - n0
            self._ingest_s += time.perf_counter() - t0
            self._busy_s += sum(r.elapsed for r in results)
            self._slices += len(results)

    def profile(self) -> ScanProfile:
        end = self._finished or time.perf_counter()
        paused = self._paused_s + (end - self._paused_since if self._paused_since is not None else 0.0)
        return ScanProfile(
            wall_s=end - self._started - paused if self._started is not None else 0.0,
            threads=self.concurrency,
            busy_s=self._busy_s,
            ingest_s=self._ingest_s,
            limited_s=self._limited_s,
            slices=self._slices,
            items=self._folded,
        )

    def _fold(self, results: list[SliceResult]) -> None:
        """Slices that came in together, folded as one (what worker.merge_slices and one fold of
        the merged slice did, without the merged copy): hard links and extensions slice by slice
        in order, one NodeStore.ingest_slices (natively without the GIL where the extension is),
        then errors and the frontier."""
        store = self.store
        parts = []
        for res in results:
            cols = res.cols
            n_entries = len(res.name_lens)
            if n_entries and not self.count_links_each:
                seen = self._seen_links
                if not isinstance(seen, set):  # native: the same loop, in one call
                    seen.mark(cols["dev"], cols["ino"], cols["nlink"], cols["kind"], cols["flags"])
                elif len(cand := np.nonzero((cols["nlink"] > 1) & (cols["kind"] == Kind.FILE))[0]):
                    flags = cols["flags"]
                    for i in cand.tolist():
                        key = (int(cols["dev"][i]), int(cols["ino"][i]))
                        if key in seen:
                            flags[i] |= np.uint32(Flag.HARDLINK_DUP)
                        else:
                            seen.add(key)
            remap = np.array([store.ext_id(e) for e in res.ext_table], dtype=np.uint16)
            ext_global = remap[res.ext_ids] if n_entries else res.ext_ids
            parts.append(
                (
                    res.refs,
                    res.bounds,
                    res.name_blob,
                    res.name_lens,
                    {key: cols[key] for key in ENTRY_COLUMNS},
                    ext_global,
                )
            )
        # the whole slices in one go (mount points stay unentered: never PENDING)
        all_starts = store.ingest_slices(parts)
        skipped = 0
        for res, starts in zip(results, all_starts, strict=True):
            if res.paths:
                self._current = res.paths[-1]
            skipped += int(np.count_nonzero(starts < 0))
        if skipped:
            logger.debug("scan: {} listing(s) of folders deleted while they were read, skipped", skipped)

        def resolve(refs: list[tuple[int, int, int]], starts: np.ndarray) -> list[int]:
            """The nodes results refer to; -1 for one that left the tree while it was read."""
            if not refs:
                return []
            r = np.array(refs, dtype=np.int64).reshape(-1, 3)
            inner = r[:, 0] != 0
            out = r[:, 1].copy()
            if bool(inner.any()):
                begin = starts[r[inner, 1]]
                out[inner] = np.where(begin >= 0, begin + r[inner, 2], -1)
            old = ~inner
            if bool(old.any()):
                gone = (store.cols["flags"][out[old]] & np.uint32(Flag.GONE)) != 0
                out[old] = np.where(gone, -1, out[old])
            nodes: list[int] = out.tolist()
            return nodes

        for res, starts in zip(results, all_starts, strict=True):
            for (_, path, msg), node in zip(
                res.errors, resolve([e[0] for e in res.errors], starts), strict=True
            ):
                if node < 0:
                    continue
                store.mark(node, Flag.ACCESS_DENIED)
                store.clear(node, Flag.PENDING)
                self._errors += 1
                if len(self._denied) < MAX_RECORDED_DENIED:
                    self._denied.append(path)
                    self.denied_reasons.append(msg)
        for res, starts in zip(results, all_starts, strict=True):
            nodes = resolve([f[0] for f in res.frontier], starts)
            self._queue.extend(
                ((0, node, 0), path, root_idx)
                for (_, path, root_idx), node in zip(res.frontier, nodes, strict=True)
                if node >= 0
            )

    # ------------------------------------------------------------------ reporting

    def progress(self) -> ScanProgress:
        c = self.store.cols
        elapsed = self._before_s + self._run_s()  # every run of the scan, without the waits between
        items = len(self.store) - 1
        scanned_alloc = int(c["tot_alloc"][0])
        rate, byte_rate = self._rates(items, scanned_alloc, elapsed)
        running = self.state == ScanState.SCANNING
        fraction: float | None = None
        remaining: float | None = None
        used = 0
        if self.roots and all(r.whole_volume and r.volume for r in self.roots):
            used = sum(r.volume.used for r in self.roots if r.volume)
        # By items when the last scan of these roots says how many there are; else by bytes against
        # the volume's used space, but only up to BYTES_CEILING: used space is no exact total (APFS
        # clones and hard links can make the scan pass it, snapshots and other volumes keep it out
        # of reach), so past that the progress is unknown rather than stuck at 99 %.
        expected = self.expected_items or 0
        by_items = items / expected if expected else None
        by_bytes = scanned_alloc / used if used > 0 else None
        if self.state == ScanState.DONE:
            fraction = 1.0 if by_items is not None or by_bytes is not None else None
        elif by_items is not None and by_items < 1:
            fraction = min(0.99, by_items)
        elif by_bytes is not None and by_bytes < BYTES_CEILING:
            fraction = by_bytes
        if running and elapsed >= ESTIMATE_AFTER_S:
            if by_items is not None and by_items < 1 and rate > 0:
                remaining = (expected - items) / rate
            elif by_bytes is not None and by_bytes < BYTES_CEILING and byte_rate > 0:
                remaining = (used - scanned_alloc) / byte_rate
        return ScanProgress(
            state=self.state,
            items=items,
            files=int(c["tot_files"][0]),
            dirs=int(c["tot_dirs"][0]),
            bytes_alloc=scanned_alloc,
            bytes_size=int(c["tot_size"][0]),
            errors=self._errors,
            pending_dirs=len(self._queue),
            current_path=self._current,
            elapsed_s=elapsed,
            rate=rate,
            progress=fraction,
            started_at=self.started_at,
            finished_at=self.finished_at,
            denied=list(self._denied),
            message=self.message,
            byte_rate=byte_rate,
            remaining_s=remaining,
            expected_items=self.expected_items,
            paused=self.paused,
        )

    def _rates(self, items: int, alloc: int, elapsed: float) -> tuple[float, float]:
        """Items and bytes per second over the last few seconds (UI-W2-14), not since the start."""
        if self.state != ScanState.SCANNING:
            return (items / elapsed, alloc / elapsed) if elapsed > 0 else (0.0, 0.0)
        now = time.perf_counter()
        samples = self._samples
        if not samples or now - samples[-1][0] >= 0.25:
            samples.append((now, items, alloc))
        while len(samples) > 2 and now - samples[1][0] >= RATE_WINDOW_S:
            samples.popleft()
        t0, i0, b0 = samples[0]
        if now - t0 < 1.0:
            return (items / elapsed, alloc / elapsed) if elapsed > 0 else (0.0, 0.0)
        return (items - i0) / (now - t0), (alloc - b0) / (now - t0)

    def space_summary(self, root: RootInfo) -> tuple[int, int] | None:
        """(free, unknown) bytes for a whole-volume root, else None."""
        if not root.whole_volume or root.volume is None:
            return None
        scanned = int(self.store.cols["tot_alloc"][root.node])
        return root.volume.free, max(0, root.volume.used - scanned)

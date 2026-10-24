"""Scan registry shared by all sessions: running scans, opened snapshots, tree queries."""

from __future__ import annotations

import asyncio
import contextlib
import os
import secrets
import threading
import time
from collections import OrderedDict
from collections.abc import Awaitable, Callable
from concurrent.futures import Executor, ProcessPoolExecutor, ThreadPoolExecutor
from dataclasses import dataclass, field, replace
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal
from weakref import WeakKeyDictionary

import numpy as np
from loguru import logger

from spaceknife_protocol import scan as p
from spaceknife_protocol.errors import ErrorCode

from .. import memory, native, paths
from ..events import denied_reason, emit, printable
from ..filter.names import forget, name_index
from ..helper import HelperError, SharedHelper, is_elevated
from ..i18n import tr
from ..io import IoGovernor, IoRegistry, set_low_io_priority
from ..privacy import full_disk_access
from ..scan import mft
from ..scan.media import media_of, system_mount
from ..scan.scanner import (
    NATIVE_BATCH,
    NATIVE_BUDGET_S,
    RootInfo,
    Scanner,
    ScanProgress,
    ScanState,
    Slicer,
    default_workers,
    make_pool,
    native_slicer,
    native_threads,
)
from ..scan.volume import VolumeInfo
from ..settings import SettingsStore
from ..store import snapshot
from ..store.lock import STRUCTURE
from ..store.nodes import Flag, Kind, NodeStore
from .compute import compute
from .rpc import RpcError

if TYPE_CHECKING:
    from ..layout.basis import Basis
    from ..store.subtree import Summary

MAX_SCANS = 8
IO_START_WAIT_S = 3.0  # a scan started with the backend waits this long for disk sampling
TICK_S = 0.5  # status pushes while a scan runs, also when nothing lands (MFT read, paused): W2 curve
CLOSED_KEEP_S = 3600.0  # ScanManager.closed: how long a closed scan's size counts
RELEASE_DELAY_S = 1.0  # after scan.close: give the heap back once the request is done with the scan
MFT_READ_SHARE = 0.85  # of an MFT scan's progress: reading and parsing; the rest is building
# the received table (~160 B with the names) plus building the tree from it (docs/memory.md §6):
# ~57 B with the native build, ~120 B with the numpy one
MFT_BUILD_BYTES_PER_ROW = {"native": 220, "numpy": 280}
SUMMARY_TOP = 50  # largest files in a cached summary: the details panel asks for 5 or 50
SUMMARY_KEEP = 8  # cached summaries per scan (other nodes of the same store version)
# a hard scan reserves room for the newest complete snapshot's items, plus this share and these rows
RESERVE_SHARE = 1.05
RESERVE_EXTRA = 4096
_KIND_NAMES: dict[int, Literal["file", "dir", "link", "other"]] = {
    Kind.FILE: "file",
    Kind.DIR: "dir",
    Kind.LINK: "link",
    Kind.OTHER: "other",
}


def volume_model(v: VolumeInfo | None) -> p.VolumeInfo | None:
    if v is None:
        return None
    return p.VolumeInfo(
        mount_point=v.mount_point,
        device=v.device,
        fs_type=v.fs_type,
        label=v.label,
        total=v.total,
        free=v.free,
        used=v.used,
        cluster_size=v.cluster_size,
        is_remote=v.is_remote,
        is_removable=v.is_removable,
        serial=v.serial,
        media=media_of(v.mount_point, v.device, v.is_remote) if not v.reason else "unknown",
        is_system=os.path.normcase(v.mount_point) == os.path.normcase(system_mount()),
        remote_path=v.remote_path,
        reason=v.reason,
        note=v.note,
        reserved=v.reserved,
    )


def node_info(store: NodeStore, node: int) -> p.NodeInfo:
    c = store.cols
    size, alloc = store.effective_size(node)
    flags = Flag(int(c["flags"][node]))
    is_dir = c["kind"][node] == Kind.DIR
    return p.NodeInfo(
        id=node,
        parent=int(c["parent"][node]),
        name=store.name(node),
        kind=_KIND_NAMES.get(int(c["kind"][node]), "other"),
        depth=int(c["depth"][node]),
        size=size,
        alloc=alloc,
        files=int(c["tot_files"][node]) if is_dir else 0,
        dirs=int(c["tot_dirs"][node]) if is_dir else 0,
        child_count=int(c["child_count"][node]),
        mtime=int(c["mtime"][node]),
        ctime=int(c["ctime"][node]),
        atime=int(c["atime"][node]),
        ext=store.ext_table[int(c["ext"][node])],
        flags=[f.name.lower() for f in Flag if f and f in flags and f.name],
    )


def on_disk(store: NodeStore, node: int) -> bool:
    """Whether the node's path exists: not under a pseudo folder of an MFT scan."""
    flags = store.cols["flags"]
    return not any(flags[n] & Flag.VIRTUAL for n in (node, *store.ancestors(node)) if n > 0)


class _Summaries:
    """One store's recent summaries, computed one at a time."""

    __slots__ = ("items", "lock")

    def __init__(self) -> None:
        self.lock = threading.Lock()
        # (node, store version, node count, class table, folder choices) -> (top asked, summary)
        self.items: OrderedDict[tuple[int, ...], tuple[int, Summary]] = OrderedDict()


_summaries: WeakKeyDictionary[NodeStore, _Summaries] = WeakKeyDictionary()
_summaries_lock = threading.Lock()


def cached_summary(store: NodeStore, basis: Basis, node: int, top: int, *, cache: bool = True) -> Summary:
    """``store.subtree.summarize`` below ``node`` with ``basis``'s classes, at most once per store
    version: the details panel, the Disks page and the end of a scan ask for the same summary
    within moments, and each would walk the whole scan. Calls for one store run one at a time
    (the later ones wait and take the result), so at most one walk's temporaries exist per scan.

    ``cache=False`` while a scan still writes the store (it bumps the version before the rows are
    in): computed under the same lock, but not kept.
    """
    from .. import pathclass
    from ..store.subtree import summarize

    with _summaries_lock:
        entry = _summaries.get(store)
        if entry is None:
            entry = _summaries[store] = _Summaries()
    key = (node, store.version, len(store), id(basis.classes), pathclass.folders_version())
    with entry.lock:
        hit = entry.items.get(key) if cache else None
        if hit is None or hit[0] < top:
            want = max(top, SUMMARY_TOP)
            hit = (want, summarize(store, node, basis.cls_of, len(basis.class_ids), want, basis.ext_cls_of))
            stale = [k for k in entry.items if k[1:] != key[1:]]  # nobody asks for an old version again
            for k in stale:
                del entry.items[k]
            if cache:
                entry.items[key] = hit
                while len(entry.items) > SUMMARY_KEEP:
                    entry.items.popitem(last=False)
        else:
            entry.items.move_to_end(key)
    s = hit[1]
    return s if len(s.top_files) <= top else replace(s, top_files=s.top_files[:top])


def summary_of(server: Any, handle: ScanHandle, node: int, top: int) -> p.TreeSummaryResult:
    """``tree.summary``: parent, largest files, class totals (with what folder classes took over),
    newest change, tag, volume, and why the node takes a folder class."""
    from .. import pathclass
    from ..layout.engine import Layout

    store = handle.store
    layout = Layout(store)
    class_ids = layout.class_ids
    s = cached_summary(store, layout, node, top, cache=not handle.running)
    shared = server.views.shared(handle.id)
    tags = shared.tags if shared is not None else None
    base = store.path(node)
    tops = []
    top_cls = layout.cls_of(s.top_files).tolist() if len(s.top_files) else []
    for f, ci in zip(s.top_files.tolist(), top_cls, strict=True):
        parent_path = store.path(int(store.cols["parent"][f]))
        rel = os.path.relpath(parent_path, base) if parent_path != base else ""
        rel = "" if rel in (".", "") else rel + os.sep
        tops.append(
            p.TopFile(
                node=node_info(store, f),
                rel_parent=rel,
                tag=tags.of(store, f) if tags else None,
                class_id=class_ids[ci],
            )
        )
    order = np.argsort(-s.class_bytes, kind="stable")

    def parts(i: int) -> list[p.ClassPart]:
        if s.class_parts is None:
            return []
        row = s.class_parts[i]
        return [p.ClassPart(class_id=class_ids[j], bytes=int(row[j])) for j in np.argsort(-row) if row[j] > 0]

    classes = [
        p.ClassStat(
            class_id=class_ids[i],
            bytes=int(s.class_bytes[i]),
            files=int(s.class_files[i]),
            parts=parts(int(i)),
        )
        for i in order
        if s.class_files[i]
    ]
    why = pathclass.explain(store, node, layout.classes)
    folder_class = (
        p.FolderClassInfo(
            class_id=why.class_id,
            reason=why.reason,
            folder=store.path(why.folder),
            user=why.user,
            fallback=why.fallback,
        )
        if why is not None
        else None
    )
    parent = int(store.cols["parent"][node])
    root = next((r for r in handle.roots if r.node == node or r.node in store.ancestors(node)), None)
    return p.TreeSummaryResult(
        node=node_info(store, node),
        path=base,
        parent=node_info(store, parent) if parent > 0 else None,
        latest_mtime=s.latest_mtime,
        top_files=tops,
        classes=classes,
        tag=tags.of(store, node) if tags else None,
        volume=volume_model(root.volume) if root is not None else None,
        on_disk=on_disk(store, node),
        folder_class=folder_class,
    )


@dataclass(slots=True)
class ScanHandle:
    id: str
    store: NodeStore
    roots: list[RootInfo]
    source: Literal["scan", "snapshot"]
    scanner: Scanner | None = None
    task: asyncio.Task[None] | None = None
    snapshot_path: str | None = None
    saved_progress: ScanProgress | None = None
    auto_snapshot: bool = True
    listeners: dict[str, Callable[[p.ScanStatus], None]] = field(default_factory=dict)
    wanted: Literal["auto", "mft", "hard"] = "hard"
    method: Literal["hard", "mft", "snapshot"] = "hard"
    phase: str = ""
    fallback: str = ""
    governor: IoGovernor | None = None
    mft_task: asyncio.Future[Any] | None = None
    mft_ok: bool = False  # a whole local NTFS volume (Windows): the MFT fast scan can read it
    upgrade: Any = None  # a helper ready to take over a running hard scan (scan.use_mft)
    ads: bool = False  # count alternate data streams (MFT scans)
    transient: int = 0  # bytes held outside the store while an MFT tree is built (memory watchdog)
    mft_read: float | None = None  # share of the $MFT read so far (helper heartbeats)
    mft_elapsed: float = 0.0  # seconds since the helper took the request, at the last heartbeat
    mft_at: float = 0.0  # time.monotonic() of that heartbeat
    monitor: Any = None  # server.monitors.ScanMonitor while file changes are watched (M5)
    upgrade_task: asyncio.Task[None] | None = None

    @property
    def running(self) -> bool:
        return self.task is not None and not self.task.done()

    def progress(self) -> ScanProgress:
        if self.scanner is not None:
            return self.scanner.progress()
        assert self.saved_progress is not None
        return self.saved_progress

    def mft_progress(self) -> tuple[float, float | None] | None:
        """(share done, seconds left) of a running MFT scan, from how much of the $MFT was read.

        Reading and parsing (overlapped) take most of the time; receiving the table and building
        the tree get the last ``1 - MFT_READ_SHARE``. Seconds left extrapolate the elapsed time.
        """
        if self.method != "mft" or not self.running or self.phase not in ("mft", "receive", "build"):
            return None
        if self.phase == "mft":
            if self.mft_read is None:
                return None
            done = MFT_READ_SHARE * self.mft_read
        else:
            done = MFT_READ_SHARE + (1 - MFT_READ_SHARE) * (0.3 if self.phase == "receive" else 0.5)
        elapsed = self.mft_elapsed + max(0.0, time.monotonic() - self.mft_at)
        left = elapsed * (1 - done) / done if done >= 0.02 and elapsed > 0.5 else None
        return done, left

    def status(self) -> p.ScanStatus:
        pr = self.progress()
        mft_share = self.mft_progress()
        if mft_share is not None:
            pr = replace(pr, progress=mft_share[0], remaining_s=mft_share[1])
        free = unknown = None
        whole = [r for r in self.roots if r.whole_volume and r.volume is not None]
        if whole and len(whole) == len(self.roots):
            free = sum(r.volume.free for r in whole if r.volume)
            used = sum(r.volume.used for r in whole if r.volume)
            unknown = max(0, used - int(self.store.cols["tot_alloc"][0]))
        return p.ScanStatus(
            scan_id=self.id,
            state=pr.state.value,
            roots=[
                p.ScanRootInfo(
                    path=r.path, node=r.node, whole_volume=r.whole_volume, volume=volume_model(r.volume)
                )
                for r in self.roots
            ],
            items=pr.items,
            files=pr.files,
            dirs=pr.dirs,
            bytes_alloc=pr.bytes_alloc,
            bytes_size=pr.bytes_size,
            errors=pr.errors,
            pending_dirs=pr.pending_dirs,
            current_path=printable(pr.current_path),
            elapsed_s=pr.elapsed_s,
            rate=pr.rate,
            byte_rate=pr.byte_rate,
            progress=pr.progress,
            remaining_s=pr.remaining_s,
            expected_items=pr.expected_items,
            started_at=pr.started_at,
            finished_at=pr.finished_at,
            denied=[printable(d) for d in pr.denied],
            message=printable(pr.message),
            free=free,
            unknown=unknown,
            snapshot=self.snapshot_path,
            source=self.source,
            method="snapshot" if self.source == "snapshot" else self.method,
            phase=self.phase,
            fallback=self.fallback,
            mft_available=self.mft_ok,
            paused=pr.paused,
            io=self.governor.status(history=True) if self.governor is not None and self.running else None,
            monitor=self.monitor.status() if self.monitor is not None else None,
        )

    def publish(self) -> None:
        status = self.status()
        for key, listener in list(self.listeners.items()):
            try:
                listener(status)
            except Exception:  # one subscriber must never fail the scan or starve the others
                logger.exception("scan {} listener {} failed", self.id, key)


class ScanManager:
    def __init__(self) -> None:
        self.scans: dict[str, ScanHandle] = {}
        self._pool: ProcessPoolExecutor | None = None
        self._pool_size = default_workers()
        # the native walker lists on plain threads (GIL released): no worker processes at all
        self._slicer = None if os.environ.get("SPACEKNIFE_NO_NATIVE") else native_slicer()
        self._threads: ThreadPoolExecutor | None = None
        self._thread_count = native_threads()
        self.io = IoRegistry(max_workers=self._thread_count if self._slicer else self._pool_size)
        self.helper = SharedHelper()  # one read-only scan helper per backend (MFT; asks for admin once)
        self._low_io = 0  # running scans that lowered this process's I/O priority
        self._io_ready = asyncio.Event()  # set once sampling started (or failed to)
        self._io_starting = False
        self.settings: SettingsStore | None = None  # set by the server (W7 defaults for new scans)
        self.monitors: Any = None  # server.monitors.MonitorManager, set by the server (M5)
        # called when a scan finishes and before its tab closes: keep what it showed (machine.disks)
        self.keepers: list[Callable[[ScanHandle], Awaitable[None]]] = []
        # set by the ViewManager: scans with open views are never evicted; evicted scans are released
        # (when, bytes) of the scans closed lately: the C heap keeps about that much for the next
        # scan to reuse, which the memory watchdog allows for (server/diag.py)
        self.closed: list[tuple[float, int]] = []
        self._releases: set[asyncio.Task[None]] = set()
        self.in_use: Callable[[str], bool] | None = None
        self.on_evict: Callable[[str], None] | None = None

    # ------------------------------------------------------------------ lifecycle

    def _executor(self) -> ProcessPoolExecutor:
        self._drop_broken_pool()
        if self._pool is None:
            self._pool = make_pool(self._pool_size)
        return self._pool

    def _drop_broken_pool(self) -> None:
        """Forget the worker pool once a worker died, so the next scan spawns a fresh one."""
        pool = self._pool
        if pool is not None and getattr(pool, "_broken", False):
            logger.warning("scan worker pool is broken; starting a new one for the next scan")
            self._pool = None
            pool.shutdown(wait=False, cancel_futures=True)

    def slicer(self) -> Slicer | None:
        """The native folder lister, or None (the Python worker then)."""
        return self._slicer

    def thread_executor(self) -> ThreadPoolExecutor:
        return self._thread_executor()

    def _thread_executor(self) -> ThreadPoolExecutor:
        if self._threads is None:
            self._threads = ThreadPoolExecutor(
                max_workers=self._thread_count, thread_name_prefix="spaceknife-list"
            )
        return self._threads

    async def start_io(self) -> None:
        """Start disk-load sampling (at backend start: the W2 baseline is measured before a scan)."""
        self._io_starting = True
        try:
            await self.io.start()
        except Exception:  # the governor then runs open loop; scanning works regardless
            logger.exception("disk load sampling could not start")
        finally:
            self._io_ready.set()

    async def close(self) -> None:
        if self.monitors is not None:
            await self.monitors.close()
        for handle in self.scans.values():
            if handle.scanner is not None:
                handle.scanner.cancel()
            if handle.mft_task is not None:
                handle.mft_task.cancel()
        tasks = [h.task for h in self.scans.values() if h.task is not None]
        await asyncio.gather(*tasks, return_exceptions=True)
        for task in list(self._releases):
            task.cancel()
        if self._pool is not None:
            self._pool.shutdown(wait=False, cancel_futures=True)
            self._pool = None
        if self._threads is not None:
            self._threads.shutdown(wait=False, cancel_futures=True)
            self._threads = None
        with contextlib.suppress(Exception):
            await self.helper.close()
        with contextlib.suppress(Exception):
            await self.io.stop()

    def _make_room(self) -> None:
        while len(self.scans) >= MAX_SCANS:
            in_use = self.in_use
            idle = [
                h for h in self.scans.values() if not h.running and not (in_use is not None and in_use(h.id))
            ]
            if not idle:
                raise RpcError(
                    ErrorCode.SCAN_BUSY, f"too many scans in memory (max {MAX_SCANS}); close one first"
                )
            oldest = min(idle, key=_finished_key)
            if self.on_evict is not None:
                self.on_evict(oldest.id)
            self.scans.pop(oldest.id, None)
            if self.monitors is not None:
                asyncio.get_running_loop().create_task(self.monitors.stop(oldest.id))
            logger.info("evicted scan {} to free memory", oldest.id)

    def get(self, scan_id: str) -> ScanHandle:
        handle = self.scans.get(scan_id)
        if handle is None:
            raise RpcError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {scan_id}")
        return handle

    # ------------------------------------------------------------------ scans

    async def start(self, params: p.ScanStartParams) -> ScanHandle:
        self._make_room()
        st = self.settings
        workers = params.workers
        if workers is None and st is not None and st.get("scan.concurrency") == "manual":
            workers = int(st.get("scan.workers"))
        cross = (
            params.cross_mounts
            if params.cross_mounts is not None
            else bool(st and st.get("scan.cross_mounts"))
        )
        auto = (
            params.auto_snapshot
            if params.auto_snapshot is not None
            else (st is None or bool(st.get("advanced.auto_snapshot")))
        )
        manual = workers is not None and workers > 0
        executor: Executor | None
        options: dict[str, Any] = {}
        if workers == 0:  # in-process, one thread (tests, tiny scans): no governor
            executor, concurrency = None, 1
        elif self._slicer is not None:
            executor = self._thread_executor()
            concurrency = min(workers or self._thread_count, self._thread_count)
            options = {"slicer": self._slicer, "batch_tasks": NATIVE_BATCH, "budget_s": NATIVE_BUDGET_S}
        else:
            executor = self._executor()
            concurrency = min(workers or self._pool_size, self._pool_size)
        links_each = st is not None and st.get("scan.hardlinks") == "each"
        skip_protected = await asyncio.to_thread(
            wants_skip_protected, params.skip_protected, str(st.get("scan.protected_places")) if st else "ask"
        )
        try:
            scanner = await asyncio.to_thread(
                Scanner,
                params.roots,
                executor=executor,
                concurrency=concurrency,
                cross_mounts=cross,
                count_links_each=links_each,
                skip_protected=skip_protected,
                **options,
            )
        except Exception as exc:
            raise RpcError(ErrorCode.SCAN_FAILED, str(exc)) from exc
        governor: IoGovernor | None = None
        if self._io_starting and not self._io_ready.is_set():  # a scan right after the backend started
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(self._io_ready.wait(), IO_START_WAIT_S)
        if executor is not None and self.io.running:
            governor = await self.io.governor_for_path(scanner.roots[0].path)
            level = params.load_level or (str(st.get("scan.load_manual")) if st else "balanced")
            custom = params.load_custom or (int(st.get("scan.custom_limit")) if st else None)
            governor.set_max_workers(concurrency)
            governor.set_level(level, float(custom) if custom is not None else None)  # type: ignore[arg-type]
            governor.set_yield_when_busy(st is None or bool(st.get("scan.yield_when_busy")))
            governor.set_manual_concurrency(concurrency if manual else None)
            scanner.governor = governor
        scanner.expected_items = await asyncio.to_thread(last_item_count, [r.path for r in scanner.roots])
        if scanner.expected_items:  # about as many as last time: the columns never grow by copying
            scanner.store.reserve(int(scanner.expected_items * RESERVE_SHARE) + RESERVE_EXTRA)
        await asyncio.to_thread(  # media probes may block: warm their cache before status() needs them
            lambda: [
                media_of(r.volume.mount_point, r.volume.device, r.volume.is_remote)
                for r in scanner.roots
                if r.volume
            ]
        )
        wanted = params.method or (str(st.get("scan.method")) if st else "auto")
        handle = ScanHandle(
            id="scan-" + secrets.token_hex(4),
            store=scanner.store,
            roots=scanner.roots,
            source="scan",
            scanner=scanner,
            auto_snapshot=auto,
            wanted=wanted,  # type: ignore[arg-type]
            governor=governor,
            ads=params.ads if params.ads is not None else bool(st is not None and st.get("scan.ads")),
        )
        handle.mft_ok = self._mft_eligible(handle)
        self.scans[handle.id] = handle
        handle.task = asyncio.create_task(self._run(handle), name=f"scan-{handle.id}")
        if self.monitors is not None:  # changes are watched from the start (UI-W2-09)
            await self.monitors.start(handle)
        logger.info("scan {} started: {}", handle.id, ", ".join(r.path for r in handle.roots))
        return handle

    async def _run(self, handle: ScanHandle) -> None:
        ticker = asyncio.create_task(self._tick(handle), name=f"tick-{handle.id}")
        low_io = self._lower_io(handle)
        try:
            if self._mft_wanted(handle) and await self._run_mft(handle):
                await self._finish(handle, 0)
            else:
                await self._run_hard(handle)
        finally:
            ticker.cancel()
            if low_io:
                self._restore_io()

    async def _tick(self, handle: ScanHandle) -> None:
        """Publish twice a second while running, also when nothing lands (W2 curve, MFT phases)."""
        while True:
            await asyncio.sleep(TICK_S)
            if handle.running:
                handle.publish()

    def _lower_io(self, handle: ScanHandle) -> bool:
        """The idle level reads at the OS's low I/O priority (setting 「空闲时使用系统低 I/O 优先级」)."""
        gov = handle.governor
        on = self.settings is None or bool(self.settings.get("scan.idle_low_io"))
        if gov is None or not gov.low_io_priority or not on:
            return False
        if self._low_io == 0:
            set_low_io_priority(None, True)
            if self._pool is not None:
                for proc in list(getattr(self._pool, "_processes", {}).values()):
                    set_low_io_priority(proc.pid, True)
        self._low_io += 1
        return True

    def _restore_io(self) -> None:
        self._low_io -= 1
        if self._low_io == 0:
            set_low_io_priority(None, False)
            if self._pool is not None:
                for proc in list(getattr(self._pool, "_processes", {}).values()):
                    set_low_io_priority(proc.pid, False)

    # ------------------------------------------------------------------ MFT fast scan

    @staticmethod
    def _mft_eligible(handle: ScanHandle) -> bool:
        """One whole local NTFS volume on Windows: the $MFT holds exactly this tree."""
        return mft.mft_unsupported(handle.roots) is None

    def mft_ready(self) -> bool:
        """An MFT scan needs no administrator prompt: rights already, or a live elevated helper."""
        current = self.helper.current
        return is_elevated() or (current is not None and current.alive and current.elevated)

    def _mft_wanted(self, handle: ScanHandle) -> bool:
        """Eligible and not a hard scan by choice; asking for administrator rights must be allowed
        (scan.elevate) unless no prompt is needed. A refused prompt falls back to a hard scan."""
        if handle.wanted == "hard" or not self._mft_eligible(handle):
            return False
        return self.settings is None or bool(self.settings.get("scan.elevate")) or self.mft_ready()

    async def _run_mft(self, handle: ScanHandle, client: Any = None) -> bool:
        """Read the volume's $MFT through the read-only helper. False: fall back to a hard scan.

        ``client``: a helper that is already running (switching a hard scan over, scan.use_mft).
        """
        scanner = handle.scanner
        assert scanner is not None
        root = handle.roots[0]
        handle.method, handle.phase = "mft", "helper" if client is None else "mft"
        scanner.begin_external()
        emit("scan", tr("开始扫描 {path}（MFT 快速扫描）", path=root.path), scan_id=handle.id, path=root.path)
        handle.publish()
        try:
            if client is None:
                elevate = self.settings is None or bool(self.settings.get("scan.elevate"))
                client = await self.helper.get(elevate=elevate)
            if scanner.cancelled:  # stopped while the administrator prompt was open
                scanner.finish_external(ScanState.CANCELLED)
                return True
            handle.phase = "mft"
            handle.publish()

            def on_progress(phase: str, elapsed: float, read: float | None) -> None:
                handle.phase = "receive" if phase == "send" else "mft"
                handle.mft_elapsed, handle.mft_at = elapsed, time.monotonic()
                if read is not None:
                    handle.mft_read = read

            job = asyncio.ensure_future(
                client.scan_mft(root.path[:2], duty=mft_duty(handle.governor), on_progress=on_progress)
            )
            handle.mft_task = job
            table = await job
            build = "native" if native.has("mft_build") else "numpy"
            handle.transient = len(table["record"]) * MFT_BUILD_BYTES_PER_ROW[build]
            handle.phase = "build"
            handle.publish()
            st = self.settings
            store = await compute(
                mft.table_to_store,
                table,
                root.path,
                client.last_scan,
                include_ads=handle.ads,
                count_links_each=st is not None and st.get("scan.hardlinks") == "each",
                consume=True,  # the table goes as the tree is built: a gigabyte less at the peak
            )
            del table
            handle.store.adopt(store)
            forget(handle.store)
            if handle.monitor is not None:
                handle.monitor.rebuilt()  # every id changed
            scanner.finish_external(ScanState.DONE)
        except asyncio.CancelledError:
            if not scanner.cancelled:  # the scan task itself was cancelled (backend shutting down)
                raise
            scanner.finish_external(ScanState.CANCELLED)
        except (HelperError, OSError, ValueError) as exc:
            reason = exc.message if isinstance(exc, HelperError) else str(exc)
            handle.method, handle.phase = "hard", ""
            handle.fallback = tr("{reason}，已改用硬扫描", reason=reason)
            scanner.reset_external()
            emit(
                "scan",
                tr("MFT 快速扫描不可用：{reason}；改用硬扫描", reason=reason),
                level="WARNING",
                scan_id=handle.id,
            )
            return False
        finally:
            handle.mft_task = None
            handle.phase = ""
            handle.transient = 0
            handle.mft_read = None
        return True

    # ------------------------------------------------------------------ hard scan

    async def _run_hard(self, handle: ScanHandle, again: bool = False) -> None:
        scanner = handle.scanner
        assert scanner is not None
        roots = [r.path for r in handle.roots]
        listed = ", ".join(roots)
        started = (
            tr("继续扫描 {paths}（硬扫描）", paths=listed)
            if again
            else tr("开始扫描 {paths}（硬扫描）", paths=listed)
        )
        emit("scan", started, scan_id=handle.id, path=roots[0])
        for r in handle.roots:
            if r.volume is not None and r.volume.note:  # e.g. a Windows drive from WSL over 9P
                emit(
                    "scan",
                    tr("{path}：{note}", path=printable(r.path), note=r.volume.note),
                    level="WARNING",
                    scan_id=handle.id,
                )
        reported = 0

        def warnings_on() -> bool:
            return self.settings is None or bool(self.settings.get("general.log_scan_warnings"))

        def progressed(_: object) -> None:
            nonlocal reported
            denied = scanner.progress().denied
            if not warnings_on():
                reported = len(denied)  # 「记录扫描警告」 off: counted, not logged
            for i in range(reported, len(denied)):
                reason = (
                    denied_reason(scanner.denied_reasons[i])
                    if i < len(scanner.denied_reasons)
                    else tr("拒绝访问")
                )
                emit(
                    "perm",
                    tr("无法访问 {path}：{reason}，已计入未知空间", path=denied[i], reason=reason),
                    level="WARNING",
                    scan_id=handle.id,
                    path=denied[i],
                )
            reported = len(denied)
            handle.publish()

        await scanner.run(on_progress=progressed)
        if scanner.state == ScanState.FAILED:
            self._drop_broken_pool()
        if handle.upgrade is not None and scanner.state == ScanState.CANCELLED:
            client, handle.upgrade = handle.upgrade, None  # scan.use_mft: the helper is ready
            scanner.uncancel()  # that cancel was ours: a user's stop from now on counts again
            if await self._run_mft(handle, client):
                await self._finish(handle, 0)
                return
            scanner.reset_external()  # the read failed: scan the rest of the volume the hard way
            handle.method = "hard"
            await scanner.run(on_progress=progressed)
        await self._finish(handle, reported if warnings_on() else None)

    async def continue_scan(self, scan_id: str) -> ScanHandle:
        """Go on with a stopped hard scan (W2 待定 16): its queue still holds the folders it had
        not read; the tree grows on where it stopped, then it is saved like any finished scan."""
        handle = self.get(scan_id)
        scanner = handle.scanner
        if handle.running:
            return handle
        if handle.source != "scan" or scanner is None or handle.method != "hard" or not scanner.has_queue:
            raise RpcError(ErrorCode.INVALID_REQUEST, tr("这次扫描没有可以继续的部分"))
        scanner.uncancel()
        handle.task = asyncio.create_task(self._run_hard(handle, again=True), name=f"scan-{handle.id}")
        return handle

    def use_mft(self, scan_id: str) -> ScanHandle:
        """Switch a running hard scan to the MFT fast scan (W2-24): the hard scan goes on while the
        helper starts (the administrator prompt), and is replaced once it is ready (W2-25)."""
        handle = self.get(scan_id)
        if not handle.mft_ok:
            raise RpcError(ErrorCode.INVALID_REQUEST, tr("只有整个本地 NTFS 卷能用 MFT 快速扫描"))
        if not handle.running or handle.method == "mft" or handle.upgrade_task is not None:
            return handle
        handle.phase, handle.fallback = "helper", ""
        handle.publish()
        handle.upgrade_task = asyncio.create_task(self._upgrade(handle), name=f"upgrade-{handle.id}")
        return handle

    async def _upgrade(self, handle: ScanHandle) -> None:
        scanner = handle.scanner
        assert scanner is not None
        try:
            client = await self.helper.get(elevate=True)
        except HelperError as exc:
            handle.fallback = tr("{reason}，继续硬扫描", reason=exc.message)
            emit(
                "scan",
                tr("未能启用 MFT 快速扫描：{reason}；继续硬扫描", reason=exc.message),
                level="WARNING",
                scan_id=handle.id,
            )
        else:
            if handle.running and handle.method == "hard" and not scanner.cancelled:
                handle.upgrade = client
                scanner.cancel()  # the hard scan stops; _run_hard switches over
        finally:
            handle.upgrade_task = None
            if handle.phase == "helper":
                handle.phase = ""
            handle.publish()

    async def _finish(self, handle: ScanHandle, reported: int | None) -> None:
        """After the tree is complete (hard or MFT): compact, report, save, index the names."""
        scanner = handle.scanner
        assert scanner is not None
        roots = [r.path for r in handle.roots]
        if handle.monitor is not None:
            handle.monitor.kick()  # the last folders of the scan get their watches now
        # nothing is added any more: give back the growth headroom (up to half of every column)
        freed = await compute(handle.store.compact)
        logger.debug("scan {}: compacted the node store, {:.0f} MB freed", handle.id, freed / 2**20)
        done = scanner.progress()
        if reported is not None and done.errors > reported:
            emit(
                "perm",
                tr("另有 {count} 个文件夹无法访问，已计入未知空间", count=f"{done.errors - reported:,}"),
                level="WARNING",
                scan_id=handle.id,
            )
        took = tr("{items} 项，{seconds} 秒", items=f"{done.items:,}", seconds=f"{done.elapsed_s:.1f}")
        if done.state == ScanState.DONE:
            finished = (
                tr("MFT 快速扫描完成：{took}", took=took)
                if handle.method == "mft"
                else tr("硬扫描完成：{took}", took=took)
            )
            emit("scan", finished, scan_id=handle.id, path=roots[0])
        elif done.state == ScanState.CANCELLED:
            emit(
                "scan",
                tr("已停止扫描：{took}，已扫描的结果保留", took=took),
                level="WARNING",
                scan_id=handle.id,
                path=roots[0],
            )
        else:
            emit(
                "scan",
                tr("扫描失败：{error}", error=done.message or tr("未知错误")),
                level="ERROR",
                scan_id=handle.id,
                path=roots[0],
            )
        if scanner.state == ScanState.DONE and handle.auto_snapshot:
            await self._auto_save(handle)
        if scanner.state == ScanState.DONE:
            await self._keep(handle)
        handle.publish()
        if scanner.state == ScanState.DONE:
            # build the name index now, so the first wildcard filter does not pay for it
            await compute(name_index(handle.store).update, len(handle.store))
        # glibc: the scan's freed temporaries go back to the system (docs/memory.md §8), aside
        trim = asyncio.get_running_loop().create_task(asyncio.to_thread(memory.trim))
        self._releases.add(trim)
        trim.add_done_callback(self._releases.discard)

    async def _auto_save(self, handle: ScanHandle) -> None:
        """Save a finished scan to the auto-snapshot folder; a failure only costs a log warning."""
        try:
            path = snapshot.auto_path([r.path for r in handle.roots])
            await asyncio.to_thread(self._save, handle, path)
        except Exception as exc:
            if isinstance(exc, OSError):
                logger.warning("could not save snapshot for {}: {}", handle.id, exc)
                reason = exc.strerror or str(exc)
            else:
                logger.exception("could not save snapshot for {}", handle.id)
                reason = str(exc) or type(exc).__name__
            emit(
                "snapshot",
                tr("自动保存快照失败：{reason}", reason=reason),
                level="WARNING",
                scan_id=handle.id,
            )
            return
        handle.snapshot_path = str(path)
        keep = int(self.settings.get("advanced.snapshot_keep")) if self.settings else snapshot.KEEP_RECENT
        try:
            deleted = await asyncio.to_thread(snapshot.prune, path.parent, keep)
        except OSError as exc:  # the snapshot itself is saved; old ones go next time
            logger.debug("could not prune old snapshots in {}: {}", path.parent, exc)
            deleted = []
        logger.info("scan {} saved to {} ({} old snapshots pruned)", handle.id, path, len(deleted))

    def cancel(self, scan_id: str) -> ScanHandle:
        handle = self.get(scan_id)
        if handle.upgrade_task is not None:
            handle.upgrade_task.cancel()
        handle.upgrade = None
        if handle.scanner is not None:
            handle.scanner.cancel()
        if handle.mft_task is not None:
            handle.mft_task.cancel()  # the helper connection is torn down; the helper stays usable
        return handle

    def pause(self, scan_id: str) -> ScanHandle:
        handle = self.get(scan_id)
        if handle.scanner is not None and handle.method == "hard":
            handle.scanner.pause()
            handle.publish()
        return handle

    def resume(self, scan_id: str) -> ScanHandle:
        handle = self.get(scan_id)
        if handle.scanner is not None:
            handle.scanner.resume()
            handle.publish()
        return handle

    def set_load(self, params: p.ScanSetLoadParams) -> ScanHandle:
        handle = self.get(params.scan_id)
        gov = handle.governor
        if gov is None:
            raise RpcError(ErrorCode.INVALID_REQUEST, tr("这次扫描没有磁盘负载控制"))
        gov.set_level(params.level, float(params.custom) if params.custom is not None else None)
        gov.set_manual_concurrency(params.concurrency)
        handle.publish()
        return handle

    async def close_scan(self, scan_id: str) -> None:
        handle = self.get(scan_id)
        if self.monitors is not None:
            await self.monitors.stop(scan_id)
        if handle.scanner is not None:
            handle.scanner.cancel()
        if handle.mft_task is not None:
            handle.mft_task.cancel()
        if handle.task is not None:
            with contextlib.suppress(Exception):
                await handle.task
        await self._keep(handle)  # what changed while it was watched
        self.scans.pop(scan_id, None)
        now = time.monotonic()
        recent = [c for c in self.closed if now - c[0] < CLOSED_KEEP_S]
        self.closed = [*recent, (now, handle.store.ram_bytes())]
        # once the request that closed it let go of the handle too, the heap can give back
        task = asyncio.get_running_loop().create_task(self._release_later())
        self._releases.add(task)
        task.add_done_callback(self._releases.discard)

    async def _keep(self, handle: ScanHandle) -> None:
        for keeper in list(self.keepers):
            try:
                await keeper(handle)
            except Exception:
                logger.opt(exception=True).warning("keeping what scan {} showed failed", handle.id)

    async def _release_later(self) -> None:
        await asyncio.sleep(RELEASE_DELAY_S)
        await asyncio.to_thread(memory.release)

    # ------------------------------------------------------------------ snapshots

    @staticmethod
    def _stats(handle: ScanHandle) -> dict[str, Any]:
        pr = handle.progress()
        return {
            "items": pr.items,
            "files": pr.files,
            "dirs": pr.dirs,
            "bytes_alloc": pr.bytes_alloc,
            "bytes_size": pr.bytes_size,
            "errors": pr.errors,
            "elapsed_s": pr.elapsed_s,
            "started_at": pr.started_at.isoformat() if pr.started_at else None,
            "finished_at": pr.finished_at.isoformat() if pr.finished_at else None,
            "denied": [printable(d) for d in pr.denied],
            "state": pr.state.value,
            "roots": [{"path": r.path, "whole_volume": r.whole_volume} for r in handle.roots],
            "method": "mft" if handle.method == "mft" else "hard",
        }

    def _save(self, handle: ScanHandle, path: Path) -> snapshot.SnapshotHeader:
        """On a worker thread: without the lock, so file changes go on meanwhile; if one came in,
        once more holding them back (a whole volume takes about a second)."""
        vols = [r.volume.to_dict() if r.volume else {} for r in handle.roots]
        roots = [r.path for r in handle.roots]
        try:
            return snapshot.save(handle.store, path, roots=roots, volumes=vols, stats=self._stats(handle))
        except snapshot.SnapshotChanged:
            logger.debug("scan {}: the tree changed while it was saved, saving again", handle.id)
        with STRUCTURE.read():
            return snapshot.save(handle.store, path, roots=roots, volumes=vols, stats=self._stats(handle))

    async def save_snapshot(self, scan_id: str, target: str | None) -> p.SnapshotInfo:
        handle = self.get(scan_id)
        if handle.running:
            raise RpcError(ErrorCode.SCAN_BUSY, tr("扫描完成（或停止）后才能保存快照"))
        path = Path(target) if target else snapshot.auto_path([r.path for r in handle.roots])
        try:
            header = await asyncio.to_thread(self._save, handle, path)
        except OSError as exc:
            raise RpcError(
                ErrorCode.SNAPSHOT_ERROR, tr("写不了 {path}：{reason}", path=path, reason=exc.strerror or exc)
            ) from exc
        except snapshot.SnapshotError as exc:
            reason = tr("写不了 {path}：{reason}", path=path, reason=exc)
            raise RpcError(ErrorCode.SNAPSHOT_ERROR, reason) from exc
        handle.snapshot_path = str(path)
        emit("snapshot", tr("快照已保存：{path}", path=path), scan_id=handle.id)
        return snapshot_info(path, header)

    async def load_snapshot(self, target: str) -> ScanHandle:
        self._make_room()
        path = Path(target)
        try:
            store, header = await asyncio.to_thread(snapshot.load, path)
        except (OSError, snapshot.SnapshotError) as exc:
            raise RpcError(
                ErrorCode.SNAPSHOT_ERROR, tr("打不开快照 {path}：{error}", path=path, error=exc)
            ) from exc
        stats = header.stats
        root_meta = {m.get("path"): m for m in stats.get("roots", [])}
        roots = []
        for idx, node in enumerate(store.roots()):
            root_path = store.name(node)
            vol = header.volumes[idx] if idx < len(header.volumes) else {}
            volume = None
            if vol:
                volume = VolumeInfo(**{k: v for k, v in vol.items() if k in VolumeInfo.__dataclass_fields__})
            roots.append(
                RootInfo(
                    path=root_path,
                    node=node,
                    volume=volume,
                    whole_volume=bool(root_meta.get(root_path, {}).get("whole_volume", False)),
                )
            )
        c = store.cols

        def parse(ts: str | None) -> datetime | None:
            return datetime.fromisoformat(ts) if ts else None

        state = saved_state(stats)
        saved = ScanProgress(
            state=state,
            items=len(store) - 1,
            files=int(c["tot_files"][0]),
            dirs=int(c["tot_dirs"][0]),
            bytes_alloc=int(c["tot_alloc"][0]),
            bytes_size=int(c["tot_size"][0]),
            errors=int(stats.get("errors", 0)),
            pending_dirs=0,
            current_path="",
            elapsed_s=float(stats.get("elapsed_s", 0.0)),
            rate=0.0,
            progress=1.0 if state == ScanState.DONE else None,  # a stopped scan's share is unknown
            started_at=parse(stats.get("started_at")),
            finished_at=parse(stats.get("finished_at")) or header.created_at,
            denied=list(stats.get("denied", [])),
        )
        handle = ScanHandle(
            id="scan-" + secrets.token_hex(4),
            store=store,
            roots=roots,
            source="snapshot",
            snapshot_path=str(path),
            saved_progress=saved,
            method=saved_method(stats) or "hard",  # kept when the snapshot is saved again
        )
        self.scans[handle.id] = handle
        logger.info("opened snapshot {} as {}", path, handle.id)
        emit(
            "snapshot",
            tr("已打开快照 {path}（{count} 项）", path=path, count=f"{header.node_count - 1:,}"),
            scan_id=handle.id,
        )
        return handle

    # ------------------------------------------------------------------ tree

    def check_node(self, handle: ScanHandle, node: int) -> None:
        if node < 0 or node >= len(handle.store):
            raise RpcError(ErrorCode.UNKNOWN_NODE, f"node {node} does not exist in {handle.id}")


def _finished_key(handle: ScanHandle) -> float:
    finished = handle.progress().finished_at
    return finished.timestamp() if finished else float("inf")


def saved_state(stats: dict[str, Any]) -> ScanState:
    """State a snapshot was saved in (``stats.state``); snapshots that lack it were finished scans."""
    try:
        state = ScanState(stats.get("state") or ScanState.DONE)
    except ValueError:
        return ScanState.DONE
    return ScanState.CANCELLED if state in (ScanState.QUEUED, ScanState.SCANNING) else state


def wants_skip_protected(requested: bool | None, setting: str) -> bool:
    """Leave out macOS's consent-guarded places (spaceknife.privacy)? A request decides (the app
    asked the user); else the scan.protected_places setting, "skip" only while there is no Full
    Disk Access ("ask" without an app to ask, as on the command line, accesses them)."""
    if requested is not None:
        return requested
    return setting == "skip" and full_disk_access() is False


def last_item_count(roots: list[str]) -> int | None:
    """Items of the newest complete snapshot of these roots: the expected total of a new scan.

    Stopped or failed scans saved by hand land in the same folder; their partial counts are skipped.
    """
    folder = paths.snapshots_dir() / snapshot.root_key(roots)
    try:
        found = sorted(folder.glob(f"*{snapshot.SUFFIX}"), key=lambda f: f.name, reverse=True)
    except OSError:
        return None
    for file in found:
        try:
            header = snapshot.read_header(file)
        except (OSError, snapshot.SnapshotError, ValueError):
            continue
        if saved_state(header.stats) == ScanState.DONE:
            return max(0, header.node_count - 1)
    return None


def snapshot_info(path: Path, header: snapshot.SnapshotHeader) -> p.SnapshotInfo:
    try:
        size = path.stat().st_size
    except OSError:
        size = 0
    return p.SnapshotInfo(
        path=str(path),
        created_at=header.created_at,
        roots=header.roots,
        node_count=header.node_count,
        bytes_alloc=int(header.stats.get("bytes_alloc", 0)),
        files=int(header.stats.get("files", 0)),
        file_bytes=size,
        method=saved_method(header.stats),
    )


def saved_method(stats: dict[str, Any]) -> Literal["hard", "mft"] | None:
    method = stats.get("method")
    return method if method in ("hard", "mft") else None


def mft_duty(gov: IoGovernor | None) -> float:
    """The $MFT reader's busy share for the scan's load level (it is a short sequential burst)."""
    if gov is None or gov.level == "max":
        return 1.0
    if gov.level == "idle":
        return 0.2
    return max(0.05, min(1.0, gov.limit / 100.0))

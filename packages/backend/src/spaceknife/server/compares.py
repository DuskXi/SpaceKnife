"""Snapshot comparisons on the server (W6, docs/manage.md §3).

``compare.open`` loads the snapshot end(s) off the event loop, merges the two trees on the compute
pool (``diff.compare``, under the structure read lock while the live tree takes part) and keeps
only what changed: a *change tree*, a small :class:`NodeStore` whose sizes are the changes
(``|target − base|``, folders the sum below them). Views open it like a scan
(``view.open {scan_id: compare_id}``), so zoom, filter, selection, results and the frame encoding
are the normal ones; the view adds the signed change and the status per block.

A comparison whose target is the live tree ("now") follows file changes: after the tree has been
quiet for :data:`LIVE_QUIET_S` (at most every :data:`LIVE_MIN_GAP_S`) it is computed again and its
views move to the new change tree by path.
"""

from __future__ import annotations

import asyncio
import itertools
import os
import re
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal

import numpy as np
from loguru import logger

from spaceknife_protocol import compare as pc
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EVENT_COMPARE_PROGRESS

from ..diff import Change, CompareCancelled, DiffTree, Status
from ..diff import compare as diff_trees
from ..i18n import Texts
from ..layout.engine import Layout
from ..store import snapshot
from ..store.nodes import COLUMNS, Flag, Kind, NodeStore
from .compute import compute
from .rpc import RpcError
from .scans import snapshot_info

if TYPE_CHECKING:
    from .scans import ScanHandle, ScanManager
    from .session import Session
    from .views import ViewHost, ViewManager

PREFIX = "cmp-"
MAX_COMPARES = 6  # per backend; the oldest one without views goes first
LIVE_QUIET_S = 3.0  # a live comparison is recomputed once the tree was quiet this long …
LIVE_MIN_GAP_S = 10.0  # … and at most this often
COUNT_UNITS = 2000  # change units counted for 「前 N 项」 (the lists show the first ones)
PROGRESS_GAP_S = 0.2

# User-facing text, kept in one place for the i18n extraction (M7.5).
TEXT = Texts(
    {
        "busy": "扫描还没有完成，完成后才能与「现在」对比",
        "same": "基准和对比目标是同一份数据",
        "roots": "快照的扫描位置和当前标签不同：{a} ≠ {b}",
        "open": "打不开快照 {path}：{error}",
        "unknown": "没有这个对比：{id}",
        "not_ready": "对比还在计算",
        "loading": "正在读取快照…",
        "comparing": "正在对比…",
        "failed": "对比失败：{error}",
        "rest": "{label} 中其余 {n} 项",
        "sep": "、",
    }
)

_DIR = np.uint8(Kind.DIR)
_STATUS_NAMES: tuple[pc.ChangeStatus, ...] = ("same", "new", "deleted", "changed")
_KIND_NAMES: dict[int, pc.ChangeKind] = {
    Kind.FILE: "file",
    Kind.DIR: "dir",
    Kind.LINK: "link",
    Kind.OTHER: "other",
}
_CLEAR = np.uint32(Flag.PENDING | Flag.HARDLINK_DUP | Flag.GONE)


# --------------------------------------------------------------------------- the change tree


@dataclass(eq=False, slots=True)
class ChangeTree:
    """What changed between two trees; one entry per row of :attr:`store` in every array.

    The store's sizes are the changes: ``alloc`` / ``size`` of a file are ``|Δ|`` of its allocated
    / logical bytes, a folder's totals the sums below it, so the treemap area is the change.
    """

    store: NodeStore
    delta: np.ndarray  # i8 net change of allocated bytes (folders: of the subtree)
    status: np.ndarray  # u1 diff.Status
    base_alloc: np.ndarray  # i8 (folders: subtree totals)
    target_alloc: np.ndarray
    grow: np.ndarray  # i8 growth below (a file: its own), ≥ 0
    shrink: np.ndarray  # i8 shrinkage below, ≥ 0
    new_files: np.ndarray  # i4 new files below (a file: 1 when it is new)
    diff: DiffTree  # the same rows as a DiffTree (change units, both ends' logical sizes)

    @classmethod
    def of(cls, diff: DiffTree) -> ChangeTree:
        """Keep the rows that changed (and their folders, which then changed too) plus the roots."""
        u = diff.store
        n = len(u)
        area = diff.grow + diff.shrink
        keep = area > 0
        keep[0] = True
        roots = np.arange(*u.child_range(0), dtype=np.int64)
        keep[roots] = True
        ids = np.flatnonzero(keep)
        m = len(ids)
        new_of = np.full(n, -1, dtype=np.int64)
        new_of[ids] = np.arange(m)
        cols = u.export_columns()
        out: dict[str, np.ndarray] = {}
        for key in COLUMNS:
            if key in ("child_start", "child_count", "tot_size", "tot_alloc", "tot_files", "tot_dirs"):
                continue
            out[key] = np.ascontiguousarray(cols[key][ids])
        old_parent = cols["parent"][ids].astype(np.int64)
        parent = np.where(old_parent >= 0, new_of[np.maximum(old_parent, 0)], -1)
        out["parent"] = parent.astype(np.int32)
        # the union tree is breadth first and grouped by parent in parent order, so the parents of
        # the kept rows still ascend and each folder's kept children are one run
        rows = np.arange(m)
        start = np.searchsorted(parent, rows, "left")
        stop = np.searchsorted(parent, rows, "right")
        count = stop - start
        out["child_start"] = np.where(count > 0, start, 0).astype(np.int32)
        out["child_count"] = count.astype(np.int32)
        kind = out["kind"]
        is_dir = kind == _DIR
        out["flags"] = out["flags"] & ~_CLEAR
        area_kept = area[ids]
        # a folder's own blocks (POSIX directories hold some) changed by area − the children's areas
        child_area = np.zeros(m, dtype=np.int64)
        has_parent = parent >= 0
        np.add.at(child_area, parent[has_parent], area_kept[has_parent])
        own = np.maximum(area_kept - child_area, 0)
        out["alloc"] = np.where(is_dir, own, np.abs(diff.delta[ids])).astype(np.int64)
        size_delta = diff.target_size[ids] - diff.base_size[ids]
        out["size"] = np.where(is_dir, 0, np.abs(size_delta)).astype(np.int64)
        store = NodeStore.from_columns(out, u.names_blob, u.ext_table)
        store.reaggregate()
        pruned = DiffTree(
            store=store,
            base_id=diff.base_id[ids],
            target_id=diff.target_id[ids],
            base_alloc=diff.base_alloc[ids],
            target_alloc=diff.target_alloc[ids],
            base_size=diff.base_size[ids],
            target_size=diff.target_size[ids],
            delta=diff.delta[ids],
            status=diff.status[ids],
            grow=diff.grow[ids],
            shrink=diff.shrink[ids],
            fold_case=diff.fold_case,
        )
        new_leaf = ((pruned.status == Status.ADDED) & ~is_dir).astype(np.int64)
        return cls(
            store=store,
            delta=pruned.delta,
            status=pruned.status,
            base_alloc=pruned.base_alloc,
            target_alloc=pruned.target_alloc,
            grow=pruned.grow,
            shrink=pruned.shrink,
            new_files=subtree_sums(store, new_leaf).astype(np.int32),
            diff=pruned,
        )

    def __len__(self) -> int:
        return len(self.store)

    def memory_bytes(self) -> int:
        return self.diff.memory_bytes() + self.new_files.nbytes

    def amounts(self, direction: pc.CompareDirection, match: np.ndarray | None = None) -> np.ndarray:
        """Area basis per row for a direction, optionally only the matching files (filter)."""
        if match is None:
            if direction == "grow":
                return self.grow
            if direction == "shrink":
                return self.shrink
            return Layout.effective_sizes(self.store)
        if direction == "all":
            leaf = self.grow + self.shrink
        else:
            leaf = self.grow if direction == "grow" else self.shrink
        n = len(self.store)
        sel = np.zeros(n, dtype=bool)
        k = min(len(match), n)
        sel[:k] = match[:k]
        sel &= self.store.cols["kind"][:n] != _DIR
        return subtree_sums(self.store, np.where(sel, leaf, 0))


def subtree_sums(store: NodeStore, leaf: np.ndarray) -> np.ndarray:
    """Per-row values summed over every subtree, a row's own included (one pass per depth level,
    like ``NodeStore.reaggregate``)."""
    n = len(store)
    sub = np.asarray(leaf[:n], dtype=np.int64).copy()
    if n == 0:
        return sub
    depth = store.cols["depth"][:n]
    parent = store.cols["parent"][:n].astype(np.int64)
    top = int(depth.max())
    order = np.argsort(depth, kind="stable")
    bounds = np.searchsorted(depth[order], np.arange(top + 2))
    for d in range(top, 0, -1):
        idx = order[bounds[d] : bounds[d + 1]]
        if idx.size:
            np.add.at(sub, parent[idx], sub[idx])
    return sub


# --------------------------------------------------------------------------- short labels (W6-32)

_GENERIC = frozenset(
    " ".join(
        (
            "data cache caches download downloads video videos file files image images temp tmp bin lib",
            "lib64 src local roaming appdata default user users storage filestorage share shared misc",
            "store backup backups content contents resources assets logs log db database packages",
            "package modules node_modules build dist out output target release debug x64 x86 win64",
            "amd64 common program programs app apps",
        )
    ).split()
)
_ID_LIKE = re.compile(r"^(?=.*\d)(?=.*[a-z])[a-z0-9_\-.{}]{8,}$|^[0-9a-f\-{}]{12,}$", re.IGNORECASE)


def _generic(name: str) -> bool:
    return name.lower() in _GENERIC or bool(_ID_LIKE.match(name))


def short_label(store: NodeStore, node: int, scope: int) -> str:
    """A file: its name. A folder with a generic name: the nearest distinctive folder above it in
    front — 「SoftwareDistribution\\Download」, 「WeChat Files\\…\\Video」 (UI-W6-32)."""
    name = store.name(node)
    if store.cols["kind"][node] != _DIR or not _generic(name):
        return name
    parent = store.cols["parent"]
    p = int(parent[node])
    skipped = False
    while p > 0 and p != scope and int(parent[p]) > 0:  # never the root itself (a whole path)
        pn = store.name(p)
        if not _generic(pn):
            return f"{pn}{os.sep}{'…' + os.sep if skipped else ''}{name}"
        skipped = True
        p = int(parent[p])
    return name


# --------------------------------------------------------------------------- comparisons


@dataclass(slots=True)
class CompareRoot:
    """A root of a change tree, as views and tags see it (``scan.scanner.RootInfo`` shape)."""

    path: str
    node: int
    volume: Any = None
    whole_volume: bool = False


@dataclass(eq=False)
class CompareHandle:
    id: str
    scan_id: str
    session: Session
    base_spec: str
    target_spec: str
    base: pc.CompareSide | None = None
    target: pc.CompareSide | None = None
    tree: ChangeTree | None = None
    phase: pc.ComparePhase = "loading"
    progress: float | None = None
    message: str = ""
    task: asyncio.Task[None] | None = None
    cancel: threading.Event = field(default_factory=threading.Event)
    listeners: dict[str, Callable[..., None]] = field(default_factory=dict)  # views hook in like a scan
    revision: int = 0
    dirty_at: float | None = None  # the live tree changed (time.monotonic); recompute pending
    computed_at: float = 0.0
    live_task: asyncio.Task[None] | None = None
    last_emit: float = 0.0
    source: str = "compare"

    @property
    def store(self) -> NodeStore:
        if self.tree is None:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["not_ready"])
        return self.tree.store

    @property
    def roots(self) -> list[CompareRoot]:
        store = self.store
        return [CompareRoot(path=store.name(r), node=r) for r in store.roots()]

    @property
    def live(self) -> bool:
        return self.target_spec == pc.NOW or self.base_spec == pc.NOW

    def info(self) -> pc.CompareInfo:
        assert self.tree is not None
        assert self.base is not None
        assert self.target is not None
        t = self.tree
        roots = list(t.store.roots())
        return pc.CompareInfo(
            compare_id=self.id,
            scan_id=self.scan_id,
            base=self.base,
            target=self.target,
            grow=int(sum(int(t.grow[r]) for r in roots)),
            shrink=int(sum(int(t.shrink[r]) for r in roots)),
            changed=int(np.count_nonzero((t.grow[1:] + t.shrink[1:]) > 0)),
            nodes=len(t),
            live=self.live,
            revision=self.revision,
        )

    def event(self) -> pc.CompareProgressEvent:
        return pc.CompareProgressEvent(
            compare_id=self.id,
            phase=self.phase,
            progress=self.progress,
            message=self.message,
            info=self.info() if self.phase == "ready" and self.tree is not None else None,
        )


class CompareManager:
    def __init__(self, scans: ScanManager, views: ViewManager) -> None:
        self.scans = scans
        self.views = views
        self.compares: dict[str, CompareHandle] = {}
        self._ids = itertools.count(1)
        views.compares = self
        # plugins' change annotators (docs/plugins.md): reasons for the items the rules left blank;
        # called on the compute pool with (compare id, items), returns the items
        self.annotate: Callable[[str, list[pc.ChangeItem]], list[pc.ChangeItem]] | None = None

    def _annotated(self, compare_id: str, items: list[pc.ChangeItem]) -> list[pc.ChangeItem]:
        if self.annotate is None or not items:
            return items
        try:
            return self.annotate(compare_id, items)
        except Exception:  # an annotator never breaks the sidebar
            logger.opt(exception=True).debug("change annotators failed")
            return items

    # ------------------------------------------------------------- sources

    def _scan(self, scan_id: str) -> ScanHandle:
        handle = self.scans.scans.get(scan_id)
        if handle is None:
            raise RpcError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {scan_id}")
        return handle

    def get(self, compare_id: str) -> CompareHandle:
        handle = self.compares.get(compare_id)
        if handle is None:
            raise RpcError(ErrorCode.UNKNOWN_SCAN, TEXT["unknown"].format(id=compare_id))
        return handle

    def lookup(self, compare_id: str) -> CompareHandle | None:
        """A comparison whose change tree is ready (views open these like scans)."""
        handle = self.compares.get(compare_id)
        return handle if handle is not None and handle.tree is not None else None

    def _now_side(self, handle: ScanHandle) -> pc.CompareSide:
        pr = handle.progress()
        when = datetime.now(UTC) if handle.source == "scan" else (pr.finished_at or datetime.now(UTC))
        return pc.CompareSide(
            kind="now",
            path=handle.snapshot_path if handle.source == "snapshot" else None,
            created_at=when,
            bytes_alloc=int(sum(int(handle.store.cols["tot_alloc"][r.node]) for r in handle.roots)),
            node_count=len(handle.store),
        )

    async def sources(self, scan_id: str) -> pc.CompareSourcesResult:
        handle = self._scan(scan_id)
        key = snapshot.root_key([r.path for r in handle.roots])
        found = await asyncio.to_thread(snapshot.list_snapshots)
        own = handle.snapshot_path if handle.source == "snapshot" else None
        infos = [
            snapshot_info(path, header)
            for path, header in found
            if snapshot.root_key(header.roots) == key and str(path) != own
        ]
        if handle.running:
            return pc.CompareSourcesResult(snapshots=infos, now=None, now_note=TEXT["busy"])
        return pc.CompareSourcesResult(snapshots=infos, now=self._now_side(handle))

    # ------------------------------------------------------------- lifecycle

    def open(self, session: Session, params: pc.CompareOpenParams) -> pc.CompareTask:
        scan = self._scan(params.scan_id)
        if params.base == params.target:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["same"])
        if pc.NOW in (params.base, params.target) and scan.running:
            raise RpcError(ErrorCode.SCAN_BUSY, TEXT["busy"])
        self._make_room()
        handle = CompareHandle(
            id=f"{PREFIX}{next(self._ids)}",
            scan_id=scan.id,
            session=session,
            base_spec=params.base,
            target_spec=params.target,
        )
        self.compares[handle.id] = handle
        handle.task = asyncio.get_running_loop().create_task(self._run(handle))
        return pc.CompareTask(compare_id=handle.id)

    def _make_room(self) -> None:
        while len(self.compares) >= MAX_COMPARES:
            idle = [h for h in self.compares.values() if not self.views.has_views(h.id)]
            if not idle:
                return
            self.close(idle[0].id)

    def close(self, compare_id: str) -> None:
        handle = self.compares.pop(compare_id, None)
        if handle is None:
            return
        handle.cancel.set()
        for task in (handle.task, handle.live_task):
            if task is not None and not task.done():
                task.cancel()
        self.views.close_scan(compare_id)
        handle.tree = None

    def close_session(self, session: Session) -> None:
        for compare_id in [c for c, h in self.compares.items() if h.session is session]:
            self.close(compare_id)

    def close_scan(self, scan_id: str) -> None:
        """The tab's scan went away: comparisons with its live tree stop following it."""
        for h in self.compares.values():
            if h.scan_id == scan_id and h.live_task is not None:
                h.live_task.cancel()

    # ------------------------------------------------------------- computing

    def _emit(self, handle: CompareHandle, force: bool = True) -> None:
        now = time.monotonic()
        if not force and now - handle.last_emit < PROGRESS_GAP_S:
            return
        handle.last_emit = now
        if handle.id in self.compares:
            handle.session.emit(EVENT_COMPARE_PROGRESS, handle.event())

    async def _side(self, handle: CompareHandle, spec: str) -> tuple[NodeStore, pc.CompareSide]:
        scan = self._scan(handle.scan_id)
        if spec == pc.NOW:
            return scan.store, self._now_side(scan)
        path = Path(spec)
        try:
            store, header = await asyncio.to_thread(snapshot.load, path)
        except (OSError, snapshot.SnapshotError) as exc:
            raise RpcError(ErrorCode.SNAPSHOT_ERROR, TEXT["open"].format(path=path, error=exc)) from exc
        mine = snapshot.root_key([r.path for r in scan.roots])
        if snapshot.root_key(header.roots) != mine:
            a = TEXT["sep"].join(header.roots)
            b = TEXT["sep"].join(r.path for r in scan.roots)
            raise RpcError(ErrorCode.SNAPSHOT_ERROR, TEXT["roots"].format(a=a, b=b))
        info = snapshot_info(path, header)
        side = pc.CompareSide(
            kind="snapshot",
            path=str(path),
            created_at=header.created_at,
            bytes_alloc=info.bytes_alloc,
            node_count=header.node_count,
        )
        return store, side

    async def _compute(self, handle: CompareHandle) -> ChangeTree:
        handle.phase, handle.progress, handle.message = "loading", None, TEXT["loading"]
        self._emit(handle)
        base, handle.base = await self._side(handle, handle.base_spec)
        target, handle.target = await self._side(handle, handle.target_spec)
        handle.phase, handle.progress, handle.message = "comparing", 0.0, TEXT["comparing"]
        self._emit(handle)
        loop = asyncio.get_running_loop()

        def progress(fraction: float) -> None:
            handle.progress = float(fraction)
            loop.call_soon_threadsafe(self._emit, handle, False)

        def work() -> ChangeTree:
            diff = diff_trees(base, target, progress=progress, cancel=handle.cancel.is_set)
            return ChangeTree.of(diff)

        return await compute(work)

    async def _run(self, handle: CompareHandle) -> None:
        try:
            tree = await self._compute(handle)
        except asyncio.CancelledError:
            handle.phase = "cancelled"
            raise
        except CompareCancelled:
            handle.phase, handle.message = "cancelled", ""
            self._emit(handle)
            return
        except RpcError as exc:
            handle.phase, handle.message = "failed", exc.message
            self._emit(handle)
            return
        except Exception as exc:
            logger.exception("comparing {} failed", handle.id)
            handle.phase, handle.message = "failed", TEXT["failed"].format(error=exc)
            self._emit(handle)
            return
        handle.tree = tree
        handle.computed_at = time.monotonic()
        handle.phase, handle.progress, handle.message = "ready", 1.0, ""
        logger.info("compare {}: {} change rows ({:.1f} MB)", handle.id, len(tree), tree.memory_bytes() / 1e6)
        self._emit(handle)

    # ------------------------------------------------------------- live target (UI-W6-38)

    def on_tree_changed(self, scan_id: str) -> None:
        """Files of a scan changed (monitor): its live comparisons are computed again soon."""
        for handle in self.compares.values():
            if handle.scan_id != scan_id or not handle.live or handle.phase != "ready":
                continue
            handle.dirty_at = time.monotonic()
            if handle.live_task is None or handle.live_task.done():
                handle.live_task = asyncio.get_running_loop().create_task(self._follow(handle))

    async def _follow(self, handle: CompareHandle) -> None:
        while handle.dirty_at is not None and handle.id in self.compares:
            now = time.monotonic()
            wait = max(handle.dirty_at + LIVE_QUIET_S, handle.computed_at + LIVE_MIN_GAP_S) - now
            if wait > 0:
                await asyncio.sleep(wait)
                continue
            handle.dirty_at = None
            scan = self.scans.scans.get(handle.scan_id)
            if scan is None or scan.running:
                return
            try:
                old = handle.tree
                tree = await self._compute(handle)
            except (CompareCancelled, RpcError) as exc:
                logger.warning("live compare {} stopped: {}", handle.id, exc)
                handle.phase = "ready"  # keep the last result
                self._emit(handle)
                return
            if handle.id not in self.compares:
                return
            handle.tree = tree
            handle.revision += 1
            handle.computed_at = time.monotonic()
            handle.phase, handle.progress, handle.message = "ready", 1.0, ""
            if old is not None:
                await self.views.replace_store(handle.id, old.store, tree)
            self._emit(handle)

    # ------------------------------------------------------------- sidebar and hover card

    def _host_tree(self, host: ViewHost) -> tuple[CompareHandle, ChangeTree]:
        handle = self.get(host.scan_id)
        if handle.tree is None:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["not_ready"])
        return handle, handle.tree

    def item(self, handle: CompareHandle, tree: ChangeTree, node: int, scope: int) -> pc.ChangeItem:
        store = tree.store
        kind = int(store.cols["kind"][node])
        status = int(tree.status[node])
        path = store.path(node)
        reason = ""
        try:
            from ..insights import match_node

            found = match_node(store, node)
            if found is not None:
                reason = found.note or ""
        except Exception:  # an insights rule problem never breaks the sidebar
            reason = ""
        return pc.ChangeItem(
            node=node,
            path=path,
            label=short_label(store, node, scope),
            kind=_KIND_NAMES.get(kind, "other"),
            delta=int(tree.delta[node]),
            base=int(tree.base_alloc[node]),
            target=int(tree.target_alloc[node]),
            status=_STATUS_NAMES[status] if status < len(_STATUS_NAMES) else "changed",
            new_files=int(tree.new_files[node]),
            reason=reason,
            on_disk=self.on_disk(handle, tree, node),
        )

    def on_disk(self, handle: CompareHandle, tree: ChangeTree, node: int) -> bool:
        """Its file is there to open or delete: the target is the live tree and it was not deleted."""
        scan = self.scans.scans.get(handle.scan_id)
        return (
            handle.target_spec == pc.NOW
            and scan is not None
            and scan.source == "scan"
            and int(tree.status[node]) != Status.REMOVED
        )

    def live_node(self, compare_id: str, node: int) -> tuple[str, int] | None:
        """The same item in the tab's live tree (file operations from a compare view)."""
        handle = self.compares.get(compare_id)
        if handle is None or handle.tree is None or not 0 < node < len(handle.tree):
            return None
        if not self.on_disk(handle, handle.tree, node):
            return None
        scan = self.scans.scans.get(handle.scan_id)
        if scan is None:
            return None
        found = scan.store.find(handle.tree.store.path(node))
        return (scan.id, found) if found is not None else None

    def summary_of(self, host: ViewHost, limit: int) -> pc.CompareSummary:
        """compare.summary (call on the compute pool)."""
        handle, tree = self._host_tree(host)
        return self._summary(handle, tree, host.view, limit)

    def _summary(self, handle: CompareHandle, tree: ChangeTree, view: Any, limit: int) -> pc.CompareSummary:
        store = tree.store
        z = view.zoom
        res = view.filter.result if view.filter.active else None
        match = res.match if res is not None else None
        grow = tree.amounts("grow", match) if match is not None else tree.grow
        shrink = tree.amounts("shrink", match) if match is not None else tree.shrink
        if z.members is not None:
            members = np.asarray(z.members, dtype=np.int64)
            scope_nodes = members
            g = int(grow[members].sum())
            s = int(shrink[members].sum())
        else:
            scope_nodes = np.array([z.node], dtype=np.int64)
            g, s = int(grow[z.node]), int(shrink[z.node])
        whole = z.members is None and z.node == view.home
        grow_items, grow_count = self._units(handle, tree, "grow", grow, scope_nodes, z.node, limit)
        shrink_items, shrink_count = self._units(handle, tree, "shrink", shrink, scope_nodes, z.node, limit)
        both = self._annotated(handle.id, grow_items + shrink_items)
        grow_items, shrink_items = both[: len(grow_items)], both[len(grow_items) :]
        assert handle.base is not None
        assert handle.target is not None
        span = (handle.target.created_at - handle.base.created_at).total_seconds()
        net = g - s
        free = daily = days = None
        if handle.target_spec == pc.NOW:
            scan = self.scans.scans.get(handle.scan_id)
            if scan is not None:
                frees = [r.volume.free for r in scan.roots if r.volume is not None]
                free = int(sum(frees)) if frees else None
        if net > 0 and span > 0:
            daily = net / (span / 86400)
            if free is not None and daily > 0:
                days = free / daily
        scope = store.path(z.node) if z.node > 0 else ""
        return pc.CompareSummary(
            view_id=view.id,
            compare_id=handle.id,
            scope=scope,
            whole=whole,
            base=handle.base,
            target=handle.target,
            span_s=span,
            grow=g,
            shrink=s,
            net=net,
            free=free,
            daily=daily,
            days_to_full=days,
            grow_items=grow_items,
            shrink_items=shrink_items,
            grow_count=grow_count,
            shrink_count=shrink_count,
        )

    def _units(
        self,
        handle: CompareHandle,
        tree: ChangeTree,
        direction: Literal["grow", "shrink"],
        amount: np.ndarray,
        scope: np.ndarray,
        scope_node: int,
        limit: int,
    ) -> tuple[list[pc.ChangeItem], int]:
        """Change units inside the scope, largest first, by the engine's rule (``DiffTree.changes``:
        they partition the change, a folder whose change comes mostly from a few children is broken
        down and its remainder is one 「其余」 unit). An aggregate zoom lists its members."""
        if len(scope) != 1:
            members = scope[amount[scope] > 0]
            members = members[np.argsort(-amount[members], kind="stable")]
            shown = [self.item(handle, tree, int(n), scope_node) for n in members[:limit].tolist()]
            return shown, len(members)
        units = tree.diff.changes(direction, max(limit, COUNT_UNITS), scope=scope_node, amount=amount)
        items = [self._unit_item(handle, tree, u, scope_node) for u in units[:limit]]
        return items, len(units)

    def _unit_item(self, handle: CompareHandle, tree: ChangeTree, unit: Change, scope: int) -> pc.ChangeItem:
        item = self.item(handle, tree, unit.node, scope)
        if not unit.rest:
            return item
        # the rest of a broken-down folder: its other changed entries together
        return item.model_copy(
            update={
                "kind": "group",
                "label": TEXT["rest"].format(label=item.label, n=unit.members),
                "delta": unit.delta,
                "base": unit.base_alloc,
                "target": unit.target_alloc,
                "status": "changed",
                "members": unit.members,
                "on_disk": False,
            }
        )

    async def node(self, host: ViewHost, key: int, children: int) -> pc.CompareNodeResult:
        handle, tree = self._host_tree(host)
        frame = host.view.last
        if frame is None or (row := frame.draw.row(key)) is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, f"no block {key} in the last frame")
        draw = frame.draw
        node = int(draw.node[row])
        members = draw.members.get(int(draw.key[row]))

        def work() -> pc.CompareNodeResult:
            scope = host.view.zoom.node
            if members is not None and len(members):
                ids = np.asarray(members, dtype=np.int64)
                folder = int(store_parent(tree.store, int(ids[0])))
                item = pc.ChangeItem(
                    node=folder,
                    path=tree.store.path(folder),
                    label=draw.names[row] if row < len(draw.names) else "",
                    kind="group",
                    delta=int(tree.delta[ids].sum()),
                    base=int(tree.base_alloc[ids].sum()),
                    target=int(tree.target_alloc[ids].sum()),
                    status="changed",
                    new_files=int(tree.new_files[ids].sum()),
                    members=len(ids),
                    on_disk=False,
                )
                kids = ids
            else:
                item = self.item(handle, tree, node, scope)
                lo, hi = tree.store.child_range(node) if tree.store.cols["kind"][node] == _DIR else (0, 0)
                kids = np.arange(lo, hi, dtype=np.int64)
            if children and kids.size:
                order = np.argsort(-np.abs(tree.delta[kids]), kind="stable")[:children]
                top = [self.item(handle, tree, int(k), scope) for k in kids[order].tolist()]
            else:
                top = []
            both = self._annotated(handle.id, [item, *top])
            return pc.CompareNodeResult(item=both[0], children=both[1:])

        return await compute(work)


def store_parent(store: NodeStore, node: int) -> int:
    return int(store.cols["parent"][node])

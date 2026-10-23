"""The delete basket and file operations on the server (W5, docs/manage.md §2).

The basket lives here, one per backend, shared by every session: ``basket.changed`` goes to all of
them. Executing it runs the operation in a worker thread (``ops.trash``), streams ``ops.progress``
to the session that asked, and keeps what undo needs: the trash references of what went to the
recycle bin, the items that left the basket. A permanent deletion waits :data:`DELETE_DELAY_S`
first (the undo window: nothing has been touched yet). Afterwards the tree follows: with live
monitoring the file change hooks see it; without, the parent folders are read again.

Moving to the trash never turns into a permanent deletion: an item the trash cannot take comes
back as ``no_trash`` and stays where it is (``ops.trash`` guarantees it, this layer only reports).
"""

from __future__ import annotations

import asyncio
import itertools
import os
import shutil
import threading
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

import numpy as np
from loguru import logger

from spaceknife_protocol import ops as po
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EVENT_BASKET, EVENT_OPS_PROGRESS

from .. import fmt
from ..dupes.guard import Elsewhere
from ..i18n import Texts
from ..ops.basket import AddResult, Basket, BasketItem, Refused
from ..store.nodes import NodeStore
from .compute import compute
from .rpc import RpcError
from .scans import on_disk

if TYPE_CHECKING:
    from ..ops.trash import OpResult, TrashStatus
    from .app import Server
    from .session import Session

DELETE_DELAY_S = 5.0  # permanent deletion: the undo window before anything is touched
PROGRESS_GAP_S = 0.1
TRASH_INFO_TTL_S = 30.0  # recycle bin state per volume, re-read at most this often
KEEP_TASKS = 8  # finished operations remembered for undo
REFRESH_QUIET_S = 1.0  # after file changes, the basket is checked again once this quiet

# User-facing text, kept in one place for the i18n extraction (M7.5).
TEXT = Texts(
    {
        "not_on_disk": "只能操作「现在」还在磁盘上的项",
        "no_block": "这个块已经不在视图里了，请再试一次",
        "busy": "还有文件操作在进行，完成后再执行",
        "unknown_task": "没有这个文件操作：{id}",
        "nothing": "删除篮里没有可以执行的项",
        "trash_same": "回收站在同一个卷上：清空回收站后才真正释放空间",
        "trash_used": "{note}，回收站现在有 {size}",
    }
)


@dataclass(eq=False)
class OpRun:
    id: str
    mode: po.OpMode
    session: Session
    total: int
    paths: list[str]
    sizes: dict[str, int]
    alloc: dict[str, int]  # item path -> freeable bytes (what a success frees)
    scans: dict[str, str]  # path -> scan id (refresh the tree afterwards)
    roots: dict[str, str] = field(default_factory=dict)  # path -> its scan root (ops.trash: case twins)
    guards: dict[str, Any] = field(default_factory=dict)  # extra copies of duplicate files (dupes.guard)
    done: int = 0
    current: str = ""
    phase: str = "waiting"
    results: list[OpResult] = field(default_factory=list)
    freed: int = 0
    cancel: threading.Event = field(default_factory=threading.Event)
    task: asyncio.Task[None] | None = None
    left: list[BasketItem] = field(default_factory=list)  # items that left the basket (undo)
    refs: list[str] = field(default_factory=list)  # trash references (undo)
    last_emit: float = 0.0

    def event(self) -> po.OpProgressEvent:
        final = self.phase in ("done", "cancelled", "undone")
        return po.OpProgressEvent(
            task_id=self.id,
            mode=self.mode,
            done=self.done,
            total=self.total,
            current=self.current,
            phase=self.phase,  # type: ignore[arg-type]
            results=[
                po.OpItemResult(path=r.path, ok=r.ok, code=r.code, message=r.message) for r in self.results
            ]
            if final
            else [],
            freed=self.freed,
            can_undo=(self.phase == "waiting") or (self.phase == "done" and bool(self.refs)),
        )


class OpsService:
    def __init__(self, server: Server) -> None:
        self.server = server
        self.basket = Basket()
        self.runs: dict[str, OpRun] = {}
        self._ids = itertools.count(1)
        self._trash_info: dict[str, tuple[float, TrashStatus]] = {}
        self._refresh_task: asyncio.Task[None] | None = None
        self._dirty_at = 0.0
        self._lock_task: asyncio.Task[None] | None = None

    # ------------------------------------------------------------- state

    def _store_of(self, scan_id: str) -> NodeStore | None:
        handle = self.server.scans.scans.get(scan_id)
        return handle.store if handle is not None else None

    def _trash(self, volume: str) -> TrashStatus:
        from ..ops.trash import trash_status

        hit = self._trash_info.get(volume)
        now = time.monotonic()
        if hit is None or now - hit[0] > TRASH_INFO_TTL_S:
            hit = (now, trash_status(volume))
            self._trash_info[volume] = hit
        return hit[1]

    def _state_sync(self) -> po.BasketState:
        """The basket for the front ends (reads volumes: call in a worker thread)."""
        items = self.basket.items()
        freeable = self.basket.freeable()
        volumes: list[po.BasketVolume] = []
        for mount in sorted({it.volume for it in items if it.volume}):
            try:
                usage = shutil.disk_usage(mount)
                total, free = usage.total, usage.free
            except OSError:
                total = free = 0
            st = self._trash(mount)
            if st.available:
                note = TEXT["trash_same"] if st.same_volume else st.location
                if st.used >= 0:
                    note = TEXT["trash_used"].format(note=note, size=fmt.size(st.used))
            else:
                note = st.reason
            volumes.append(
                po.BasketVolume(
                    mount_point=mount,
                    total=total,
                    free=free,
                    freeable=int(freeable.get(mount, 0)),
                    trash=st.available,
                    trash_note=note,
                )
            )
        return po.BasketState(
            items=[_item_model(it) for it in items],
            volumes=volumes,
            freeable=int(sum(freeable.values())),
        )

    async def state(self) -> po.BasketState:
        return await asyncio.to_thread(self._state_sync)

    async def broadcast(self) -> po.BasketState:
        state = await self.state()
        for session in list(self.server.sessions):
            session.emit(EVENT_BASKET, po.BasketEvent(state=state))
        return state

    # ------------------------------------------------------------- path forms (W5-09…14)

    def path_forms(self, scan_id: str, node: int) -> po.PathForms:
        from ..ops.paths import path_forms

        cmp = self.server.compares.lookup(scan_id)
        if cmp is not None and cmp.tree is not None:
            store = cmp.tree.store
            if not 0 < node < len(store):
                raise RpcError(ErrorCode.UNKNOWN_NODE, f"unknown node: {node}")
            path = store.path(node)
            exists = self.server.compares.on_disk(cmp, cmp.tree, node)
        else:
            handle = self.server.scans.get(scan_id)
            self.server.scans.check_node(handle, node)
            path = handle.store.path(node)
            exists = on_disk(handle.store, node)
        f = path_forms(path)
        return po.PathForms(raw=f.raw, quoted=f.quoted, posix=f.posix, uri=f.uri, on_disk=exists)

    # ------------------------------------------------------------- adding

    def _live(self, scan_id: str, nodes: list[int]) -> tuple[str, list[int], list[Refused]]:
        """Nodes of a compare view's change tree → the same items in the tab's live tree."""
        compares = self.server.compares
        cmp = compares.lookup(scan_id)
        if cmp is None or cmp.tree is None:
            return scan_id, nodes, []
        live: list[int] = []
        refused: list[Refused] = []
        target = cmp.scan_id
        for node in nodes:
            found = compares.live_node(scan_id, node)
            if found is None:
                path = cmp.tree.store.path(node) if 0 < node < len(cmp.tree) else str(node)
                refused.append(Refused(path, TEXT["not_on_disk"]))
            else:
                target, n = found
                live.append(n)
        return target, live, refused

    async def add(self, session: Session, params: po.BasketAddParams) -> po.BasketAddResult:
        scan_id = params.scan_id
        nodes = list(params.nodes)
        members: list[int] | None = None
        if params.key is not None and params.view_id is not None:
            host = self.server.views.get(session, params.view_id)
            frame = host.view.last
            row = frame.draw.row(params.key) if frame is not None else None
            if frame is None or row is None:
                raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_block"])
            group = frame.draw.members.get(int(params.key))
            if group is not None and len(group):
                members = [int(m) for m in np.asarray(group).tolist()]
                nodes = [int(host.view.store.cols["parent"][members[0]])]
            else:
                nodes = [int(frame.draw.node[row])]
            scan_id = host.scan_id
        target, live, refused = self._live(scan_id, nodes)
        if members is not None and target != scan_id:  # a group of a compare view: its members, live
            _, live_members, more = self._live(scan_id, members)
            refused += more
            members = live_members
        live_store = self._store_of(target)
        if live_store is None:
            raise RpcError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {target}")
        result = AddResult(refused=list(refused))
        if live and (members is None or members):
            added = await compute(self.basket.add, live_store, target, live, members)
            result.added, result.merged = added.added, added.merged
            result.absorbed, result.refused = added.absorbed, result.refused + added.refused
        state = await self.broadcast()
        self._check_locks()
        return po.BasketAddResult(
            state=state,
            added=len(result.added),
            merged=len(result.merged),
            absorbed=len(result.absorbed),
            refused=[po.Refusal(path=r.path, reason=r.reason) for r in result.refused],
        )

    async def remove(self, ids: list[str]) -> po.BasketState:
        self.basket.remove(ids)
        return await self.broadcast()

    async def clear(self) -> po.BasketState:
        self.basket.clear()
        return await self.broadcast()

    def _check_locks(self) -> None:
        """Who holds basket items open (UI-W5-37), in the background; the drawer updates."""
        if self._lock_task is not None and not self._lock_task.done():
            return

        async def run() -> None:
            from ..ops.locks import find_lockers

            try:
                paths = self.basket.lock_targets()
                if not paths:
                    return
                before = self.basket.version
                lockers = await asyncio.to_thread(find_lockers, paths)
                self.basket.set_lockers(lockers)
                if self.basket.version != before:
                    await self.broadcast()
            except Exception:
                logger.exception("checking basket locks failed")

        self._lock_task = asyncio.get_running_loop().create_task(run())

    # ------------------------------------------------------------- tree changes

    def on_tree_changed(self, scan_id: str) -> None:
        """Files changed (monitor, or an operation of ours): sizes and missing items again."""
        if not len(self.basket) or not any(it.scan_id == scan_id for it in self.basket.items()):
            return
        self._dirty_at = time.monotonic()
        if self._refresh_task is None or self._refresh_task.done():
            self._refresh_task = asyncio.get_running_loop().create_task(self._refresh_later())

    async def _refresh_later(self) -> None:
        while True:
            wait = self._dirty_at + REFRESH_QUIET_S - time.monotonic()
            if wait <= 0:
                break
            await asyncio.sleep(wait)
        try:
            await compute(self.basket.refresh, self._store_of)
            await self.broadcast()
        except Exception:
            logger.exception("refreshing the basket failed")

    # ------------------------------------------------------------- executing (W5-41…49)

    def execute(self, session: Session, params: po.BasketExecuteParams) -> po.OpTask:
        if any(r.phase in ("waiting", "running") for r in self.runs.values()):
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["busy"])
        targets = self.basket.targets(params.ids)
        if not targets:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["nothing"])
        items = {it.id: it for it in self.basket.items()}
        alloc: dict[str, int] = {}
        scans: dict[str, str] = {}
        for t in targets:
            it = items.get(t.item)
            if it is None:
                continue
            scans[t.path] = it.scan_id
            # a group frees its members' share; a single item its own freeable bytes
            alloc[t.path] = it.alloc // max(1, len(it.members)) if it.members else it.alloc
        run = OpRun(
            id=f"op-{next(self._ids)}",
            mode=params.mode,
            session=session,
            total=len(targets),
            paths=[t.path for t in targets],
            sizes={t.path: t.size for t in targets},
            alloc=alloc,
            scans=scans,
            # kept with each item, so it holds once its scan is closed too
            roots={t.path: t.root for t in targets if t.root},
            guards=self.basket.guards_for(t.path for t in targets),
            phase="waiting" if params.mode == "delete" else "running",
        )
        self.runs[run.id] = run
        while len(self.runs) > KEEP_TASKS:
            del self.runs[next(iter(self.runs))]
        run.task = asyncio.get_running_loop().create_task(self._run(run))
        logger.info("file operation {}: {} {} items", run.id, run.mode, run.total)
        return po.OpTask(
            task_id=run.id,
            mode=run.mode,
            total=run.total,
            delay_s=DELETE_DELAY_S if run.mode == "delete" else 0.0,
        )

    def _doomed(self, run: OpRun) -> Elsewhere | None:
        """Duplicate-file guards: a keeper is lost too when anything in the basket covers it, is
        it under another path (an extra copy of another search), or is a folder above it."""
        if not run.guards:
            return None
        basket = self.basket
        return Elsewhere(
            covers=lambda path: basket.contains(path) is not None,
            members=basket.dupes_ids,
            plain=basket.plain_paths,
        )

    def _emit(self, run: OpRun, force: bool = True) -> None:
        now = time.monotonic()
        if not force and now - run.last_emit < PROGRESS_GAP_S:
            return
        run.last_emit = now
        run.session.emit(EVENT_OPS_PROGRESS, run.event())

    async def _run(self, run: OpRun) -> None:
        from ..ops.trash import delete_permanently, move_to_trash

        try:
            if run.mode == "delete":
                self._emit(run)
                try:
                    await asyncio.sleep(DELETE_DELAY_S)
                except asyncio.CancelledError:
                    run.phase = "undone"  # called off in the undo window: nothing was touched
                    self._emit(run)
                    return
                run.phase = "running"
            self._emit(run)
            loop = asyncio.get_running_loop()

            def progress(done: int, total: int, res: OpResult) -> None:
                def apply() -> None:
                    run.done, run.current = done, res.path
                    self._emit(run, force=False)

                loop.call_soon_threadsafe(apply)

            if run.mode == "trash":
                results = await asyncio.to_thread(
                    move_to_trash,
                    run.paths,
                    progress=progress,
                    cancel=run.cancel.is_set,
                    sizes=run.sizes,
                    roots=run.roots,
                    guards=run.guards,
                    also_doomed=self._doomed(run),
                )
            else:
                results = await asyncio.to_thread(
                    delete_permanently,
                    run.paths,
                    progress=progress,
                    cancel=run.cancel.is_set,
                    roots=run.roots,
                    guards=run.guards,
                    also_doomed=self._doomed(run),
                )
            run.results = results
            run.done = sum(1 for r in results if r.code != "cancelled")
            run.freed = sum(run.alloc.get(r.path, 0) for r in results if r.ok)
            run.refs = [r.trash_ref for r in results if r.ok and r.trash_ref]
            run.left = self.basket.apply_results(results)
            run.phase = "cancelled" if run.cancel.is_set() else "done"
            failed = [r for r in results if not r.ok and r.code != "cancelled"]
            for r in failed[:20]:
                logger.warning("{}: {} ({})", r.path, r.message, r.code)
            logger.info(
                "file operation {}: {} ok, {} failed, {} freed",
                run.id,
                sum(1 for r in results if r.ok),
                len(failed),
                fmt.size(run.freed),
            )
        except Exception:
            logger.exception("file operation {} failed", run.id)
            run.phase = "done"
        self._emit(run)
        await self._after(run, [r.path for r in run.results if r.ok])
        await self.broadcast()

    async def _after(self, run: OpRun, paths: list[str]) -> None:
        """The tree follows what changed: monitored scans see it, the others read the parents again."""
        by_scan: dict[str, set[str]] = {}
        for path in paths:
            scan_id = run.scans.get(path)
            if scan_id:
                by_scan.setdefault(scan_id, set()).add(os.path.dirname(path.rstrip("\\/")) or path)
        monitors = self.server.monitors
        for scan_id, parents in by_scan.items():
            handle = self.server.scans.scans.get(scan_id)
            if handle is None or handle.running or handle.monitor is not None:
                continue
            for parent in sorted(parents):
                node = handle.store.find(parent)
                if node is not None:
                    try:
                        await monitors.rescan(handle, node)
                    except RpcError as exc:
                        logger.warning("re-reading {} after a file operation: {}", parent, exc.message)

    def _get(self, task_id: str) -> OpRun:
        run = self.runs.get(task_id)
        if run is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["unknown_task"].format(id=task_id))
        return run

    def cancel(self, task_id: str) -> None:
        run = self._get(task_id)
        if run.phase == "waiting" and run.task is not None:
            run.task.cancel()
        elif run.phase == "running":
            run.cancel.set()

    async def undo(self, task_id: str) -> None:
        """Call off a deletion still waiting, or put what went to the trash back (UI-W5-44)."""
        from ..ops.trash import restore

        run = self._get(task_id)
        if run.phase == "waiting":
            self.cancel(task_id)
            return
        if run.phase != "done" or not run.refs:
            return
        refs, run.refs = run.refs, []
        results = await asyncio.to_thread(restore, refs)
        back = [r for r in results if r.ok]
        for r in results:
            if not r.ok:
                logger.warning("restoring {}: {} ({})", r.path, r.message, r.code)
        restored = {r.path for r in back}
        self.basket.reinstate([it for it in run.left if _restored(it, restored)])
        run.phase = "undone"
        run.results = results
        self._emit(run)
        await self._after(run, sorted(restored))
        await compute(self.basket.refresh, self._store_of)
        await self.broadcast()


def _restored(item: BasketItem, restored: set[str]) -> bool:
    if item.path in restored:
        return True
    return any(m in restored for m in item.members)


def _item_model(it: BasketItem) -> po.BasketItem:
    return po.BasketItem(
        id=it.id,
        scan_id=it.scan_id,
        path=it.path,
        name=it.name,
        kind=it.kind,
        members=len(it.members),
        alloc=it.alloc,
        size=it.size,
        files=it.files,
        dirs=it.dirs,
        volume=it.volume,
        status=it.status,
        note=it.note,
    )

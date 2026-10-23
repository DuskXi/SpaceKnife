"""Duplicate-file searches on the server (docs/dupes.md §9.6, §10, §11).

One search runs at a time per backend. Its results stay until ``dupes.clear``, a new search over
the same scans, or until one of its scans closes (or leaves memory): then a running search stops
and the results go (a cleanup run already started goes on: every step carries its paths and
identities, docs/dupes.md §16). Nothing here changes a file: cleaning up goes through the cleanup
plan (``server/dupes_cleanup.py``), reviewed, confirmed and checked again. Events go to every session that
announced ``dupes.v1``: ``dupes.progress`` at most four times a second, ``dupes.changed`` at most
once a second; views in the ``dupes`` colouring are repainted at most once a second while groups
come in.

A search only covers finished live scans (snapshots and comparisons show the past: their files
cannot be read). File changes the monitor reports take changed copies out of their groups (they go
back to 「待核对」 until the next search, which reads only what changed thanks to the hash cache).
"""

from __future__ import annotations

import asyncio
import contextlib
import itertools
import os
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

import numpy as np
from loguru import logger

from spaceknife_protocol import CAP_DUPES
from spaceknife_protocol import dupes as pd
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EVENT_DUPES_CHANGED, EVENT_DUPES_PROGRESS

from .. import fmt
from ..dupes import Job, JobOptions, KeepOptions, Results, ScanInput, Scope
from ..dupes import cache as dcache
from ..dupes import codes as C
from ..dupes.guard import Stamped
from ..dupes.select import plan as make_plan
from ..dupes.select import select
from ..i18n import Texts, tr
from .compute import compute
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .scans import ScanHandle

PROGRESS_GAP_S = 0.25
CHANGED_GAP_S = 1.0
REPAINT_GAP_S = 1.0

TEXT = Texts(
    {
        "unknown": "没有这次查找：{id}",
        "busy": "已经有一次查找在进行；先停止它",
        "not_live": "快照是过去的状态，不能核对内容",
        "scanning": "扫描还没有完成，完成后才能查找重复文件",
        "no_scan": "没有这个扫描：{id}",
        "no_view": "这个视图不属于这次查找的扫描",
        "may_share": "这个卷能共享数据块，没能查到哪些副本共享：实际腾出的可能更少",
        "network": "网络盘上的文件可能是同一个文件，只保留不删除",
        "failed": "查找中途出错，已核对的 {groups} 组保留，可以再查一次（详情见日志）",
        "failed_none": "查找中途出错，可以再查一次（详情见日志）",
    }
)


@dataclass(eq=False)
class JobHandle:
    id: str
    params: pd.DupesStartParams
    scan_ids: list[str]
    job: Job
    results: Results
    task: asyncio.Task[None] | None = None
    started_at: float = field(default_factory=time.time)
    last_progress: float = 0.0
    last_changed: float = 0.0
    last_repaint: float = 0.0
    pending_changed: bool = False
    changed_timer: asyncio.TimerHandle | None = None
    version: int = 0
    rate_t: float = 0.0
    rate_b: int = 0
    rate: float = 0.0
    keep_params: pd.DupesKeepParams | None = None

    @property
    def running(self) -> bool:
        return self.task is not None and not self.task.done()


class DupesManager:
    def __init__(self, server: Server) -> None:
        self.server = server
        self.jobs: dict[str, JobHandle] = {}
        self._ids = itertools.count(1)
        # what this backend wrote to last.json lately: a plan right after a search sees it even
        # before the write in the background is done
        self._last: dict[str, dict[str, Any]] = {}
        self._closing: set[asyncio.Task[None]] = set()

    # ------------------------------------------------------------------ scans and scopes
    def _live(self, scan_ids: list[str]) -> list[ScanHandle]:
        out = []
        for sid in scan_ids:
            h = self.server.scans.scans.get(sid)
            if h is None:
                raise RpcError(ErrorCode.UNKNOWN_SCAN, TEXT["no_scan"].format(id=sid))
            if h.source != "scan":
                raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["not_live"])
            if h.running:
                raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["scanning"])
            out.append(h)
        return out

    def _scope(self, sc: pd.DupesScope) -> Scope:
        ids = list(dict.fromkeys(sc.scan_ids))
        if not sc.cross_volume:
            ids = ids[:1]
        handles = self._live(ids)
        inputs = [ScanInput(h.id, h.store, h.roots) for h in handles]
        st = self.server.settings
        return Scope(
            inputs,
            mode=sc.mode,
            scan=0,
            node=sc.node or 0,
            min_size=max(sc.min_size, 1),
            include_system=sc.include_system or bool(st and st.get("dupes.include_system")),
            include_network=sc.include_network or bool(st and st.get("dupes.include_network")),
        )

    def get(self, job_id: str) -> JobHandle:
        h = self.jobs.get(job_id)
        if h is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["unknown"].format(id=job_id))
        return h

    def for_scan(self, scan_id: str) -> tuple[JobHandle, int] | None:
        """The newest search covering ``scan_id`` and the scan's index in it."""
        for h in reversed(list(self.jobs.values())):
            if scan_id in h.scan_ids:
                return h, h.scan_ids.index(scan_id)
        return None

    def has_dupes(self, scan_id: str) -> bool:
        return self.for_scan(scan_id) is not None

    def palette_status(self, scan_id: str) -> str:
        """「正在核对 43%」 while a search runs, 「可腾出 12.3 GB」 after it."""
        hit = self.for_scan(scan_id)
        if hit is None:
            return ""
        h = hit[0]
        if h.running:
            pr = h.job.progress()
            return tr("正在核对 {pct}%", pct=round(100 * (pr or 0)))
        _, _, reclaim = h.results.totals()
        return tr("可腾出 {size}", size=fmt.size(reclaim))

    # ------------------------------------------------------------------ plan / start / control
    async def plan(self, sc: pd.DupesScope) -> pd.DupesPlan:
        scope = self._scope(sc)
        sel = await compute(select, scope)
        p = make_plan(sel, n_scans=len(scope.scans))
        num: dict[str, Any] = p
        cross_extra = None
        if sc.cross_volume and len(scope.scans) > 1:
            first = Scope(scope.scans[:1], min_size=scope.min_size, include_system=scope.include_system)
            alone: dict[str, Any] = make_plan(await compute(select, first), n_scans=1)
            cross_extra = {
                "files": int(num["candidates"]) - int(alone["candidates"]),
                "bytes": int(num["bytes"]) - int(alone["bytes"]),
            }
        last = await asyncio.to_thread(dcache.read_last)
        last.update(self._last)
        roots = ";".join(r.path for s in scope.scans for r in s.roots)
        hit: dict[str, Any] = last.get(roots) or {}
        covering = self.for_scan(sc.scan_ids[0])
        st = self.server.settings
        per_scan = np.bincount(sel.cand["scan"], minlength=len(scope.scans)) if len(sel.cand) else None
        by_disk: dict[str, tuple[str, list[str]]] = {}  # device -> (mount, scans)
        for s in scope.scans:
            for r in s.roots:
                vol = r.volume
                key = vol.device if vol is not None and vol.device else r.path
                mount, ids = by_disk.setdefault(key, (vol.mount_point if vol else r.path, []))
                if s.scan_id not in ids:
                    ids.append(s.scan_id)
        reg = self.server.scans.io
        disks: list[pd.DupesPlanDisk] = []
        for mount, ids in by_disk.values():
            media = "unknown"
            with contextlib.suppress(Exception):
                if reg.running:
                    media = str((await reg.governor_for_path(mount)).disk.media)
            disks.append(pd.DupesPlanDisk(disk=mount, media=media, scans=ids))
        return pd.DupesPlan(
            min_size=scope.min_size,
            load=(str(st.get("dupes.load")) if st else "quiet"),  # type: ignore[arg-type]
            scans=[
                pd.DupesPlanScan(
                    scan_id=s.scan_id,
                    roots=[r.path for r in s.roots],
                    candidates=int(per_scan[i]) if per_scan is not None else 0,
                )
                for i, s in enumerate(scope.scans)
            ],
            disks=disks,
            current=self.status_of(covering[0]) if covering else None,
            candidates=int(num["candidates"]),
            groups=int(num["groups"]),
            bytes=int(num["bytes"]),
            probe_bytes=int(num["probe_bytes"]),
            full_bytes_max=int(num["full_bytes_max"]),
            reclaim_max=int(num["reclaim_max"]),
            excluded=dict(num["excluded"]),
            cross_extra=cross_extra,
            last=pd.DupesLast(
                at=str(hit.get("at", "")),
                groups=int(hit.get("groups", 0)),
                files=int(hit.get("files", 0)),
                reclaim=int(hit.get("reclaim", 0)),
                rate=float(hit.get("rate", 0) or 0),
            )
            if hit
            else None,
        )

    def _keep_options(self) -> KeepOptions:
        st = self.server.settings
        if st is None:
            return KeepOptions()
        return KeepOptions(
            rule=str(st.get("dupes.keep_rule")),  # type: ignore[arg-type]
            folder_consistent=bool(st.get("dupes.folder_consistent")),
            per_volume=bool(st.get("dupes.cross_keep_per_volume")),
        )

    async def start(self, params: pd.DupesStartParams) -> pd.DupesStatus:
        sc = params.scope
        running = [h for h in self.jobs.values() if h.running]
        if running:
            cur = running[0]
            inside = set(sc.scan_ids[: (len(sc.scan_ids) if sc.cross_volume else 1)]) <= set(cur.scan_ids)
            if sc.mode == "file" and sc.node and inside and cur.job.sel is not None:
                self._hurry(cur, sc)
                return self.status_of(cur)
            if not params.replace:
                raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["busy"])
            await cur.job.cancel()
        scope = self._scope(sc)
        ids = [s.scan_id for s in scope.scans]
        # results of these scans are replaced; their manual choices (digest + path) carry over
        carried: dict[tuple[bytes, str], Any] = {}
        for old in [h for h in self.jobs.values() if set(h.scan_ids) & set(ids)]:
            carried.update(old.results.keep.overrides)
            self.jobs.pop(old.id, None)
        st = self.server.settings
        load = params.load or (str(st.get("dupes.load")) if st else "quiet")
        keep_atime = str(st.get("dupes.keep_access_time")) if st else "skip"
        opts = JobOptions(
            load=load,  # type: ignore[arg-type]
            load_custom=params.load_custom,
            keep_atime=keep_atime,  # type: ignore[arg-type]
            budget_bytes=params.budget_bytes,
            max_seconds=params.max_seconds,
        )
        keep = self._keep_options()
        keep.overrides = carried
        results = Results(scope.scans, keep)
        job_id = f"dupes-{next(self._ids)}"
        handle = JobHandle(job_id, params, ids, None, results)  # type: ignore[arg-type]
        job = Job(
            scope,
            options=opts,
            registry=self.server.scans.io if self.server.scans.io.running else None,
            scan_busy=self._scan_busy,
            sink=results,
            on_progress=lambda _j: self._on_progress(handle),
            compute=compute,
        )
        handle.job = job
        self.jobs[job_id] = handle
        handle.task = asyncio.create_task(self._run(handle), name=job_id)
        logger.info("dupes {} started: {} (cross {})", job_id, ", ".join(ids), sc.cross_volume)
        return self.status_of(handle)

    def _hurry(self, h: JobHandle, sc: pd.DupesScope) -> None:
        """A file's own group goes first in a running search."""
        sel = h.job.sel
        if sel is None or sc.node is None:
            return
        si = h.scan_ids.index(sc.scan_ids[0])
        hit = np.flatnonzero((sel.cand["scan"] == si) & (sel.cand["node"] == sc.node))
        if len(hit):
            h.job.urgent.append(int(sel.cand["group"][hit[0]]))

    async def _run(self, h: JobHandle) -> None:
        try:
            await h.job.run()
        finally:
            self._remember(h)
            self._on_progress(h, force=True)
            self._changed(h, "verified", force=True)
            self._repaint(h, force=True)

    def _remember(self, h: JobHandle) -> None:
        if h.job.state != "done":
            return
        groups, files, reclaim = h.results.totals()
        roots = ";".join(r.path for s in h.job.scope.scans for r in s.roots)
        c = h.job.counters
        read = c.bytes_probe + c.bytes_full
        rate = read / h.job.elapsed if h.job.elapsed > 0 and read >= 64 << 20 else 0.0
        entry = {
            roots: {
                "at": datetime.now(UTC).isoformat(),
                "groups": groups,
                "files": files,
                "reclaim": reclaim,
                "rate": round(rate, 1),
            }
        }
        self._last.update(entry)
        loop = asyncio.get_running_loop()
        loop.run_in_executor(None, dcache.write_last, entry)

    def _scan_busy(self, disk: str) -> bool:
        for s in self.server.scans.scans.values():
            if s.running and s.governor is not None and str(s.governor.disk.id) == disk:
                return True
        return False

    async def pause(self, job_id: str) -> pd.DupesStatus:
        h = self.get(job_id)
        await h.job.pause()
        return self.status_of(h)

    async def resume(self, job_id: str) -> pd.DupesStatus:
        h = self.get(job_id)
        h.job.resume()
        return self.status_of(h)

    async def cancel(self, job_id: str) -> pd.DupesStatus:
        h = self.get(job_id)
        await h.job.cancel()
        if h.task is not None:
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await asyncio.wait_for(asyncio.shield(h.task), 5)
        return self.status_of(h)

    def set_load(self, params: pd.DupesLoadParams) -> pd.DupesStatus:
        h = self.get(params.job_id)
        h.job.set_load(params.level, params.custom)
        return self.status_of(h)

    async def clear(self, job_id: str) -> None:
        h = self.get(job_id)
        if h.running:
            await h.job.cancel()
        self.jobs.pop(job_id, None)
        self._changed(h, "cleared", force=True)
        # views in the dupes colouring go back to class colours
        for host in list(self.server.views.views.values()):
            if host.scan_id in h.scan_ids and host.view.color_mode == "dupes":
                host.view.color_mode = "class"
                self.server.views.push_change(host)

    # ------------------------------------------------------------------ status and events
    def status_of(self, h: JobHandle) -> pd.DupesStatus:
        j = h.job
        f = j.funnel
        c = j.counters
        sel = j.sel
        planned = 0
        upper = 0
        if sel is not None and len(sel.groups):
            sizes = sel.groups["size"].astype(np.int64)
            counts = sel.groups["count"].astype(np.int64)
            upper = int((sizes * counts).sum())
            planned = int((counts * 2 * C.BLOCK).sum())
        read = c.bytes_probe + c.bytes_full
        now = time.monotonic()
        if now - h.rate_t >= 1.0:
            if h.rate_t:
                h.rate = max(0.0, (read - h.rate_b) / (now - h.rate_t))
            h.rate_t, h.rate_b = now, read
        eta = None
        if j.state in ("identifying", "probing", "verifying") and h.rate > 0:
            eta = max(0.0, (upper - read) / h.rate)
        ledger: list[pd.DupesLedgerVolume] = []
        for v, led in sorted(h.results.ledger().items()):
            vol = j.vols[v] if v < len(j.vols) else None
            note = ""
            if vol is not None and "may_share" in vol.notes:
                note = TEXT["may_share"]
            ledger.append(
                pd.DupesLedgerVolume(
                    volume=vol.name if vol else str(v),
                    extra=led.extra,
                    reclaim=led.reclaim,
                    shared=led.shared,
                    groups=led.groups,
                    note=note,
                )
            )
        groups, extra, reclaim = h.results.totals()
        level = j.opts.load
        error = ""
        if j.state == "failed":
            error = TEXT["failed"].format(groups=groups) if groups else TEXT["failed_none"]
        stop = (
            "failed" if j.state == "failed" else (j._stop_reason if j.state in ("done", "cancelled") else "")
        )
        return pd.DupesStatus(
            job_id=h.id,
            state=j.state,
            scan_ids=h.scan_ids,
            mode=j.scope.mode,
            node=j.scope.node or None,
            min_size=j.scope.min_size,
            cross_volume=len(h.scan_ids) > 1,
            progress=j.progress(),
            funnel=pd.DupesFunnel(
                candidates=f.candidates,
                identified=f.identified,
                physical=f.physical,
                aliases=f.aliases,
                clones=f.clones,
                probed=f.probed,
                verifying=f.verifying,
                confirmed_groups=f.confirmed_groups,
                confirmed_files=f.confirmed_files,
                shared_groups=f.shared_groups,
                unique=f.unique,
                skipped=f.skipped,
            ),
            bytes=pd.DupesBytes(
                read=read,
                probe=c.bytes_probe,
                full=c.bytes_full,
                planned_probe=planned,
                full_upper=upper,
                cache_hits=c.cache_hits,
            ),
            rate=round(h.rate, 1),
            eta_max_s=round(eta, 1) if eta is not None else None,
            load=level,
            waiting_for=j.waiting_for,
            ledger=ledger,
            skipped_by_reason=dict(j.skipped_by_reason),
            atime_unkept=c.atime_unkept,
            atime_moved_by_others=c.atime_moved,
            groups=groups,
            stale_groups=h.results.stale(),
            extra=extra,
            reclaim=reclaim,
            version=h.results.version,
            error=error,
            stop_reason=stop,  # type: ignore[arg-type]
            undecided=int(j.skipped_by_reason.get("stopped", 0)),
            elapsed_s=round(j.elapsed or (time.monotonic() - j._t0 if j._t0 else 0.0), 2),
        )

    def _sessions(self) -> list[Any]:
        return [s for s in list(self.server.sessions) if CAP_DUPES in s.capabilities]

    def _on_progress(self, h: JobHandle, force: bool = False) -> None:
        now = time.monotonic()
        if not force and now - h.last_progress < PROGRESS_GAP_S:
            return
        h.last_progress = now
        status = self.status_of(h)
        for s in self._sessions():
            s.emit(EVENT_DUPES_PROGRESS, status)
        if h.results.version != h.version:
            self._changed(h, "verified")
            self._repaint(h)

    def _changed(self, h: JobHandle, reason: str, force: bool = False) -> None:
        now = time.monotonic()
        if not force and now - h.last_changed < CHANGED_GAP_S:
            if h.changed_timer is None:
                loop = asyncio.get_running_loop()
                h.changed_timer = loop.call_later(CHANGED_GAP_S, self._changed_later, h, reason)
            return
        h.last_changed = now
        h.version = h.results.version
        ev = pd.DupesChangedEvent(job_id=h.id, version=h.results.version, reason=reason)  # type: ignore[arg-type]
        for s in self._sessions():
            s.emit(EVENT_DUPES_CHANGED, ev)

    def _changed_later(self, h: JobHandle, reason: str) -> None:
        h.changed_timer = None
        self._changed(h, reason, force=True)
        self._repaint(h, force=True)

    def _repaint(self, h: JobHandle, force: bool = False) -> None:
        now = time.monotonic()
        if not force and now - h.last_repaint < REPAINT_GAP_S:
            return
        h.last_repaint = now
        for host in list(self.server.views.views.values()):
            if host.scan_id in h.scan_ids and (host.view.color_mode == "dupes" or force):
                self.server.views.push_change(host)

    # ------------------------------------------------------------------ results
    async def groups(self, p: pd.DupesGroupsParams) -> pd.DupesGroupsPage:
        h = self.get(p.job_id)
        si = h.scan_ids.index(p.scan_id) if p.scan_id in h.scan_ids else None
        under = (si, p.under) if si is not None and p.under else None

        def page() -> pd.DupesGroupsPage:
            gs = h.results.sorted_groups(
                p.sort,
                cross_only=p.cross_only,
                include_shared=p.include_shared,
                scan=si,
                under=under,
                query=p.query,
            )
            items = [self._group_model(h, g, limit=8) for g in gs[p.offset : p.offset + p.limit]]
            return pd.DupesGroupsPage(version=h.results.version, total=len(gs), items=items)

        return await compute(page)

    def group(self, p: pd.DupesGroupParams) -> pd.DupeGroup:
        h = self.get(p.job_id)
        if not 0 <= p.group < len(h.results.groups):
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["unknown"].format(id=p.group))
        return self._group_model(h, h.results.groups[p.group], offset=p.offset, limit=p.limit)

    def _group_model(self, h: JobHandle, g: Any, *, offset: int = 0, limit: int = 8) -> pd.DupeGroup:
        res = h.results
        rows = list(range(g.start, g.start + g.count))
        order = sorted(
            rows, key=lambda r: (res.role(r) not in (C.ROLE_KEEP, C.ROLE_NEEDED), res.paths[r].lower())
        )
        members = [pd.DupeMember(**res.member_dict(r, h.scan_ids)) for r in order[offset : offset + limit]]
        vols = [res.vol_names[v] if v < len(res.vol_names) else str(v) for v in g.volumes]
        return pd.DupeGroup(
            id=g.id,
            size=g.size,
            copies=g.units,
            names=g.names,
            reclaim=g.reclaim,
            shared=g.shared_bytes,
            state="shared" if g.shared else "confirmed",
            volumes=vols,
            cross=len(g.volumes) > 1,
            keep_why=C.WHY_NAMES.get(g.keep_why) if g.keep_why else None,  # type: ignore[arg-type]
            members=members,
            total_members=g.count,
        )

    async def folder(self, p: pd.DupesFolderParams) -> pd.DupesFolderInfo:
        h = self.get(p.job_id)
        if p.scan_id not in h.scan_ids:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_scan"].format(id=p.scan_id))
        si = h.scan_ids.index(p.scan_id)
        cov = await compute(h.results.coverage, si, p.node)
        groups = len(await compute(h.results.sorted_groups, scan=si, under=(si, p.node)))
        return pd.DupesFolderInfo(
            node=p.node,
            redundant=h.results.redundant(si, p.node),
            groups=groups,
            only_here=cov.only_here,
            only_here_bytes=cov.only_here_bytes,
            elsewhere=cov.elsewhere,
            elsewhere_bytes=cov.elsewhere_bytes,
            unchecked=cov.unchecked,
            unchecked_bytes=cov.unchecked_bytes,
            same_data=cov.same_data,
            same_data_bytes=cov.same_data_bytes,
            small=cov.small,
            small_bytes=cov.small_bytes,
            only_here_list=[pd.DupesOnlyHere(node=n, path=pth, alloc=a) for n, pth, a in cov.only_here_list],
            elsewhere_list=[
                pd.DupesElsewhere(scan_id=h.scan_ids[osi], node=node, path=pth, bytes=nb)
                for osi, pth, nb, node in cov.elsewhere_list
            ],
        )

    async def folders(self, p: pd.DupesFoldersParams) -> pd.DupesFoldersPage:
        h = self.get(p.job_id)
        if p.scan_id not in h.scan_ids:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_scan"].format(id=p.scan_id))
        si = h.scan_ids.index(p.scan_id)
        rows, version = await compute(_folder_rows, h.results, si)
        items = []
        for r in rows[p.offset : p.offset + p.limit]:
            twin_sid = h.scan_ids[r.twin_scan] if r.twin_scan >= 0 else ""
            items.append(
                pd.DupesFolderRow(
                    node=r.node,
                    path=r.path,
                    redundant=r.redundant,
                    alloc=r.alloc,
                    twin=r.twin,
                    twin_scan_id=twin_sid,
                    twin_node=r.twin_node,
                    twin_bytes=r.twin_bytes,
                )
            )
        return pd.DupesFoldersPage(version=version, total=len(rows), items=items)

    async def locate(self, session: Any, p: pd.DupesLocateParams) -> pd.DupesLocateResult:
        h = self.get(p.job_id)
        host = self.server.views.get(session, p.view_id)
        if host.scan_id not in h.scan_ids:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_view"])
        frame = host.view.last
        if frame is None:
            return pd.DupesLocateResult(seq=host.seq, rows=[])
        si = h.scan_ids.index(host.scan_id)
        rows, truncated = await compute(h.results.locate, si, p.group, frame.draw, p.limit)
        return pd.DupesLocateResult(
            seq=host.seq,
            rows=[
                pd.DupesLocateRow(row=r, exact=e, role=C.ROLE_NAMES.get(role, "keep"))  # type: ignore[arg-type]
                for r, e, role in rows
            ],
            truncated=truncated,
        )

    async def keep(self, p: pd.DupesKeepParams) -> pd.DupesStatus:
        """Rules as given; manual choices added to the search's (a later 「保留这一份」 in the same
        group replaces that group's earlier one; other groups keep theirs), all of them forgotten
        first with ``reset_overrides``."""
        h = self.get(p.job_id)
        res = h.results
        over: dict[tuple[bytes, str], Any] = {} if p.reset_overrides else dict(res.keep.overrides)
        for o in p.overrides:
            if 0 <= o.group < len(res.groups) and res.groups[o.group].digest is not None:
                digest = res.groups[o.group].digest or b""
                if o.role == "keep":  # 「保留这一份」 in a group replaces that group's earlier one
                    for k in [k for k, v in over.items() if k[0] == digest and v == "keep"]:
                        del over[k]
                over[(digest, o.path)] = o.role
        await res.set_keep_async(
            KeepOptions(
                rule=p.rule,
                prefer=list(p.prefer),
                folder_consistent=p.folder_consistent,
                per_volume=p.per_volume,
                overrides=over,
            )
        )
        h.keep_params = p
        self._changed(h, "keep", force=True)
        self._repaint(h, force=True)
        return self.status_of(h)

    # ------------------------------------------------------------------ views and hover cards
    def annotate(self, scan_id: str, store: Any, draw: Any) -> tuple[np.ndarray, np.ndarray] | None:
        hit = self.for_scan(scan_id)
        if hit is None:
            n = len(draw)
            return np.zeros(n, np.uint8), np.zeros(n, np.uint32)
        h, si = hit
        return h.results.annotate(si, store, draw)

    def node_info(self, scan_id: str, node: int) -> pd.DupeNodeInfo | None:
        hit = self.for_scan(scan_id)
        if hit is None:
            return None
        h, si = hit
        res = h.results
        store = res.scans[si].store
        if not 0 < node < len(store):
            return None
        from ..store.nodes import Kind

        if int(store.cols["kind"][node]) == Kind.DIR:
            red = res.redundant(si, node)
            return pd.DupeNodeInfo(state="none", redundant=red) if red else None
        row = res.member_of(si, node)
        if row < 0:
            if res.is_pending(si, node):
                return pd.DupeNodeInfo(state="pending")
            sel = h.job.sel
            if sel is not None:
                hit_c = np.flatnonzero((sel.cand["scan"] == si) & (sel.cand["node"] == node))
                if len(hit_c):
                    st = int(sel.cand["state"][hit_c[0]])
                    if st == C.ST_SKIPPED:
                        reason = C.REASON_NAMES.get(int(sel.cand["reason"][hit_c[0]]), "read_error")
                        return pd.DupeNodeInfo(state="skipped", reason=C.TEXT[reason])
                    if st == C.ST_UNIQUE:
                        return pd.DupeNodeInfo(state="unique")  # read and found different
                    return None  # undecided (a member of a group a change took apart, ...)
                uniq = sel.unique_nodes.get(si)
                if uniq is not None and len(uniq):
                    p = int(np.searchsorted(uniq, node))
                    if p < len(uniq) and int(uniq[p]) == node:
                        return pd.DupeNodeInfo(state="unique")  # no other file has its size
            return None
        g = res.groups[int(res.m["group"][row])]
        role = res.role(row)
        others = [
            pd.DupeMember(**res.member_dict(r, h.scan_ids))
            for r in range(g.start, g.start + g.count)
            if r != row
        ][:3]
        my_vol = int(res.m["vol"][row])
        other_vols = sorted(
            {
                res.vol_names[int(res.m["vol"][r])]
                for r in range(g.start, g.start + g.count)
                if int(res.m["vol"][r]) != my_vol and int(res.m["vol"][r]) < len(res.vol_names)
            }
        )
        state = (
            "shared"
            if g.shared or role in (C.ROLE_ALIAS, C.ROLE_CLONE)
            else ("extra" if role == C.ROLE_EXTRA else "keep")
        )
        return pd.DupeNodeInfo(
            state=state,  # type: ignore[arg-type]
            group=g.id,
            copies=g.units,
            role=C.ROLE_NAMES.get(role, "keep"),  # type: ignore[arg-type]
            why=C.WHY_NAMES.get(res.why(row)) if role in (C.ROLE_KEEP, C.ROLE_NEEDED) else None,  # type: ignore[arg-type]
            reclaim=g.reclaim,
            others=others,
            other_volumes=other_vols,
        )

    # ------------------------------------------------------------------ tree changes
    def on_tree_changed(self, scan_id: str, changes: dict[int, int]) -> None:
        hit = self.for_scan(scan_id)
        if hit is None or not changes:
            return
        h, si = hit
        rows = h.results.changed_nodes(si, changes.keys())
        if rows and h.results.invalidate(rows):
            self._changed(h, "invalidated")
            self._repaint(h)

    def remap(self, scan_id: str, new_ids: Callable[[np.ndarray], np.ndarray]) -> None:
        hit = self.for_scan(scan_id)
        if hit is None:
            return
        h, si = hit
        h.results.remap(si, new_ids)
        sel = h.job.sel
        if sel is not None and len(sel.cand):
            mine = sel.cand["scan"] == si
            if mine.any():
                new = np.asarray(new_ids(sel.cand["node"][mine].astype(np.int64)), dtype=np.int64)
                col = sel.cand["node"]
                col[mine] = np.maximum(new, 0).astype(np.int32)
                gone = np.flatnonzero(mine)[new < 0]
                sel.cand["state"][gone] = C.ST_SKIPPED
                sel.cand["reason"][gone] = C.GONE
            for pc in h.job._paths:
                pc.clear()

    async def on_scan_closed(self, scan_id: str) -> None:
        """A scan of a search closed (or left memory): a running search stops, its results go
        (with the tree they point into). The basket keeps the extra copies already in it."""
        for h in [h for h in self.jobs.values() if scan_id in h.scan_ids]:
            self.jobs.pop(h.id, None)
            if h.running:
                await h.job.cancel()
                if h.task is not None:
                    with contextlib.suppress(asyncio.CancelledError, Exception):
                        await asyncio.wait_for(asyncio.shield(h.task), 5)
            if h.changed_timer is not None:
                h.changed_timer.cancel()
                h.changed_timer = None
            self._changed(h, "cleared", force=True)
            logger.info("dupes {} dropped: scan {} closed", h.id, scan_id)
            for host in list(self.server.views.views.values()):
                if host.scan_id in h.scan_ids and host.scan_id != scan_id:
                    if host.view.color_mode == "dupes":
                        host.view.color_mode = "class"
                    self.server.views.push_change(host)

    def scan_closed_soon(self, scan_id: str) -> None:
        """:meth:`on_scan_closed` from a place that cannot wait (eviction)."""
        if any(scan_id in h.scan_ids for h in self.jobs.values()):
            task = asyncio.get_running_loop().create_task(self.on_scan_closed(scan_id))
            self._closing.add(task)
            task.add_done_callback(self._closing.discard)


def _stamped(res: Results, row: int) -> Stamped:
    m = res.m[row]
    g = res.groups[int(m["group"])]
    vol_dev = 0
    job = res.job
    if job is not None and int(m["vol"]) < len(job.vols):
        vol_dev = job.vols[int(m["vol"])].dev
    return Stamped(
        path=res.paths[row],
        dev=vol_dev,
        fid_lo=int(m["fid_lo"]),
        fid_hi=int(m["fid_hi"]),
        size=g.size,
        mtime=int(m["mtime"]),
        change=int(m["change"]),
        usn=int(m["usn"]),
    )


@dataclass(slots=True)
class _FolderRow:
    node: int
    path: str
    redundant: int
    alloc: int
    twin: str = ""
    twin_scan: int = -1
    twin_node: int = 0
    twin_bytes: int = 0


def _folder_rows(res: Results, si: int) -> tuple[list[_FolderRow], int]:
    """The 「文件夹」 page (docs/dupes.md §8.4): folders holding extra copies themselves, and the
    folders where two or more such branches meet when their share is at least 10 %; a chain of
    folders each holding only the one below is listed once, at its deepest. Each row names its
    twin: the folder where most of its extra copies are kept."""
    from ..store.nodes import Kind

    with res.lock:
        version = res.version
        idx = res.scan_index(si)
        n = res.n
        m = res.m[:n].copy()
        valid = res._valid_rows().copy()
        keep_rows = np.array([g.keep_row for g in res.groups], dtype=np.int64)
        paths = res.paths
    store = res.scans[si].store
    cols = store.cols
    if not len(idx.folders):
        return [], version
    extra = np.flatnonzero(
        (m["scan"] == si) & valid & ((m["role"] & 0x0F) == C.ROLE_EXTRA) & ((m["flags"] & C.F_VIEW) == 0)
    )
    parent = cols["parent"][m["node"][extra].astype(np.int64)].astype(np.int64)
    direct = set(np.unique(parent).tolist())
    # branch points: folders with two or more child folders that hold extra bytes
    folders = idx.folders
    up = cols["parent"][folders].astype(np.int64)
    kids = np.zeros(len(folders), np.int64)
    pos = np.searchsorted(folders, up)
    pos = np.minimum(pos, len(folders) - 1)
    hit = folders[pos] == up
    np.add.at(kids, pos[hit], 1)
    tot = cols["tot_alloc"][folders].astype(np.int64)
    branch = (kids >= 2) & (idx.folder_bytes >= 0.10 * np.maximum(tot, 1))
    listed = [
        i
        for i in range(len(folders))
        if (int(folders[i]) in direct or bool(branch[i]))
        and int(cols["depth"][folders[i]]) > 1
        and int(cols["kind"][folders[i]]) == Kind.DIR
    ]
    listed.sort(key=lambda i: (-int(idx.folder_bytes[i]), int(folders[i])))
    # twins of the folders listed: where each extra copy's keeper sits, by bytes
    twin_of: dict[int, dict[tuple[int, str], int]] = {}
    want = {int(folders[i]) for i in listed}
    for r, par in zip(extra.tolist(), parent.tolist(), strict=True):
        g = int(m["group"][r])
        k = int(keep_rows[g]) if g < len(keep_rows) else -1
        if k < 0:
            continue
        key = (int(m["scan"][k]), os.path.dirname(paths[k]))
        alloc = int(m["alloc"][r])
        cur = par
        for _ in range(4096):  # the folder itself and its listed ancestors
            if cur in want:
                d = twin_of.setdefault(cur, {})
                d[key] = d.get(key, 0) + alloc
            if cur <= 0:
                break
            cur = int(cols["parent"][cur])
    out: list[_FolderRow] = []
    for i in listed:
        node = int(folders[i])
        here = store.path(node)
        row = _FolderRow(node, here, int(idx.folder_bytes[i]), int(tot[i]))
        best = twin_of.get(node)
        if best:
            tsi, tpath, tb = _twin(best, si, here)
            row.twin, row.twin_scan, row.twin_bytes = tpath, tsi, tb
            with contextlib.suppress(Exception):
                found = res.scans[tsi].store.find(tpath)
                row.twin_node = int(found) if found is not None else 0
        out.append(row)
    return out, version


def _twin(keepers: dict[tuple[int, str], int], si: int, here: str) -> tuple[int, str, int]:
    """The folder a folder repeats: the deepest folder holding at least half of the keepers of its
    extra copies (a backup of six month folders repeats ``Photos\\2021``, not one month), never the
    folder itself or one above it; else the folder with the most of them."""
    total = sum(keepers.values())
    norm = os.path.normcase(here)
    by: dict[tuple[int, str], int] = {}
    for (ksi, path), nbytes in keepers.items():
        cur = path
        for _ in range(4096):
            by[(ksi, cur)] = by.get((ksi, cur), 0) + nbytes
            up = os.path.dirname(cur)
            if up == cur:
                break
            cur = up

    def mine(ksi: int, path: str) -> bool:
        p = os.path.normcase(path)
        return ksi == si and (p == norm or norm.startswith(p.rstrip("\\/") + os.sep))

    fit = [(k, b) for k, b in by.items() if 2 * b >= total and not mine(*k)]
    if fit:
        (ksi, path), nbytes = max(fit, key=lambda kv: (kv[0][1].count(os.sep), kv[1], kv[0][1]))
        return ksi, path, nbytes
    (ksi, path), nbytes = max(keepers.items(), key=lambda kv: (kv[1], kv[0][1]))
    return ksi, path, nbytes

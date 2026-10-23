"""Duplicate-file cleanup plans on the server (docs/dupes.md §16; capability ``dupes.cleanup.v1``).

One plan per finished search, built when the user opens it. The risk check runs in the background
(metadata only, paced by the disk's permits). Edits, previews, pages run in the compute pool. An
approval freezes the steps; ``execute`` takes its token, makes a run (journal under
``data_dir()/dupes/cleanup/runs``), checks every step again (preflight), then runs it in a worker
thread; one file operation at a time (I8): a run and a delete-basket execution never overlap.

At start, before any session, interrupted runs are finished from their journals (I10); the first
session hears what was found (``dupes.cleanup_interrupted``).
"""

from __future__ import annotations

import asyncio
import contextlib
import itertools
import json
import os
import sys
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any

from loguru import logger

from spaceknife_protocol import CAP_DUPES_CLEANUP
from spaceknife_protocol import dupes_cleanup as pc
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EVENT_DUPES_CLEANUP_CHANGED,
    EVENT_DUPES_CLEANUP_INTERRUPTED,
    EVENT_DUPES_CLEANUP_PROGRESS,
)

from ..dupes.cleanup import codes as K
from ..dupes.cleanup import edits as E
from ..dupes.cleanup import execute as X
from ..dupes.cleanup import persist
from ..dupes.cleanup import views as V
from ..dupes.cleanup.caps import VolCaps, volume_caps
from ..dupes.cleanup.facts import TagIndex, probe_many, system_facts
from ..dupes.cleanup.fsops import faults_from_env, make_fs, sandbox_from_env
from ..dupes.cleanup.journal import (
    Journal,
    JournalError,
    fold,
    new_run_id,
    read_plan,
    read_records,
    run_dirs,
    runs_root,
)
from ..dupes.cleanup.plan import CleanupPlan, Refused, policy_from_keep
from ..dupes.cleanup.policy import preset as make_preset
from ..i18n import Texts
from .compute import compute
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .dupes import JobHandle

PROGRESS_GAP_S = 0.25
PURGE_DELAY_S = 5.0
PROBE_BATCH = 256
KEEP_RUNS_DAYS = 90
KEEP_RUNS = 50

TEXT = Texts(
    {
        "no_plan": "没有这个清理方案：{id}",
        "running": "查找还在进行：完成后再制定清理方案",
        "snapshot": "快照是过去的状态，不能核对内容",
        "stale_version": "方案已经变了（第 {now} 版）：刷新后再改",
        "token": "批准已经失效（方案在批准之后改过）：再批准一次",
        "unapproved": "方案还没有批准",
        "busy": "还有文件操作在进行，完成后再执行",
        "no_task": "没有这个任务：{id}",
        "no_run": "没有这次清理：{id}",
        "gates": "还不能批准：{why}",
        "export_failed": "写不了 {path}：{why}",
        "import_failed": "读不了 {path}：{why}",
        "preflight": "预检：核对 {n} 项没有变过…",
        "running_now": "清理正在执行：完成后再改方案",
        "purge_wait": "{s} 秒后开始清除…",
    }
)


@dataclass(eq=False)
class PlanHandle:
    plan: CleanupPlan
    job_id: str
    roots: list[str]
    probe_task: asyncio.Task[None] | None = None
    resume: tuple[float, Any] | None = None
    last_changed: float = 0.0


@dataclass(eq=False)
class TaskHandle:
    id: str
    kind: str
    run_id: str = ""
    plan_id: str = ""
    state: str = "waiting"
    phase: str = ""
    done: int = 0
    total: int = 0
    current: str = ""
    counts: dict[str, int] = field(default_factory=dict)
    cancel: threading.Event = field(default_factory=threading.Event)
    pause: threading.Event = field(default_factory=threading.Event)
    task: asyncio.Task[None] | None = None
    last_emit: float = 0.0
    freed_now: int = 0
    freed_later: int = 0
    message: str = ""
    skipped: list[pc.CleanupSkipped] = field(default_factory=list)
    needs_confirm: bool = False


class CleanupManager:
    def __init__(self, server: Server) -> None:
        self.server = server
        self.plans: dict[str, PlanHandle] = {}
        self.tasks: dict[str, TaskHandle] = {}
        self._ids = itertools.count(1)
        self.report: pc.CleanupInterrupted | None = None
        self._told: set[int] = set()
        self.link_targets: set[str] = set()
        self._vol_cache: dict[tuple[int, str], VolCaps] = {}

    # ------------------------------------------------------------------ state
    def busy(self) -> bool:
        return any(
            t.state in ("waiting", "preflight", "running", "paused") and t.kind != "facts"
            for t in self.tasks.values()
        )

    def _sessions(self) -> list[Any]:
        return [s for s in list(self.server.sessions) if CAP_DUPES_CLEANUP in s.capabilities]

    def _get(self, plan_id: str) -> PlanHandle:
        h = self.plans.get(plan_id)
        if h is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_plan"].format(id=plan_id))
        return h

    def _setting(self, key: str, default: Any) -> Any:
        st = self.server.settings
        if st is None:
            return default
        try:
            return st.get(key)
        except KeyError:
            return default

    def attention_runs(self) -> int:
        n = 0
        for d in run_dirs():
            steps, _, _ = fold(read_records(d))
            n += sum(1 for s in steps.values() if s.result == "failed_attention" and not s.resolved)
        return n

    def runs_count(self) -> int:
        return sum(1 for _ in run_dirs())

    def _changed(self, h: PlanHandle, reason: str) -> None:
        h.last_changed = time.monotonic()
        ev = pc.CleanupChangedEvent(plan_id=h.plan.id, version=h.plan.version, reason=reason)  # type: ignore[arg-type]
        for s in self._sessions():
            s.emit(EVENT_DUPES_CLEANUP_CHANGED, ev)

    def _emit(self, t: TaskHandle, *, force: bool = False) -> None:
        now = time.monotonic()
        if not force and now - t.last_emit < PROGRESS_GAP_S:
            return
        t.last_emit = now
        ev = pc.CleanupProgressEvent(
            task_id=t.id,
            kind=t.kind,  # type: ignore[arg-type]
            phase=t.phase,
            state=t.state,  # type: ignore[arg-type]
            done=t.done,
            total=t.total,
            current=t.current,
            counts=dict(t.counts),
            freed_now=t.freed_now,
            freed_later=t.freed_later,
            run_id=t.run_id,
            plan_id=t.plan_id,
            message=t.message,
            skipped=list(t.skipped),
            needs_confirm=t.needs_confirm,
        )
        for s in self._sessions():
            s.emit(EVENT_DUPES_CLEANUP_PROGRESS, ev)

    # ------------------------------------------------------------------ start
    async def recover_all(self) -> None:
        """Finish interrupted runs (before any session; I10)."""
        dirs = [d for d in run_dirs() if X.interrupted(d)]
        if dirs:
            for d in dirs:
                try:
                    counts = await asyncio.to_thread(self._recover_one, d)
                except Exception:
                    logger.exception("finishing the interrupted cleanup {} failed", d.name)
                    continue
                self.report = pc.CleanupInterrupted(
                    run_id=d.name,
                    at_seq=counts.get("at", 0),
                    restored=counts.get("restored", 0) + counts.get("cancelled", 0),
                    done=counts.get("done", 0),
                    attention=counts.get("attention", 0),
                    message=X.TEXT["interrupted"].format(
                        seq=counts.get("at", 0),
                        restored=counts.get("restored", 0) + counts.get("cancelled", 0),
                        done=counts.get("done", 0),
                        attention=counts.get("attention", 0),
                    ),
                )
                logger.info("cleanup {} finished after an interruption: {}", d.name, counts)
        await asyncio.to_thread(self._load_link_targets)
        await asyncio.to_thread(self._prune_runs)

    def _ctx_for(self, run_id: str, plan: dict[str, Any]) -> X.RunCtx:
        from ..paths import data_dir

        sandbox = sandbox_from_env()
        if sandbox is None and plan.get("sandbox"):
            from ..dupes.cleanup.fsops import Sandbox

            sandbox = Sandbox(list(plan["sandbox"]))
        return X.RunCtx(
            run_id=run_id,
            steps=list(plan.get("steps", [])),
            fs=make_fs(sandbox, faults_from_env()),
            options=dict(plan.get("options", {})),
            data_dir=data_dir(),
            ops_kind=str(plan.get("ops", "native")),
            at_root=bool(self._setting("dupes.cleanup.recovery_at_root", True)),
            no_trash_recovery=str(self._setting("dupes.cleanup.no_trash", "recovery")) == "recovery",
        )

    def _recover_one(self, d: Path) -> dict[str, int]:
        plan = read_plan(d)
        ctx = self._ctx_for(d.name, plan)
        return X.recover(ctx, d)

    def _load_link_targets(self) -> None:
        targets: set[str] = set()
        for d in run_dirs():
            plan = read_plan(d)
            by_seq = {int(s["seq"]): s for s in plan.get("steps", [])}
            steps, _, _ = fold(read_records(d))
            for seq, st in steps.items():
                s = by_seq.get(seq)
                if (
                    s
                    and s["action"] == "symlink"
                    and st.result in ("done", "done_partial")
                    and not st.undone
                    and s.get("target")
                ):
                    targets.add(os.path.normcase(s["target"]["path"]))
        self.link_targets = targets

    def link_target_count(self, path: str) -> int:
        """SpaceKnife-made symbolic links pointing at ``path`` or inside it (the delete basket asks)."""
        if not self.link_targets:
            return 0
        key = os.path.normcase(path).rstrip("\\/")
        return sum(1 for t in self.link_targets if t == key or t.startswith(key + os.sep))

    def _prune_runs(self) -> None:
        """Runs with nothing left in a recovery area: older than 90 days and beyond the newest 50 go."""
        import shutil

        cutoff = time.time() - KEEP_RUNS_DAYS * 86400
        for i, d in enumerate(run_dirs()):
            if i < KEEP_RUNS:
                continue
            try:
                if d.stat().st_mtime > cutoff:
                    continue
            except OSError:
                continue
            steps, _, end = fold(read_records(d))
            held = any(
                st.done and st.done.get("rec") and not st.undone and not st.purged for st in steps.values()
            )
            if end is not None and not held:
                with contextlib.suppress(OSError):
                    shutil.rmtree(d)

    def recover_report(self, session: Any) -> pc.CleanupRecoverReport:
        return pc.CleanupRecoverReport(report=self.report)

    def hello(self, session: Any) -> None:
        """The first session after a recovery hears about it (once)."""
        if (
            self.report is not None
            and id(self.report) not in self._told
            and CAP_DUPES_CLEANUP in session.capabilities
        ):
            self._told.add(id(self.report))
            session.emit(EVENT_DUPES_CLEANUP_INTERRUPTED, self.report)

    # ------------------------------------------------------------------ plans
    def _vols(self, job: Any) -> list[VolCaps]:
        from ..ops.trash import trash_status

        out: list[VolCaps] = []
        sysf = system_facts()
        for v in job.vols:
            key = (v.dev, v.mount)
            hit = self._vol_cache.get(key)
            if hit is None:
                hit = volume_caps(v, sys_facts=sysf, trash_of=lambda m: trash_status(m, usage=False))
                with contextlib.suppress(Exception):
                    reg = self.server.scans.io
                    if reg.running:
                        pass
                self._vol_cache[key] = hit
            out.append(hit)
        return out

    def plan_of_job(self, job_id: str) -> PlanHandle | None:
        for h in self.plans.values():
            if h.job_id == job_id:
                return h
        return None

    async def open(self, p: pc.CleanupOpenParams) -> pc.CleanupState:
        dm = self.server.dupes
        jh: JobHandle = dm.get(p.job_id)
        if jh.running:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["running"])
        hit = self.plan_of_job(p.job_id)
        if hit is not None:
            if p.preset and p.preset != hit.plan.policy.preset:
                await self._edit(
                    hit, pc.CleanupEdit(op="preset", preset=p.preset, backup_volume=p.backup_volume)
                )
            return self.state_of(hit)
        roots = [r.path for s in jh.job.scope.scans for r in s.roots]
        vols = await asyncio.to_thread(self._vols, jh.job)
        sysf = system_facts(
            macos_links=bool(self._setting("dupes.cleanup.macos_links", False)),
            native=jh.job.ops.name == "native",
        )
        raw = str(self._setting("dupes.keep_criteria", "oldest_created,plain_name,shortest_path"))
        crit = [c.strip() for c in raw.split(",") if c.strip()]
        base = policy_from_keep(jh.results.keep, criteria=crit[1:] if crit else None)
        name = p.preset or str(self._setting("dupes.cleanup.preset", "recommended"))
        policy = make_preset(name, base, backup_volume=p.backup_volume)
        policy.options.on_failure = str(self._setting("dupes.cleanup.on_failure", "continue"))  # type: ignore[assignment]
        policy.options.verify = str(self._setting("dupes.cleanup.verify", "auto"))  # type: ignore[assignment]
        policy.options.symlink_style = str(self._setting("dupes.cleanup.symlink_style", "auto"))  # type: ignore[assignment]
        stores = [s.store for s in jh.results.scans]
        tags = TagIndex(stores)
        tags.link_targets = set(self.link_targets)
        basket = self.server.ops.basket
        plan_id = f"plan-{next(self._ids)}"
        at_root = bool(self._setting("dupes.cleanup.recovery_at_root", True))

        def in_place(v: int) -> bool:
            return not at_root and sandbox_from_env() is None and v < len(vols) and vols[v].mount != ""

        plan = await compute(
            lambda: CleanupPlan(
                plan_id,
                jh.results,
                jh.scan_ids,
                policy,
                vols=vols,
                sys_facts=sysf,
                tags=tags,
                in_basket=lambda path: basket.contains(path) is not None,
                recovery_in_place=in_place,
                no_trash_recovery=str(self._setting("dupes.cleanup.no_trash", "recovery")) == "recovery",
            )
        )
        h = PlanHandle(plan, p.job_id, roots)
        saved = await asyncio.to_thread(persist.load, roots)
        if saved is not None and (
            saved[1].manual or saved[1].groups or saved[1].keep_rules or saved[1].action_rules
        ):
            h.resume = saved
        self.plans[plan_id] = h
        h.probe_task = asyncio.create_task(self._probe_loop(h), name=f"{plan_id}-facts")
        logger.info("cleanup plan {} opened for {} ({} groups)", plan_id, p.job_id, len(plan.groups))
        return self.state_of(h)

    async def _probe_loop(self, h: PlanHandle) -> None:
        """The risk check: one metadata look per copy, in batches, paced by the disk's permits."""
        plan = h.plan
        try:
            while not plan.closed:
                with plan.lock:
                    todo = plan.probe_order()[:PROBE_BATCH]
                if not todo:
                    break
                paths = [plan.paths[r] for r in todo]
                permit = None
                with contextlib.suppress(Exception):
                    reg = self.server.scans.io
                    if reg.running:
                        gov = await reg.governor_for_path(paths[0])
                        permit = await gov.permit()
                try:
                    got = await asyncio.to_thread(probe_many, paths)
                finally:
                    if permit is not None:
                        permit.release()
                with plan.lock:
                    for r, pr in zip(todo, got, strict=True):
                        plan.probes[r] = pr
                done, total = plan.facts_progress()
                if done >= total or len(plan.probes) % (PROBE_BATCH * 8) < PROBE_BATCH:
                    await compute(plan.recompute)
                    self._changed(h, "facts")
            await compute(plan.recompute)
            self._changed(h, "facts")
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.exception("risk check of {} failed", plan.id)

    def _gates(self, h: PlanHandle) -> list[str]:
        dm = self.server.dupes
        jh = dm.jobs.get(h.job_id)
        scanning = any(s.running for s in self.server.scans.scans.values() if s.id in h.plan.scan_ids)
        return h.plan.gates(
            busy=self.server.ops.busy(),
            job_running=bool(jh and jh.running),
            scanning=scanning,
            attention=self.attention_runs(),
        )

    def state_of(self, h: PlanHandle) -> pc.CleanupState:
        plan = h.plan
        with plan.lock:
            P = plan.policy
            done, total = plan.facts_progress()
            gates = self._gates(h)
            appr = plan.approval
            running = next(
                (
                    t.id
                    for t in self.tasks.values()
                    if t.plan_id == plan.id and t.state in ("waiting", "preflight", "running", "paused")
                ),
                "",
            )
            return pc.CleanupState(
                plan_id=plan.id,
                job_id=h.job_id,
                scan_ids=plan.scan_ids,
                version=plan.version,
                preset=P.preset,
                criteria=list(P.criteria),
                per_volume=P.per_volume,
                folder_consistent=P.folder_consistent,
                default_action=P.default_action,
                options={
                    "on_failure": P.options.on_failure,
                    "verify": P.options.verify,
                    "symlink_style": P.options.symlink_style,
                },
                keep_rules=[V.rule_view(plan, r) for r in P.keep_rules],
                action_rules=[V.rule_view(plan, r) for r in P.action_rules],
                pairs=[
                    pc.CleanupPairView(a=x.a, b=x.b, keep=x.keep, other_action=x.other_action, auto=x.auto)
                    for x in P.pairs
                ],
                auto_pairs=len({k for k in (plan._side or {})}) // 2 if plan._side else 0,
                manual=P.manual_count(),
                summary=V.summary(plan, gates, recovery=self._recovery_where(plan)),
                facts_done=done,
                facts_total=total,
                approval=(
                    pc.CleanupApprovalInfo(
                        token=appr.token,
                        version=appr.version,
                        steps=len(appr.steps),
                        by_action={
                            a: sum(1 for s in appr.steps if s.action == a)
                            for a in {s.action for s in appr.steps}
                        },
                        acks=dict(appr.acks),
                        at=datetime.fromtimestamp(appr.at, tz=UTC).isoformat(),
                    )
                    if appr is not None and appr.version == plan.version
                    else None
                ),
                can_undo=bool(plan.undo_stack),
                can_redo=bool(plan.redo_stack),
                resume_offer=(
                    pc.CleanupResumeOffer(
                        saved_at=datetime.fromtimestamp(h.resume[0], tz=UTC).isoformat(),
                        manual=h.resume[1].manual_count(),
                        rules=len(h.resume[1].keep_rules) + len(h.resume[1].action_rules),
                    )
                    if h.resume is not None
                    else None
                ),
                platform=sys.platform,
                symlink_right=plan.sysf.symlink_right,
                macos_links=plan.sysf.macos_links,
                running=running,
            )

    def _recovery_where(self, plan: CleanupPlan) -> dict[int, str]:
        from ..paths import data_dir

        out: dict[int, str] = {}
        ddev = None
        with contextlib.suppress(OSError):
            ddev = os.stat(str(data_dir().parent if not data_dir().exists() else data_dir())).st_dev
        at_root = bool(self._setting("dupes.cleanup.recovery_at_root", True))
        for vc in plan.vols:
            if ddev is not None and sys.platform != "win32" and ddev == vc.dev:
                out[vc.index] = str(data_dir() / "dupes" / "cleanup" / "recovery")
            elif at_root:
                out[vc.index] = os.path.join(vc.mount, ".SpaceKnife-recovery")
            else:
                out[vc.index] = K.TEXT["relative"]
        return out

    async def resume(self, p: pc.CleanupResumeParams) -> pc.CleanupState:
        h = self._get(p.plan_id)
        if p.accept and h.resume is not None:
            plan = h.plan
            saved = h.resume[1]

            def take() -> None:
                with plan.lock:
                    before = plan.snapshot()
                    plan.policy = saved.clone()
                    plan.commit(before)

            await compute(take)
        h.resume = None
        if not p.accept:
            await asyncio.to_thread(persist.drop, h.roots)
        self._changed(h, "edit")
        return self.state_of(h)

    def _check_version(self, h: PlanHandle, base: int) -> None:
        if base != h.plan.version:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["stale_version"].format(now=h.plan.version))

    def _not_running(self, h: PlanHandle) -> None:
        if any(
            t.plan_id == h.plan.id and t.state in ("waiting", "preflight", "running", "paused")
            for t in self.tasks.values()
        ):
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["running_now"])

    async def _edit(self, h: PlanHandle, e: pc.CleanupEdit) -> None:
        try:
            await compute(E.edit, h.plan, e)
        except Refused as exc:
            raise RpcError(ErrorCode.INVALID_REQUEST, str(exc)) from exc
        await asyncio.to_thread(persist.save, h.roots, h.plan.policy)
        self._changed(h, "edit")

    async def edit(self, p: pc.CleanupEditParams) -> pc.CleanupState:
        h = self._get(p.plan_id)
        self._not_running(h)
        self._check_version(h, p.base_version)
        await self._edit(h, p.edit)
        return self.state_of(h)

    async def preview(self, p: pc.CleanupEditParams) -> pc.CleanupDiff:
        h = self._get(p.plan_id)
        self._check_version(h, p.base_version)
        d = await compute(E.preview, h.plan, p.edit)
        return pc.CleanupDiff(
            version=h.plan.version,
            before=d["before"],
            after=d["after"],
            freed_before=d["freed_before"],
            freed_after=d["freed_after"],
            unavailable=d["unavailable"],
            risks_added=d["risks_added"],
            risks_removed=d["risks_removed"],
            protected=d["protected"],
            changed=d["changed"],
            examples=[pc.CleanupDiffRow(**x) for x in d["examples"]],
            refused=d["refused"],
        )

    async def undo_edit(self, p: pc.CleanupVersionParams, *, redo: bool = False) -> pc.CleanupState:
        h = self._get(p.plan_id)
        self._not_running(h)
        self._check_version(h, p.base_version)
        await compute(h.plan.redo if redo else h.plan.undo)
        await asyncio.to_thread(persist.save, h.roots, h.plan.policy)
        self._changed(h, "edit")
        return self.state_of(h)

    async def groups(self, p: pc.CleanupGroupsParams) -> pc.CleanupGroupsPage:
        h = self._get(p.plan_id)
        with_sync = h.plan.sync_results
        await compute(with_sync)
        return await compute(V.groups_page, h.plan, p, h.plan.scan_ids)

    async def group(self, p: pc.CleanupGroupParams) -> pc.CleanupGroupDetail:
        h = self._get(p.plan_id)
        if not 0 <= p.group < len(h.plan.groups):
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_plan"].format(id=p.group))
        return await compute(V.group_detail, h.plan, p.group, h.plan.scan_ids)

    async def facets(self, p: pc.CleanupFilterParams) -> pc.CleanupFacets:
        h = self._get(p.plan_id)
        return await compute(V.facets, h.plan, p.filter)

    async def pairs(self, p: pc.CleanupPairsParams) -> pc.CleanupPairsPage:
        h = self._get(p.plan_id)
        return await compute(V.pairs_page, h.plan, p)

    async def volumes(self, p: pc.CleanupPlanParams) -> pc.CleanupVolumeMatrix:
        h = self._get(p.plan_id)
        return await compute(V.volume_matrix, h.plan)

    async def summary(self, p: pc.CleanupPlanParams) -> pc.CleanupSummary:
        h = self._get(p.plan_id)
        gates = self._gates(h)
        return await compute(lambda: V.summary(h.plan, gates, recovery=self._recovery_where(h.plan)))

    async def risk_copies(self, p: pc.CleanupRiskCopiesParams) -> pc.CleanupRiskCopies:
        h = self._get(p.plan_id)
        return await compute(V.risk_copies, h.plan, p.risk, p.offset, p.limit, h.plan.scan_ids)

    async def sample(self, p: pc.CleanupSampleParams) -> pc.CleanupSample:
        h = self._get(p.plan_id)
        return pc.CleanupSample(groups=await compute(V.sample, h.plan, p.per_bucket))

    async def who_uses(self, p: pc.CleanupFilterParams) -> pc.CleanupWhoUses:
        from ..dupes.cleanup.query import evaluate
        from ..ops.locks import find_lockers

        h = self._get(p.plan_id)
        _, rows = await compute(evaluate, h.plan, p.filter)
        import numpy as np

        paths = [h.plan.paths[int(r)] for r in np.flatnonzero(rows & h.plan.is_rep)[:2000].tolist()]
        return pc.CleanupWhoUses(users=await asyncio.to_thread(find_lockers, paths))

    async def export(self, p: pc.CleanupExportParams) -> pc.CleanupExportResult:
        def write() -> int:
            if p.run_id:
                d = runs_root() / p.run_id
                detail = self._run_detail_sync(d, "", 0, 10**9)
                if p.format == "csv":
                    import csv
                    import io

                    buf = io.StringIO()
                    w = csv.writer(buf)
                    w.writerow(("seq", "action", "path", "target", "code", "state", "recovery", "freed"))
                    for it in detail.items:
                        w.writerow(
                            (it.seq, it.action, it.path, it.target, it.code, it.state, it.recovery, it.freed)
                        )
                    text = buf.getvalue()
                else:
                    text = detail.model_dump_json(indent=1)
            else:
                h = self._get(p.plan_id)
                if p.format == "csv":
                    text = V.export_csv(h.plan)
                else:
                    text = json.dumps(V.export_json(h.plan, h.plan.scan_ids), ensure_ascii=False, indent=1)
            data = text.encode("utf-8", "surrogatepass")
            with open(p.path, "wb") as f:
                f.write(data)
            return len(data)

        try:
            n = await compute(write)
        except OSError as exc:
            raise RpcError(
                ErrorCode.INVALID_REQUEST, TEXT["export_failed"].format(path=p.path, why=exc.strerror or exc)
            ) from exc
        return pc.CleanupExportResult(path=p.path, bytes=n)

    async def import_(self, p: pc.CleanupImportParams) -> pc.CleanupImportResult:
        h = self._get(p.plan_id)
        self._not_running(h)

        def read() -> Any:
            with open(p.path, encoding="utf-8", errors="surrogatepass") as f:
                return json.load(f)

        try:
            data = await asyncio.to_thread(read)
            applied, unmatched, pol = await compute(V.import_json, h.plan, data)
        except (OSError, ValueError) as exc:
            raise RpcError(
                ErrorCode.INVALID_REQUEST, TEXT["import_failed"].format(path=p.path, why=exc)
            ) from exc
        plan = h.plan

        def take() -> None:
            with plan.lock:
                before = plan.snapshot()
                plan.policy = pol
                plan.commit(before)

        await compute(take)
        self._changed(h, "edit")
        return pc.CleanupImportResult(applied=applied, unmatched=unmatched[:500], state=self.state_of(h))

    async def close(self, p: pc.CleanupPlanParams) -> None:
        h = self.plans.get(p.plan_id)
        if h is None:
            return
        await asyncio.to_thread(persist.save, h.roots, h.plan.policy)
        self.drop(p.plan_id)

    def drop(self, plan_id: str) -> None:
        h = self.plans.pop(plan_id, None)
        if h is None:
            return
        h.plan.closed = True
        if h.probe_task is not None:
            h.probe_task.cancel()
        self._changed(h, "closed")

    def drop_job(self, job_id: str) -> None:
        """A search went (replaced, cleared, its scan closed): its plan draft goes too, runs stay."""
        for pid in [pid for pid, h in self.plans.items() if h.job_id == job_id]:
            h = self.plans[pid]
            with contextlib.suppress(Exception):
                persist.save(h.roots, h.plan.policy)
            self.drop(pid)

    def on_results(self, job_id: str) -> None:
        h = self.plan_of_job(job_id)
        if h is None:
            return

        async def sync() -> None:
            if await compute(h.plan.sync_results):
                self._changed(h, "results")

        with contextlib.suppress(RuntimeError):
            asyncio.get_running_loop().create_task(sync())

    def on_basket(self) -> None:
        for h in self.plans.values():
            self._changed(h, "basket")

    # ------------------------------------------------------------------ approval and running
    async def approve(self, p: pc.CleanupApproveParams) -> pc.CleanupApprovalInfo:
        h = self._get(p.plan_id)
        self._check_version(h, p.version)
        gates = [g for g in self._gates(h) if g != K.TEXT["gate_busy"]]
        if gates:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["gates"].format(why=K.TEXT["semi"].join(gates)))

        def through(folder: str) -> bool:
            try:
                return os.path.normcase(os.path.realpath(folder)) != os.path.normcase(os.path.abspath(folder))
            except OSError:
                return True

        appr = await compute(lambda: h.plan.approve(through_link=through))
        # the folder of every step, by identity, as approved (R1 checks it again)
        await asyncio.to_thread(self._parent_ids, appr)
        self._changed(h, "approved")
        st = self.state_of(h)
        assert st.approval is not None
        return st.approval

    def _parent_ids(self, appr: Any) -> None:
        fs = make_fs(sandbox_from_env())
        seen: dict[str, tuple[int, int, int] | None] = {}
        for s in appr.steps:
            if s.parent not in seen:
                ident = None
                with contextlib.suppress(Exception):
                    dh = fs.open_dir(s.parent)
                    try:
                        ident = fs.fstat(dh).ident
                    finally:
                        fs.close(dh)
                seen[s.parent] = ident
            ident = seen[s.parent]
            s.parent_id = list(ident) if ident is not None else None

    def _approved(self, h: PlanHandle, token: str) -> Any:
        appr = h.plan.approval
        if appr is None:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["unapproved"])
        if appr.token != token or appr.version != h.plan.version:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["token"])
        return appr

    def _steps_json(self, appr: Any) -> list[dict[str, Any]]:
        out = []
        for s in appr.steps:
            out.append(s.to_json())
        return out

    def _roots_of(self, h: PlanHandle, steps: list[dict[str, Any]]) -> dict[str, str]:
        res = h.plan.res
        out: dict[str, str] = {}
        for s in steps:
            si = int(s["scan"])
            if si < len(res.scans):
                for r in res.scans[si].roots:
                    key = os.path.normcase(r.path)
                    if os.path.normcase(s["path"]).startswith(key):
                        out[s["path"]] = r.path
                        break
        return out

    def _new_task(self, kind: str, plan_id: str = "", run_id: str = "") -> TaskHandle:
        t = TaskHandle(f"cleanup-{next(self._ids)}", kind, run_id=run_id, plan_id=plan_id)
        self.tasks[t.id] = t
        for old in [
            k
            for k, v in self.tasks.items()
            if v.state in ("done", "failed", "stopped", "partial", "undone", "recovered")
        ][:-16]:
            self.tasks.pop(old, None)
        return t

    def _loop_progress(
        self, t: TaskHandle, loop: asyncio.AbstractEventLoop
    ) -> Callable[[str, int, int, str, dict[str, int]], None]:
        def progress(phase: str, done: int, total: int, current: str, counts: dict[str, int]) -> None:
            def apply() -> None:
                t.phase, t.done, t.total, t.current = phase, done, total, current
                t.counts = counts or t.counts
                self._emit(t)

            loop.call_soon_threadsafe(apply)

        return progress

    def _permit(self, loop: asyncio.AbstractEventLoop) -> Callable[[str], None]:
        held: list[Any] = []

        def permit(path: str) -> None:
            for p in held:
                with contextlib.suppress(Exception):
                    p.release()
            held.clear()
            reg = self.server.scans.io
            if not reg.running:
                return

            async def ask() -> Any:
                gov = await reg.governor_for_path(path)
                return await gov.permit()

            with contextlib.suppress(Exception):
                held.append(asyncio.run_coroutine_threadsafe(ask(), loop).result(timeout=60))

        return permit

    async def preflight(self, p: pc.CleanupTokenParams) -> pc.CleanupTask:
        h = self._get(p.plan_id)
        appr = self._approved(h, p.token)
        steps = self._steps_json(appr)
        t = self._new_task("preflight", plan_id=h.plan.id)
        t.total = len(steps)
        t.state = "preflight"
        loop = asyncio.get_running_loop()

        async def go() -> None:
            ctx = self._ctx(h, "", steps, appr.options)
            try:
                skip = await asyncio.to_thread(X.preflight, ctx, steps)
                t.skipped = _skipped(skip, steps)
                t.done = len(steps)
                t.counts = {
                    "would_do": len(steps) - len(skip),
                    **{c: sum(1 for v in skip.values() if v[0] == c) for c in {v[0] for v in skip.values()}},
                }
                t.state = "done"
            except Exception:
                logger.exception("cleanup preflight failed")
                t.state = "failed"
            self._emit(t, force=True)

        t.task = loop.create_task(go())
        return pc.CleanupTask(task_id=t.id, kind="preflight", total=t.total)

    def _ctx(
        self, h: PlanHandle, run_id: str, steps: list[dict[str, Any]], options: dict[str, str]
    ) -> X.RunCtx:
        from ..paths import data_dir

        jh = self.server.dupes.jobs.get(h.job_id)
        return X.RunCtx(
            run_id=run_id,
            steps=steps,
            fs=make_fs(sandbox_from_env(), faults_from_env()),
            options=dict(options),
            data_dir=data_dir(),
            ops_kind=jh.job.ops.name if jh is not None else "native",
            keep_atime=jh.job.opts.keep_atime if jh is not None else "skip",
            at_root=bool(self._setting("dupes.cleanup.recovery_at_root", True)),
            no_trash_recovery=str(self._setting("dupes.cleanup.no_trash", "recovery")) == "recovery",
            check_locks=True,
            elsewhere=self.server.ops.basket_elsewhere(),
            roots=self._roots_of(h, steps),
            lockers=_lockers,
        )

    async def execute(self, session: Any, p: pc.CleanupExecuteParams) -> pc.CleanupTask:
        h = self._get(p.plan_id)
        appr = self._approved(h, p.token)
        if self.server.ops.busy():
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["busy"])
        gates = [g for g in self._gates(h) if g != K.TEXT["gate_busy"]]
        if gates:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["gates"].format(why=K.TEXT["semi"].join(gates)))
        options = dict(appr.options)
        if p.on_failure:
            options["on_failure"] = p.on_failure
        if p.verify:
            options["verify"] = p.verify
        steps = self._steps_json(appr)
        run_id = new_run_id()
        t = self._new_task("execute", plan_id=h.plan.id, run_id=run_id)
        t.total = len(steps)
        t.state = "preflight"
        t.phase = "preflight"
        t.message = TEXT["preflight"].format(n=len(steps))
        loop = asyncio.get_running_loop()
        ctx = self._ctx(h, run_id, steps, options)
        ctx.progress = self._loop_progress(t, loop)
        ctx.cancel, ctx.pause = t.cancel, t.pause
        ctx.permit = self._permit(loop)
        jh = self.server.dupes.jobs.get(h.job_id)

        def on_batch(pairs: list[tuple[dict[str, Any], X.Outcome]]) -> None:
            items = []
            for s, o in pairs:
                if o.code not in ("done", "done_partial"):
                    continue
                row = _row_of(h.plan, s)
                out: dict[str, Any] = {"keeper_after": o.keeper_after, "copy_after": o.copy_after}
                if s.get("target"):
                    out["keeper_row"] = int(s["target"].get("row", -1))
                items.append((row, s["action"], out))
                t.freed_now += o.freed_now
                t.freed_later += o.freed_later
            if not items:
                return

            def apply() -> None:
                if jh is not None:
                    paths = [s["path"] for s, _ in pairs] + [
                        s["target"]["path"] for s, _ in pairs if s.get("target")
                    ]
                    self.server.dupes.expect(jh.id, paths)
                    jh.results.apply_cleanup(items)

            loop.call_soon_threadsafe(apply)

        ctx.on_batch = on_batch

        async def go() -> None:
            try:
                plan_doc = {
                    "run": run_id,
                    "plan": h.plan.id,
                    "token": appr.token,
                    "steps": steps,
                    "acks": dict(appr.acks),
                    "options": options,
                    "ops": ctx.ops_kind,
                    "roots": h.roots,
                    "sandbox": ctx.fs.sandbox.roots if ctx.fs.sandbox else None,
                    "started": datetime.now(UTC).isoformat(),
                }
                try:
                    ctx.journal = await asyncio.to_thread(Journal.create, run_id, plan_doc)
                except JournalError as exc:
                    t.state = "failed"
                    t.message = K.TEXT["gate_journal"].format(path=str(runs_root()))
                    logger.warning("cleanup journal: {}", exc)
                    return
                ctx.journal.write(
                    {
                        "t": "run",
                        "v": 1,
                        "run": run_id,
                        "token": appr.token,
                        "os": sys.platform,
                        "policy": options,
                        "acks": dict(appr.acks),
                    },
                    sync=True,
                )
                skip = await asyncio.to_thread(X.preflight, ctx, steps)
                if skip and not p.proceed_after_drops:
                    t.skipped = _skipped(skip, steps)
                    t.needs_confirm = True
                    t.state = "waiting"
                    t.phase = "confirm"
                    self._emit(t, force=True)
                    ctx.journal.write(
                        {"t": "end", "state": "stopped", "counts": {}, "stop": "confirm"}, sync=True
                    )
                    ctx.journal.close()
                    t.state = "stopped"
                    return
                t.state = "running"
                t.phase = "execute"
                t.message = ""
                self._emit(t, force=True)
                report = await asyncio.to_thread(X.run, ctx, skip=skip)
                t.state = str(report.get("state", "done"))
                t.counts = dict(report.get("counts", {}))
                t.skipped = _skipped(
                    {o.seq: (o.code, o.why) for o in ctx.outcomes.values() if not o.code.startswith("done")},
                    steps,
                )
                t.message = X.TEXT[ctx.stop_reason] if ctx.stop_reason else ""
                t.done = t.total
            except Exception:
                logger.exception("cleanup run {} failed", run_id)
                t.state = "failed"
            finally:
                if ctx.journal is not None:
                    ctx.journal.close()
                self._emit(t, force=True)
                await self._after_run(h, steps)

        t.task = loop.create_task(go())
        logger.info("cleanup run {}: {} steps", run_id, len(steps))
        return pc.CleanupTask(task_id=t.id, kind="execute", run_id=run_id, total=t.total)

    async def _after_run(self, h: PlanHandle, steps: list[dict[str, Any]]) -> None:
        """The plan is built again from the results (the approval is gone: nothing runs twice); the
        tree follows (monitored scans see it, the others read the folders again)."""
        await asyncio.to_thread(self._load_link_targets)
        dm = self.server.dupes
        jh = dm.jobs.get(h.job_id)
        if jh is not None and h.plan.id in self.plans:
            old = h.plan
            tags = old.tagx
            tags.link_targets = set(self.link_targets)
            try:
                plan = await compute(
                    lambda: CleanupPlan(
                        old.id,
                        jh.results,
                        jh.scan_ids,
                        old.policy,
                        vols=old.vols,
                        sys_facts=old.sysf,
                        tags=tags,
                        in_basket=old.in_basket,
                        recovery_in_place=old.recovery_in_place,
                        no_trash_recovery=old.no_trash_recovery,
                    )
                )
                plan.version = old.version + 1
                plan.probes = dict(old.probes)
                h.plan = plan
                if h.probe_task is not None:
                    h.probe_task.cancel()
                h.probe_task = asyncio.create_task(self._probe_loop(h))
            except Exception:
                logger.exception("rebuilding the cleanup plan failed")
            self._changed(h, "run")
            dm._changed(jh, "verified", force=True)
            dm._repaint(jh, force=True)
        # the tree: monitored scans see the changes; the others read the parents again
        by_scan: dict[str, set[str]] = {}
        for s in steps:
            si = int(s["scan"])
            if si < len(h.plan.scan_ids):
                by_scan.setdefault(h.plan.scan_ids[si], set()).add(s["parent"])
        for scan_id, parents in by_scan.items():
            handle = self.server.scans.scans.get(scan_id)
            if handle is None or handle.running or handle.monitor is not None:
                continue
            for parent in sorted(parents):
                node = handle.store.find(parent)
                if node is not None:
                    with contextlib.suppress(Exception):
                        await self.server.monitors.rescan(handle, node)
        with contextlib.suppress(Exception):
            await self.server.ops.broadcast()

    def _task(self, task_id: str) -> TaskHandle:
        t = self.tasks.get(task_id)
        if t is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_task"].format(id=task_id))
        return t

    def _task_model(self, t: TaskHandle) -> pc.CleanupTask:
        return pc.CleanupTask(task_id=t.id, kind=t.kind, run_id=t.run_id, total=t.total)  # type: ignore[arg-type]

    def pause(self, p: pc.CleanupTaskParams) -> pc.CleanupTask:
        t = self._task(p.task_id)
        if t.state == "running":
            t.pause.set()
            t.state = "paused"
            self._emit(t, force=True)
        return self._task_model(t)

    def resume_run(self, p: pc.CleanupTaskParams) -> pc.CleanupTask:
        t = self._task(p.task_id)
        if t.state == "paused":
            t.pause.clear()
            t.state = "running"
            self._emit(t, force=True)
        return self._task_model(t)

    def cancel(self, p: pc.CleanupTaskParams) -> pc.CleanupTask:
        t = self._task(p.task_id)
        t.cancel.set()
        t.pause.clear()
        if t.kind == "purge" and t.state == "waiting" and t.task is not None:
            t.task.cancel()
        return self._task_model(t)

    # ------------------------------------------------------------------ history
    def _summary(self, d: Path) -> pc.CleanupRunSummary:
        plan = read_plan(d)
        steps, run, end = fold(read_records(d))
        by_seq = {int(s["seq"]): s for s in plan.get("steps", [])}
        by_action: dict[str, int] = {}
        counts: dict[str, int] = {}
        held = 0
        held_by: dict[str, int] = {}
        in_trash = 0
        can_undo = 0
        can_purge = 0
        attention = 0
        freed_now = 0
        undone = 0
        for seq, st in steps.items():
            s = by_seq.get(seq)
            if s is None:
                continue
            code = st.result or ("pending" if st.intent else "")
            if code:
                counts[code] = counts.get(code, 0) + 1
            if st.result in ("done", "done_partial"):
                by_action[s["action"]] = by_action.get(s["action"], 0) + 1
                if st.undone:
                    undone += 1
                    continue
                done = st.done or {}
                freed_now += int(done.get("freed_now", 0) or 0)
                if s["action"] != "clone" and not st.purged:
                    can_undo += 1
                if done.get("rec") and not st.purged:
                    held += int(s.get("alloc", 0))
                    vol = os.path.splitdrive(s["path"])[0] or "/"
                    held_by[vol] = held_by.get(vol, 0) + int(s.get("alloc", 0))
                    can_purge += 1
                if done.get("trash_ref"):
                    in_trash += 1
            if st.result == "failed_attention" and not st.resolved:
                attention += 1
        state = str((end or {}).get("state", "running" if run else "waiting"))
        if undone and undone >= sum(by_action.values()):
            state = "undone"
        days = int(self._setting("dupes.cleanup.purge_reminder_days", 7) or 0)
        started = str(plan.get("started", ""))
        reminder = False
        if days and held and started:
            with contextlib.suppress(ValueError):
                reminder = (datetime.now(UTC) - datetime.fromisoformat(started)).days >= days
        return pc.CleanupRunSummary(
            run_id=d.name,
            started=started,
            finished=datetime.fromtimestamp(float((end or {}).get("at", 0) or 0), tz=UTC).isoformat()
            if end
            else "",
            state=state
            if state
            in (
                "waiting",
                "preflight",
                "running",
                "paused",
                "done",
                "partial",
                "stopped",
                "failed",
                "undone",
                "recovered",
            )
            else "done",  # type: ignore[arg-type]
            roots=list(plan.get("roots", [])),
            by_action=by_action,
            counts=counts,
            freed_now=freed_now,
            held=held,
            held_by_volume=held_by,
            in_trash=in_trash,
            can_undo=can_undo,
            can_purge=can_purge,
            attention=attention,
            purge_reminder=reminder,
        )

    async def runs(self, p: pc.CleanupRunsParams) -> pc.CleanupRunsPage:
        def page() -> pc.CleanupRunsPage:
            dirs = list(run_dirs())
            return pc.CleanupRunsPage(
                total=len(dirs), items=[self._summary(d) for d in dirs[p.offset : p.offset + p.limit]]
            )

        return await asyncio.to_thread(page)

    def _run_detail_sync(self, d: Path, filt: str, offset: int, limit: int) -> pc.CleanupRunDetail:
        if not (d / "plan.json").exists():
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_run"].format(id=d.name))
        plan = read_plan(d)
        steps, _, _ = fold(read_records(d))
        items: list[pc.CleanupRunItem] = []
        for s in plan.get("steps", []):
            seq = int(s["seq"])
            st = steps.get(seq)
            code = st.result if st and st.result else ("pending" if st and st.intent else "cancelled")
            if st is not None and st.undone:
                state = "undone"
            elif st is not None and st.purged:
                state = "purged"
            elif code.startswith("done"):
                state = "done"
            elif code == "failed_attention" and not (st and st.resolved):
                state = "attention"
            elif code.startswith("failed"):
                state = "failed"
            elif code == "cancelled":
                state = "cancelled"
            elif code == "pending":
                state = "pending"
            else:
                state = "skipped"
            if filt and filt not in (code, state):
                continue
            done = (st.done if st else None) or {}
            items.append(
                pc.CleanupRunItem(
                    seq=seq,
                    action=s["action"],
                    path=s["path"],
                    target=(s.get("target") or {}).get("path", ""),
                    code=code,
                    text=X.text_of(code, st.message if st else ""),
                    state=state,  # type: ignore[arg-type]
                    recovery=str(done.get("rec", "")),
                    freed=int(done.get("freed_now", 0) or 0) + int(done.get("freed_later", 0) or 0),
                )
            )
        return pc.CleanupRunDetail(
            run=self._summary(d), total=len(items), items=items[offset : offset + limit]
        )

    async def run_detail(self, p: pc.CleanupRunParams) -> pc.CleanupRunDetail:
        return await asyncio.to_thread(
            self._run_detail_sync, runs_root() / p.run_id, p.filter, p.offset, p.limit
        )

    def _run_dir(self, run_id: str) -> Path:
        d = runs_root() / run_id
        if not (d / "plan.json").exists() or "/" in run_id or "\\" in run_id or ".." in run_id:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_run"].format(id=run_id))
        return d

    async def undo_plan(self, p: pc.CleanupRunSeqsParams) -> pc.CleanupUndoPlan:
        d = self._run_dir(p.run_id)
        items = await asyncio.to_thread(X.undo_plan, d, p.seqs)
        out = [pc.CleanupPlanItem(seq=s, path=path, can=can, why=why) for s, path, can, why in items]
        return pc.CleanupUndoPlan(
            run_id=p.run_id,
            items=out[:5000],
            can=sum(1 for x in out if x.can),
            cannot=sum(1 for x in out if not x.can),
        )

    async def _file_task(
        self, kind: str, run_id: str, work: Callable[[X.RunCtx, TaskHandle], Any], *, delay: float = 0.0
    ) -> pc.CleanupTask:
        if self.server.ops.busy():
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["busy"])
        d = self._run_dir(run_id)
        t = self._new_task(kind, run_id=run_id)
        t.state = "waiting" if delay else "running"
        loop = asyncio.get_running_loop()
        plan = read_plan(d)
        ctx = self._ctx_for(run_id, plan)
        ctx.progress = self._loop_progress(t, loop)
        ctx.cancel = t.cancel

        async def go() -> None:
            try:
                if delay:
                    t.message = TEXT["purge_wait"].format(s=int(delay))
                    self._emit(t, force=True)
                    try:
                        await asyncio.sleep(delay)
                    except asyncio.CancelledError:
                        t.state = "stopped"
                        t.message = ""
                        return
                t.state = "running"
                t.message = ""
                self._emit(t, force=True)
                result = await asyncio.to_thread(work, ctx, t)
                t.counts = {}
                for code, _ in (result or {}).values():
                    t.counts[code] = t.counts.get(code, 0) + 1
                t.done = sum(t.counts.values())
                t.total = t.done
                t.state = "undone" if kind == "undo" else "done"
            except Exception:
                logger.exception("cleanup {} of {} failed", kind, run_id)
                t.state = "failed"
            finally:
                self._emit(t, force=True)
                await asyncio.to_thread(self._load_link_targets)
                with contextlib.suppress(Exception):
                    await self._after_history(plan)

        t.task = loop.create_task(go())
        return pc.CleanupTask(task_id=t.id, kind=kind, run_id=run_id, delay_s=delay)  # type: ignore[arg-type]

    async def _after_history(self, plan: dict[str, Any]) -> None:
        """After undo or purge the searches covering those files see the changes (monitored scans by
        themselves; the others read the parents again)."""
        parents = sorted({s["parent"] for s in plan.get("steps", [])})
        for handle in list(self.server.scans.scans.values()):
            if handle.running or handle.monitor is not None or handle.source != "scan":
                continue
            for parent in parents:
                node = None
                with contextlib.suppress(Exception):
                    node = handle.store.find(parent)
                if node is not None:
                    with contextlib.suppress(Exception):
                        await self.server.monitors.rescan(handle, node)

    async def undo(self, p: pc.CleanupRunSeqsParams) -> pc.CleanupTask:
        return await self._file_task(
            "undo", p.run_id, lambda ctx, t: X.undo(ctx, runs_root() / p.run_id, p.seqs, beside=p.beside)
        )

    async def purge_plan(self, p: pc.CleanupPurgeParams) -> pc.CleanupPurgePlan:
        d = self._run_dir(p.run_id)
        items = await asyncio.to_thread(X.purge_plan, d, p.seqs)
        out = [
            pc.CleanupPlanItem(seq=s, path=path, can=can, why=why, bytes=b)
            for s, path, can, why, b, _ in items
        ]
        s = self._summary(d)
        return pc.CleanupPurgePlan(
            run_id=p.run_id,
            items=out[:5000],
            can=sum(1 for x in out if x.can),
            cannot=sum(1 for x in out if not x.can),
            bytes=sum(x.bytes for x in out if x.can),
            streams=sum(1 for *_, st in items if st),
            in_trash=s.in_trash,
        )

    async def purge(self, p: pc.CleanupPurgeParams) -> pc.CleanupTask:
        return await self._file_task(
            "purge",
            p.run_id,
            lambda ctx, t: X.purge(
                ctx, runs_root() / p.run_id, p.seqs, verify=p.verify, ack_streams=p.ack_streams
            ),
            delay=PURGE_DELAY_S,
        )

    async def resolve(self, p: pc.CleanupResolveParams) -> pc.CleanupRunDetail:
        d = self._run_dir(p.run_id)
        plan = read_plan(d)
        ctx = self._ctx_for(p.run_id, plan)
        await asyncio.to_thread(X.resolve, ctx, d, p.seq, p.choice)
        return await asyncio.to_thread(self._run_detail_sync, d, "", 0, 200)


def _lockers(paths: list[str]) -> dict[str, list[str]]:
    from ..ops.locks import find_lockers

    return find_lockers(paths)


def _row_of(plan: CleanupPlan, s: dict[str, Any]) -> int:
    """The member row of a step's copy in the results (the plan was built from them)."""
    r = int(s.get("row", -1)) if "row" in s else -1
    if 0 <= r < plan.n and plan.paths[r] == s["path"]:
        return r
    for i, p in enumerate(plan.paths):
        if p == s["path"] and int(plan.scan[i]) == int(s["scan"]):
            return i
    return -1


def _skipped(skip: dict[int, tuple[str, str]], steps: list[dict[str, Any]]) -> list[pc.CleanupSkipped]:
    by: dict[str, list[str]] = {}
    path = {int(s["seq"]): s["path"] for s in steps}
    for seq, (code, _why) in skip.items():
        by.setdefault(code, []).append(path.get(seq, ""))
    return [
        pc.CleanupSkipped(code=c, text=X.text_of(c), count=len(v), examples=v[:5])
        for c, v in sorted(by.items(), key=lambda kv: -len(kv[1]))
    ]

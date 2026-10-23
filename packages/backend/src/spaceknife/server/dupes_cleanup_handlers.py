"""Handlers for dupes.cleanup_*: duplicate-file cleanup plans (capability dupes.cleanup.v1,
docs/dupes.md §16). Every method needs the capability: a front end that does not know cleanup plans
never reaches them."""

from __future__ import annotations

from typing import TYPE_CHECKING

from spaceknife_protocol import CAP_DUPES_CLEANUP
from spaceknife_protocol import dupes_cleanup as pc
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    DUPES_CLEANUP_APPROVE,
    DUPES_CLEANUP_CANCEL,
    DUPES_CLEANUP_CLOSE,
    DUPES_CLEANUP_EDIT,
    DUPES_CLEANUP_EXECUTE,
    DUPES_CLEANUP_EXPORT,
    DUPES_CLEANUP_FACETS,
    DUPES_CLEANUP_GROUP,
    DUPES_CLEANUP_GROUPS,
    DUPES_CLEANUP_IMPORT,
    DUPES_CLEANUP_OPEN,
    DUPES_CLEANUP_PAIRS,
    DUPES_CLEANUP_PAUSE,
    DUPES_CLEANUP_PREFLIGHT,
    DUPES_CLEANUP_PREVIEW,
    DUPES_CLEANUP_PURGE,
    DUPES_CLEANUP_PURGE_PLAN,
    DUPES_CLEANUP_RECOVER_REPORT,
    DUPES_CLEANUP_REDO_EDIT,
    DUPES_CLEANUP_RESOLVE,
    DUPES_CLEANUP_RESUME,
    DUPES_CLEANUP_RESUME_RUN,
    DUPES_CLEANUP_RISK_COPIES,
    DUPES_CLEANUP_RUN,
    DUPES_CLEANUP_RUNS,
    DUPES_CLEANUP_SAMPLE,
    DUPES_CLEANUP_STATE,
    DUPES_CLEANUP_SUMMARY,
    DUPES_CLEANUP_UNDO,
    DUPES_CLEANUP_UNDO_EDIT,
    DUPES_CLEANUP_UNDO_PLAN,
    DUPES_CLEANUP_VOLUMES,
    DUPES_CLEANUP_WHO_USES,
)

from ..i18n import tr
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def _need(session: Session) -> None:
    if CAP_DUPES_CLEANUP not in session.capabilities:
        raise RpcError(ErrorCode.INVALID_REQUEST, tr("这个界面不支持重复文件清理方案"))


def install(server: Server) -> None:
    router = server.router
    m = server.cleanup

    @router.method(DUPES_CLEANUP_OPEN)
    async def cleanup_open(session: Session, params: pc.CleanupOpenParams) -> pc.CleanupState:
        _need(session)
        return await m.open(params)

    @router.method(DUPES_CLEANUP_RESUME)
    async def cleanup_resume(session: Session, params: pc.CleanupResumeParams) -> pc.CleanupState:
        _need(session)
        return await m.resume(params)

    @router.method(DUPES_CLEANUP_STATE)
    async def cleanup_state(session: Session, params: pc.CleanupPlanParams) -> pc.CleanupState:
        _need(session)
        return m.state_of(m._get(params.plan_id))

    @router.method(DUPES_CLEANUP_PREVIEW)
    async def cleanup_preview(session: Session, params: pc.CleanupEditParams) -> pc.CleanupDiff:
        _need(session)
        return await m.preview(params)

    @router.method(DUPES_CLEANUP_EDIT)
    async def cleanup_edit(session: Session, params: pc.CleanupEditParams) -> pc.CleanupState:
        _need(session)
        return await m.edit(params)

    @router.method(DUPES_CLEANUP_UNDO_EDIT)
    async def cleanup_undo_edit(session: Session, params: pc.CleanupVersionParams) -> pc.CleanupState:
        _need(session)
        return await m.undo_edit(params)

    @router.method(DUPES_CLEANUP_REDO_EDIT)
    async def cleanup_redo_edit(session: Session, params: pc.CleanupVersionParams) -> pc.CleanupState:
        _need(session)
        return await m.undo_edit(params, redo=True)

    @router.method(DUPES_CLEANUP_GROUPS)
    async def cleanup_groups(session: Session, params: pc.CleanupGroupsParams) -> pc.CleanupGroupsPage:
        _need(session)
        return await m.groups(params)

    @router.method(DUPES_CLEANUP_GROUP)
    async def cleanup_group(session: Session, params: pc.CleanupGroupParams) -> pc.CleanupGroupDetail:
        _need(session)
        return await m.group(params)

    @router.method(DUPES_CLEANUP_FACETS)
    async def cleanup_facets(session: Session, params: pc.CleanupFilterParams) -> pc.CleanupFacets:
        _need(session)
        return await m.facets(params)

    @router.method(DUPES_CLEANUP_PAIRS)
    async def cleanup_pairs(session: Session, params: pc.CleanupPairsParams) -> pc.CleanupPairsPage:
        _need(session)
        return await m.pairs(params)

    @router.method(DUPES_CLEANUP_VOLUMES)
    async def cleanup_volumes(session: Session, params: pc.CleanupPlanParams) -> pc.CleanupVolumeMatrix:
        _need(session)
        return await m.volumes(params)

    @router.method(DUPES_CLEANUP_SUMMARY)
    async def cleanup_summary(session: Session, params: pc.CleanupPlanParams) -> pc.CleanupSummary:
        _need(session)
        return await m.summary(params)

    @router.method(DUPES_CLEANUP_RISK_COPIES)
    async def cleanup_risk_copies(
        session: Session, params: pc.CleanupRiskCopiesParams
    ) -> pc.CleanupRiskCopies:
        _need(session)
        return await m.risk_copies(params)

    @router.method(DUPES_CLEANUP_SAMPLE)
    async def cleanup_sample(session: Session, params: pc.CleanupSampleParams) -> pc.CleanupSample:
        _need(session)
        return await m.sample(params)

    @router.method(DUPES_CLEANUP_WHO_USES)
    async def cleanup_who_uses(session: Session, params: pc.CleanupFilterParams) -> pc.CleanupWhoUses:
        _need(session)
        return await m.who_uses(params)

    @router.method(DUPES_CLEANUP_EXPORT)
    async def cleanup_export(session: Session, params: pc.CleanupExportParams) -> pc.CleanupExportResult:
        _need(session)
        return await m.export(params)

    @router.method(DUPES_CLEANUP_IMPORT)
    async def cleanup_import(session: Session, params: pc.CleanupImportParams) -> pc.CleanupImportResult:
        _need(session)
        return await m.import_(params)

    @router.method(DUPES_CLEANUP_APPROVE)
    async def cleanup_approve(session: Session, params: pc.CleanupApproveParams) -> pc.CleanupApprovalInfo:
        _need(session)
        return await m.approve(params)

    @router.method(DUPES_CLEANUP_PREFLIGHT)
    async def cleanup_preflight(session: Session, params: pc.CleanupTokenParams) -> pc.CleanupTask:
        _need(session)
        return await m.preflight(params)

    @router.method(DUPES_CLEANUP_EXECUTE)
    async def cleanup_execute(session: Session, params: pc.CleanupExecuteParams) -> pc.CleanupTask:
        _need(session)
        return await m.execute(session, params)

    @router.method(DUPES_CLEANUP_PAUSE)
    async def cleanup_pause(session: Session, params: pc.CleanupTaskParams) -> pc.CleanupTask:
        _need(session)
        return m.pause(params)

    @router.method(DUPES_CLEANUP_RESUME_RUN)
    async def cleanup_resume_run(session: Session, params: pc.CleanupTaskParams) -> pc.CleanupTask:
        _need(session)
        return m.resume_run(params)

    @router.method(DUPES_CLEANUP_CANCEL)
    async def cleanup_cancel(session: Session, params: pc.CleanupTaskParams) -> pc.CleanupTask:
        _need(session)
        return m.cancel(params)

    @router.method(DUPES_CLEANUP_RUNS)
    async def cleanup_runs(session: Session, params: pc.CleanupRunsParams) -> pc.CleanupRunsPage:
        _need(session)
        return await m.runs(params)

    @router.method(DUPES_CLEANUP_RUN)
    async def cleanup_run(session: Session, params: pc.CleanupRunParams) -> pc.CleanupRunDetail:
        _need(session)
        return await m.run_detail(params)

    @router.method(DUPES_CLEANUP_UNDO_PLAN)
    async def cleanup_undo_plan(session: Session, params: pc.CleanupRunSeqsParams) -> pc.CleanupUndoPlan:
        _need(session)
        return await m.undo_plan(params)

    @router.method(DUPES_CLEANUP_UNDO)
    async def cleanup_undo(session: Session, params: pc.CleanupRunSeqsParams) -> pc.CleanupTask:
        _need(session)
        return await m.undo(params)

    @router.method(DUPES_CLEANUP_PURGE_PLAN)
    async def cleanup_purge_plan(session: Session, params: pc.CleanupPurgeParams) -> pc.CleanupPurgePlan:
        _need(session)
        return await m.purge_plan(params)

    @router.method(DUPES_CLEANUP_PURGE)
    async def cleanup_purge(session: Session, params: pc.CleanupPurgeParams) -> pc.CleanupTask:
        _need(session)
        return await m.purge(params)

    @router.method(DUPES_CLEANUP_RESOLVE)
    async def cleanup_resolve(session: Session, params: pc.CleanupResolveParams) -> pc.CleanupRunDetail:
        _need(session)
        return await m.resolve(params)

    @router.method(DUPES_CLEANUP_RECOVER_REPORT)
    async def cleanup_recover_report(session: Session, params: Empty) -> pc.CleanupRecoverReport:
        _need(session)
        return m.recover_report(session)

    @router.method(DUPES_CLEANUP_CLOSE)
    async def cleanup_close(session: Session, params: pc.CleanupPlanParams) -> EmptyResult:
        _need(session)
        await m.close(params)
        return EmptyResult()

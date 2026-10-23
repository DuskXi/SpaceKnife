"""Handlers for dupes.*: duplicate-file searches (capability dupes.v1, docs/dupes.md §11)."""

from __future__ import annotations

from typing import TYPE_CHECKING

from spaceknife_protocol import CAP_DUPES
from spaceknife_protocol import dupes as pd
from spaceknife_protocol.base import EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    DUPES_BASKET_MEMBERS,
    DUPES_CANCEL,
    DUPES_CLEAR,
    DUPES_FOLDER,
    DUPES_FOLDERS,
    DUPES_GROUP,
    DUPES_GROUPS,
    DUPES_KEEP,
    DUPES_LOCATE,
    DUPES_PAUSE,
    DUPES_PLAN,
    DUPES_RESUME,
    DUPES_SET_LOAD,
    DUPES_START,
    DUPES_STATUS,
    DUPES_TO_BASKET,
)

from ..i18n import tr
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def _need(session: Session) -> None:
    if CAP_DUPES not in session.capabilities:
        raise RpcError(ErrorCode.INVALID_REQUEST, tr("这个界面不支持查找重复文件"))


def install(server: Server) -> None:
    router = server.router
    dupes = server.dupes

    @router.method(DUPES_PLAN)
    async def dupes_plan(session: Session, params: pd.DupesScope) -> pd.DupesPlan:
        _need(session)
        return await dupes.plan(params)

    @router.method(DUPES_START)
    async def dupes_start(session: Session, params: pd.DupesStartParams) -> pd.DupesStatus:
        _need(session)
        return await dupes.start(params)

    @router.method(DUPES_PAUSE)
    async def dupes_pause(session: Session, params: pd.DupesJobParams) -> pd.DupesStatus:
        return await dupes.pause(params.job_id)

    @router.method(DUPES_RESUME)
    async def dupes_resume(session: Session, params: pd.DupesJobParams) -> pd.DupesStatus:
        return await dupes.resume(params.job_id)

    @router.method(DUPES_CANCEL)
    async def dupes_cancel(session: Session, params: pd.DupesJobParams) -> pd.DupesStatus:
        return await dupes.cancel(params.job_id)

    @router.method(DUPES_STATUS)
    async def dupes_status(session: Session, params: pd.DupesJobParams) -> pd.DupesStatus:
        return dupes.status_of(dupes.get(params.job_id))

    @router.method(DUPES_SET_LOAD)
    async def dupes_set_load(session: Session, params: pd.DupesLoadParams) -> pd.DupesStatus:
        return dupes.set_load(params)

    @router.method(DUPES_GROUPS)
    async def dupes_groups(session: Session, params: pd.DupesGroupsParams) -> pd.DupesGroupsPage:
        return await dupes.groups(params)

    @router.method(DUPES_GROUP)
    async def dupes_group(session: Session, params: pd.DupesGroupParams) -> pd.DupeGroup:
        return dupes.group(params)

    @router.method(DUPES_FOLDER)
    async def dupes_folder(session: Session, params: pd.DupesFolderParams) -> pd.DupesFolderInfo:
        return await dupes.folder(params)

    @router.method(DUPES_FOLDERS)
    async def dupes_folders(session: Session, params: pd.DupesFoldersParams) -> pd.DupesFoldersPage:
        return await dupes.folders(params)

    @router.method(DUPES_LOCATE)
    async def dupes_locate(session: Session, params: pd.DupesLocateParams) -> pd.DupesLocateResult:
        return await dupes.locate(session, params)

    @router.method(DUPES_BASKET_MEMBERS)
    async def dupes_basket_members(
        session: Session, params: pd.DupesBasketMembersParams
    ) -> pd.DupesBasketMembers:
        return dupes.basket_members(params)

    @router.method(DUPES_KEEP)
    async def dupes_keep(session: Session, params: pd.DupesKeepParams) -> pd.DupesStatus:
        return await dupes.keep(params)

    @router.method(DUPES_TO_BASKET)
    async def dupes_to_basket(session: Session, params: pd.DupesBasketParams) -> pd.DupesBasketResult:
        return await dupes.to_basket(params)

    @router.method(DUPES_CLEAR)
    async def dupes_clear(session: Session, params: pd.DupesJobParams) -> EmptyResult:
        await dupes.clear(params.job_id)
        return EmptyResult()

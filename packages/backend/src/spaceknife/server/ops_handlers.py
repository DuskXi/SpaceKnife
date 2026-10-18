"""Handlers for path.forms, basket.* and ops.*: the delete basket and file operations (W5)."""

from __future__ import annotations

from typing import TYPE_CHECKING

from spaceknife_protocol import ops as po
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.methods import (
    BASKET_ADD,
    BASKET_CLEAR,
    BASKET_EXECUTE,
    BASKET_LIST,
    BASKET_REMOVE,
    OPS_CANCEL,
    OPS_UNDO,
    PATH_FORMS,
)

from .ops_service import OpsService

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def install(server: Server) -> None:
    router = server.router
    ops: OpsService = server.ops

    @router.method(PATH_FORMS)
    async def path_forms(session: Session, params: po.PathFormsParams) -> po.PathForms:
        return ops.path_forms(params.scan_id, params.node)

    @router.method(BASKET_LIST)
    async def basket_list(session: Session, _: Empty) -> po.BasketState:
        return await ops.state()

    @router.method(BASKET_ADD)
    async def basket_add(session: Session, params: po.BasketAddParams) -> po.BasketAddResult:
        return await ops.add(session, params)

    @router.method(BASKET_REMOVE)
    async def basket_remove(session: Session, params: po.BasketRemoveParams) -> po.BasketState:
        return await ops.remove(params.ids)

    @router.method(BASKET_CLEAR)
    async def basket_clear(session: Session, _: Empty) -> po.BasketState:
        return await ops.clear()

    @router.method(BASKET_EXECUTE)
    async def basket_execute(session: Session, params: po.BasketExecuteParams) -> po.OpTask:
        return ops.execute(session, params)

    @router.method(OPS_CANCEL)
    async def ops_cancel(session: Session, params: po.OpTaskParams) -> EmptyResult:
        ops.cancel(params.task_id)
        return EmptyResult()

    @router.method(OPS_UNDO)
    async def ops_undo(session: Session, params: po.OpTaskParams) -> EmptyResult:
        await ops.undo(params.task_id)
        return EmptyResult()

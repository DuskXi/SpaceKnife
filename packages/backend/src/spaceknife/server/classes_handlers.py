"""Handlers for classes.*: the editable file class table (W7 「文件类别」, UI-W7-33)."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING

from spaceknife_protocol.base import Empty
from spaceknife_protocol.classes import ClassesEvent, ClassesState, ClassesUpdateParams
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import CLASSES_GET, CLASSES_RESET, CLASSES_UPDATE, EVENT_CLASSES

from .. import classes
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def state() -> ClassesState:
    """The table as the interface shows it: built-in names in the current language (M7.5)."""
    table = classes.active()
    return ClassesState(
        classes=classes.shown(table), defaults=classes.shown(classes.DEFAULT_CLASSES), custom=table.custom
    )


def load_saved() -> None:
    """At start: the user's table, if there is one."""
    classes.load_user()


async def apply_table(server: Server, table: classes.ClassTable) -> ClassesState:
    """Make a table the active one: saved, announced, every view recoloured and refiltered."""
    await asyncio.to_thread(classes.save, table, classes.user_file())
    classes.set_active(table)
    out = state()
    for session in list(server.sessions):
        session.emit(EVENT_CLASSES, ClassesEvent(state=out))
    await server.views.reclassify()  # colours, :class: filters, class totals
    return out


def install(server: Server) -> None:
    router = server.router

    async def apply(table: classes.ClassTable) -> ClassesState:
        return await apply_table(server, table)

    @router.method(CLASSES_GET)
    async def classes_get(session: Session, _: Empty) -> ClassesState:
        return state()

    @router.method(CLASSES_UPDATE)
    async def classes_update(session: Session, params: ClassesUpdateParams) -> ClassesState:
        try:
            table = classes.build(list(params.classes))
        except ValueError as exc:
            raise RpcError(ErrorCode.INVALID_PARAMS, str(exc)) from exc
        return await apply(table)

    @router.method(CLASSES_RESET)
    async def classes_reset(session: Session, _: Empty) -> ClassesState:
        return await apply(classes.DEFAULT_CLASSES)

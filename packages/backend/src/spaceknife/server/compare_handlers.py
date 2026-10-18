"""Handlers for compare.*: snapshot comparisons (W6, docs/manage.md §3)."""

from __future__ import annotations

from typing import TYPE_CHECKING

from spaceknife_protocol import compare as pc
from spaceknife_protocol.base import EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    COMPARE_CLOSE,
    COMPARE_INFO,
    COMPARE_NODE,
    COMPARE_OPEN,
    COMPARE_SOURCES,
    COMPARE_SUMMARY,
)

from .compares import TEXT, CompareManager
from .compute import Latest
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def install(server: Server) -> None:
    router = server.router
    compares: CompareManager = server.compares
    latest = Latest()

    @router.method(COMPARE_SOURCES)
    async def compare_sources(session: Session, params: pc.CompareSourcesParams) -> pc.CompareSourcesResult:
        return await compares.sources(params.scan_id)

    @router.method(COMPARE_OPEN)
    async def compare_open(session: Session, params: pc.CompareOpenParams) -> pc.CompareTask:
        return compares.open(session, params)

    @router.method(COMPARE_INFO)
    async def compare_info(session: Session, params: pc.CompareIdParams) -> pc.CompareInfo:
        handle = compares.get(params.compare_id)
        if handle.tree is None:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["not_ready"])
        return handle.info()

    @router.method(COMPARE_CLOSE)
    async def compare_close(session: Session, params: pc.CompareIdParams) -> EmptyResult:
        compares.close(params.compare_id)
        return EmptyResult()

    @router.method(COMPARE_SUMMARY)
    async def compare_summary(session: Session, params: pc.CompareSummaryParams) -> pc.CompareSummary:
        host = server.views.get(session, params.view_id)
        return await latest.run(("summary", params.view_id), compares.summary_of, host, params.limit)

    @router.method(COMPARE_NODE)
    async def compare_node(session: Session, params: pc.CompareNodeParams) -> pc.CompareNodeResult:
        host = server.views.get(session, params.view_id)
        return await compares.node(host, params.key, params.children)

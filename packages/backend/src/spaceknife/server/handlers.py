"""Built-in method handlers: system, app lifecycle and subscriptions."""

from __future__ import annotations

import asyncio
import os
import platform
from datetime import UTC, datetime
from typing import TYPE_CHECKING

from spaceknife_protocol import CAP_LOG_STREAM, CAP_SUBSCRIPTIONS
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import APP_SHUTDOWN, SUBSCRIBE, SYSTEM_INFO, SYSTEM_PING, UNSUBSCRIBE
from spaceknife_protocol.streams import TOPIC_LOG, SubscribeParams, SubscribeResult, UnsubscribeParams
from spaceknife_protocol.system import PingParams, PingResult, PlatformInfo, ShutdownResult, SystemInfo

from .rpc import RpcError
from .topics import log_topic

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def install(server: Server) -> None:
    from .classes_handlers import install as install_classes_handlers
    from .compare_handlers import install as install_compare_handlers
    from .diag import install as install_diag
    from .export_handlers import install as install_export_handlers
    from .filter_handlers import install as install_filter_handlers
    from .maintenance_handlers import install as install_maintenance_handlers
    from .ops_handlers import install as install_ops_handlers
    from .palette_handlers import install as install_palette_handlers
    from .scan_handlers import install as install_scan_handlers
    from .settings_handlers import install as install_settings_handlers
    from .view_handlers import install as install_view_handlers

    router = server.router
    server.topics.register(TOPIC_LOG, log_topic(server.log_hub))
    install_scan_handlers(server)
    install_filter_handlers(server)
    install_view_handlers(server)
    install_palette_handlers(server)
    install_settings_handlers(server)
    install_compare_handlers(server)
    install_classes_handlers(server)
    install_ops_handlers(server)
    install_export_handlers(server)
    install_maintenance_handlers(server)
    install_diag(server)

    @router.method(SYSTEM_INFO)
    async def system_info(session: Session, _: Empty) -> SystemInfo:
        return SystemInfo(
            server=server.server_info,
            pid=os.getpid(),
            started_at=server.started_at,
            platform=PlatformInfo(
                system=platform.system(), release=platform.release(), machine=platform.machine()
            ),
            sessions=len(server.sessions),
        )

    @router.method(SYSTEM_PING)
    async def system_ping(session: Session, params: PingParams) -> PingResult:
        return PingResult(payload=params.payload, server_time=datetime.now(UTC))

    @router.method(APP_SHUTDOWN)
    async def app_shutdown(session: Session, _: Empty) -> ShutdownResult:
        # Answer first, then stop: the response must reach the client before sockets close.
        asyncio.get_running_loop().call_later(0.05, server.request_shutdown)
        return ShutdownResult(accepted=True)

    @router.method(SUBSCRIBE)
    async def subscribe(session: Session, params: SubscribeParams) -> SubscribeResult:
        if CAP_SUBSCRIPTIONS not in session.capabilities:
            raise RpcError(ErrorCode.INVALID_REQUEST, "subscriptions capability was not negotiated")
        if params.topic == TOPIC_LOG and CAP_LOG_STREAM not in session.capabilities:
            raise RpcError(ErrorCode.INVALID_REQUEST, "log-stream capability was not negotiated")
        sub = await server.topics.open(session, params.topic, params.params, params.rate_hz)
        session.add_subscription(sub)
        return SubscribeResult(sub=sub.id)

    @router.method(UNSUBSCRIBE)
    async def unsubscribe(session: Session, params: UnsubscribeParams) -> EmptyResult:
        if not session.remove_subscription(params.sub):
            raise RpcError(ErrorCode.UNKNOWN_SUBSCRIPTION, f"unknown subscription: {params.sub}")
        return EmptyResult()

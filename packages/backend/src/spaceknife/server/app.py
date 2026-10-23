"""The WebSocket server: binds to loopback, enforces path/origin/subprotocol, owns sessions."""

from __future__ import annotations

import asyncio
import platform
import secrets
import sys
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, datetime
from http import HTTPStatus

from loguru import logger
from websockets.asyncio.server import Server as WsServer
from websockets.asyncio.server import ServerConnection, serve
from websockets.http11 import Request, Response
from websockets.typing import Origin, Subprotocol

from spaceknife_protocol import ALL_CAPABILITIES, SUBPROTOCOL, WS_PATH
from spaceknife_protocol.errors import CloseCode
from spaceknife_protocol.schema import schema_hash
from spaceknife_protocol.session import ServerInfo

from .. import __version__
from ..logbus import LogHub
from ..settings import SettingsStore
from .classes_handlers import load_saved as load_saved_classes
from .compares import CompareManager
from .monitors import MonitorManager
from .ops_service import OpsService
from .rpc import Router
from .scans import ScanManager
from .session import Session
from .topics import TopicRegistry
from .update_handlers import UpdateService
from .views import ViewManager

DEFAULT_ORIGINS: tuple[str | None, ...] = ("app://spaceknife", None)


def new_token() -> str:
    return secrets.token_urlsafe(32)


@dataclass(slots=True)
class ServerConfig:
    token: str
    host: str = "127.0.0.1"
    port: int = 0
    allowed_origins: tuple[str | None, ...] = DEFAULT_ORIGINS
    hello_timeout: float = 5.0
    max_inbound_bytes: int = 1 << 20
    max_outbound_chunk_bytes: int = 4 << 20
    ping_interval: float | None = 20.0
    capabilities: tuple[str, ...] = field(default=ALL_CAPABILITIES)
    log_file: bool = False  # write the rotating log file (the real app; not tests)
    update_checks: bool | None = None  # automatic update checks; None: as log_file (the real app)


class Server:
    def __init__(
        self, config: ServerConfig, *, log_hub: LogHub | None = None, router: Router | None = None
    ) -> None:
        if len(config.token) < 16:
            raise ValueError("token must be at least 16 characters")
        if config.host not in ("127.0.0.1", "::1"):
            logger.warning("binding to non-loopback host {} exposes the backend to the network", config.host)
        self.config = config
        self.log_hub = log_hub or LogHub()
        self.router = router or Router()
        self.topics = TopicRegistry()
        self.settings = SettingsStore()
        load_saved_classes()  # the user's file classes (W7)
        self.scans = ScanManager()
        self.scans.settings = self.settings
        self.views = ViewManager(self.scans)
        self.views.settings = self.settings
        self.monitors = MonitorManager(self.scans, self.views)  # file change hooks (M5)
        self.compares = CompareManager(self.scans, self.views)  # snapshot comparisons (W6)
        self.ops = OpsService(self)  # the delete basket and file operations (W5)
        self.views.ops = self.ops
        from .dupes import DupesManager

        self.dupes = DupesManager(self)  # duplicate files (docs/dupes.md)
        self.views.dupes = self.dupes
        from .dupes_cleanup import CleanupManager

        self.cleanup = CleanupManager(self)  # their cleanup plans and runs (docs/dupes.md §16)
        self.updates = UpdateService(self)  # update checks against the GitHub releases
        self.scans.monitors = self.monitors
        self.on_start: list[Callable[[], None]] = []  # run once the loop is serving
        self.background: list[asyncio.Task[None]] = []
        self.capabilities = config.capabilities
        self.sessions: set[Session] = set()
        self.started_at = datetime.now(UTC)
        self.schema_hash = schema_hash()
        self.server_info = ServerInfo(
            name="spaceknife-backend",
            version=__version__,
            python=platform.python_version(),
            platform=sys.platform,
        )
        self.shutdown_requested = asyncio.Event()
        self._ws: WsServer | None = None
        self.port = 0

        from .handlers import install
        from .update_handlers import install as install_update_handlers

        install(self)
        install_update_handlers(self)  # after the settings handlers: their listener sets the language

    async def start(self) -> int:
        # interrupted cleanup runs are finished from their journals before any session (I10)
        try:
            await self.cleanup.recover_all()
        except Exception:
            logger.exception("finishing interrupted cleanup runs failed")
        self._ws = await serve(
            self._handle,
            self.config.host,
            self.config.port,
            subprotocols=[Subprotocol(SUBPROTOCOL)],
            origins=[Origin(o) if o is not None else None for o in self.config.allowed_origins],
            process_request=self._process_request,
            compression=None,
            max_size=self.config.max_inbound_bytes,
            ping_interval=self.config.ping_interval,
            ping_timeout=self.config.ping_interval,
        )
        sock = next(iter(self._ws.sockets))
        self.port = int(sock.getsockname()[1])
        for hook in self.on_start:
            hook()
        logger.info("listening on ws://{}:{}{}", self.config.host, self.port, WS_PATH)
        return self.port

    @property
    def url(self) -> str:
        host = f"[{self.config.host}]" if ":" in self.config.host else self.config.host
        return f"ws://{host}:{self.port}{WS_PATH}"

    def _process_request(self, connection: ServerConnection, request: Request) -> Response | None:
        if request.path != WS_PATH:
            return connection.respond(HTTPStatus.NOT_FOUND, "Not Found\n")
        return None

    async def _handle(self, ws: ServerConnection) -> None:
        session = Session(self, ws)
        self.sessions.add(session)
        try:
            await session.run()
        finally:
            self.sessions.discard(session)
            self.views.close_session(session)
            self.compares.close_session(session)

    def request_shutdown(self) -> None:
        if not self.shutdown_requested.is_set():
            logger.info("shutdown requested")
            self.shutdown_requested.set()

    async def close(self) -> None:
        await asyncio.gather(
            *(s.close(CloseCode.GOING_AWAY, "server shutting down") for s in list(self.sessions)),
            return_exceptions=True,
        )
        if self._ws is not None:
            self._ws.close()
            await self._ws.wait_closed()
            self._ws = None
        for task in self.background:
            task.cancel()
        self.background.clear()
        await self.updates.close()
        await self.scans.close()
        logger.info("server stopped")

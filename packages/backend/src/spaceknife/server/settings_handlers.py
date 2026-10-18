"""Handlers for settings.* and the live effects of a settings change (W7, UI-W7-07, UI-W7-28)."""

from __future__ import annotations

import asyncio
import contextlib
from typing import TYPE_CHECKING

from loguru import logger

from spaceknife_protocol import settings as ps
from spaceknife_protocol.base import Empty
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EVENT_SETTINGS_CHANGED, SETTINGS_GET, SETTINGS_RESET, SETTINGS_UPDATE

from .. import paths
from ..log import set_file_logging
from ..settings import ConflictError, SettingsError, SettingsStore
from ..store import mapped
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .session import Session

POLL_INTERVAL = 1.0  # s between checks for external edits of the settings file


def install(server: Server) -> None:
    router = server.router
    store: SettingsStore = server.settings
    applied: dict[str, object] = {}

    def on_change(st: SettingsStore) -> None:
        state = st.state()
        for session in list(server.sessions):
            session.emit(EVENT_SETTINGS_CHANGED, ps.SettingsChangedEvent(state=state))
        apply_live(st)

    def apply_live(st: SettingsStore) -> None:
        level = st.get("advanced.log_level")
        if server.config.log_file and applied.get("log_level") != level:
            set_file_logging(paths.logs_dir() / "spaceknife.log", str(level))
            applied["log_level"] = level
        mapped.POLICY.fraction = int(st.get("advanced.memory_budget")) / 100  # the next growth / load
        server.views.apply_settings_live(st)
        monitor = bool(st.get("scan.monitor"))
        if applied.get("monitor", monitor) != monitor:  # live monitoring switched on or off
            with contextlib.suppress(RuntimeError):  # no loop yet: applied when scans start
                asyncio.get_running_loop().create_task(server.monitors.apply_settings())
        applied["monitor"] = monitor

    store.listeners["server"] = on_change
    apply_live(store)

    async def watch() -> None:
        while True:
            await asyncio.sleep(POLL_INTERVAL)
            try:
                store.poll()  # on the loop: listeners emit to sessions and schedule frames
            except Exception:
                logger.exception("polling the settings file failed")

    def start_watch() -> None:
        mapped.cleanup_stale()  # column files of crashed runs (delete-on-close normally does it)
        task = asyncio.get_running_loop().create_task(watch(), name="settings-watch")
        server.background.append(task)

    server.on_start.append(start_watch)

    def run(fn: object) -> ps.SettingsState:
        try:
            fn()  # type: ignore[operator]
        except ConflictError as exc:
            raise RpcError(ErrorCode.SETTINGS_CONFLICT, str(exc)) from exc
        except SettingsError as exc:
            raise RpcError(ErrorCode.INVALID_PARAMS, str(exc)) from exc
        return store.state()

    @router.method(SETTINGS_GET)
    async def settings_get(session: Session, _: Empty) -> ps.SettingsState:
        return store.state()

    @router.method(SETTINGS_UPDATE)
    async def settings_update(session: Session, params: ps.SettingsUpdateParams) -> ps.SettingsState:
        return run(lambda: store.update(params.values, params.version))

    @router.method(SETTINGS_RESET)
    async def settings_reset(session: Session, params: ps.SettingsResetParams) -> ps.SettingsState:
        return run(lambda: store.reset(params.category))

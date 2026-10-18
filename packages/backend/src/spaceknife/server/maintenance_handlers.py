"""Handlers of the settings page's 「高级」 (UI-W7-36): settings export / import (SpaceKnife's own file
or SpaceSniffer's configuration), the scan helper, the cache, remembered tags."""

from __future__ import annotations

import asyncio
import os
import shutil
from pathlib import Path
from typing import TYPE_CHECKING

from loguru import logger

from spaceknife_protocol import settings as ps
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    SETTINGS_EXPORT,
    SETTINGS_IMPORT,
    SYSTEM_CACHE_CLEAR,
    SYSTEM_HELPER_STOP,
    SYSTEM_MAINTENANCE,
    TAGS_CLEAR_SAVED,
)

from .. import classes, paths, settings_io
from ..i18n import Texts, tr
from ..settings import BY_KEY, SettingsError
from ..store import snapshot
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .session import Session

# User-facing text, translated when read (M7.5).
TEXT = Texts(
    {
        "write": "写不了 {path}：{reason}",
        "folder": "目标文件夹不存在：{folder}",
    }
)


def _tree_bytes(root: Path) -> tuple[int, int]:
    """(bytes, files) below a folder; a missing one: (0, 0). Small folders only (cache, snapshots)."""
    total = files = 0
    if not root.exists():
        return 0, 0
    for dirpath, _dirs, names in os.walk(root):
        for n in names:
            try:
                total += os.stat(os.path.join(dirpath, n)).st_size
                files += 1
            except OSError:
                continue
    return total, files


def _clear_cache(root: Path) -> None:
    """Delete what can be deleted: column files of live scans are open (Windows refuses, POSIX
    keeps the mapping), the rest goes."""
    if not root.exists():
        return
    for entry in root.iterdir():
        try:
            if entry.is_dir() and not entry.is_symlink():
                shutil.rmtree(entry, ignore_errors=True)
            else:
                entry.unlink()
        except OSError:
            continue


def install(server: Server) -> None:
    router = server.router

    def info_sync() -> ps.MaintenanceInfo:
        helper = server.scans.helper.current
        cache_bytes, _ = _tree_bytes(paths.cache_dir())
        snaps = paths.snapshots_dir()
        snap_bytes, _ = _tree_bytes(snaps)
        tags_dir = paths.data_dir() / "tags"
        return ps.MaintenanceInfo(
            helper_running=helper is not None,
            helper_elevated=bool(helper is not None and helper.elevated),
            cache_dir=str(paths.cache_dir()),
            cache_bytes=cache_bytes,
            snapshots_dir=str(snaps),
            snapshots_bytes=snap_bytes,
            snapshot_count=sum(1 for _ in snaps.glob(f"*/*{snapshot.SUFFIX}")) if snaps.exists() else 0,
            saved_tags=sum(1 for _ in tags_dir.glob("*.json")) if tags_dir.exists() else 0,
        )

    async def info() -> ps.MaintenanceInfo:
        return await asyncio.to_thread(info_sync)

    @router.method(SETTINGS_EXPORT)
    async def settings_export(session: Session, params: ps.SettingsFileParams) -> EmptyResult:
        path = Path(params.path)
        if not path.parent.is_dir():
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["folder"].format(folder=path.parent))
        try:
            await asyncio.to_thread(settings_io.export_to, server.settings, path)
        except OSError as exc:
            reason = exc.strerror or exc
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["write"].format(path=path, reason=reason)) from exc
        return EmptyResult()

    @router.method(SETTINGS_IMPORT)
    async def settings_import(session: Session, params: ps.SettingsImportParams) -> ps.SettingsImportResult:
        store = server.settings
        try:
            plan = await asyncio.to_thread(settings_io.read_plan, Path(params.path), dict(store.values))
        except ValueError as exc:
            raise RpcError(ErrorCode.INVALID_PARAMS, str(exc)) from exc
        changes = [
            ps.SettingChange(key=k, name=tr(BY_KEY[k].name), before=store.values[k], after=v)
            for k, v in plan.values.items()
        ]
        if params.apply:
            try:
                if plan.values:
                    store.update(plan.values)
            except SettingsError as exc:
                raise RpcError(ErrorCode.INVALID_PARAMS, str(exc)) from exc
            if plan.classes is not None:
                from .classes_handlers import apply_table

                await apply_table(server, classes.ClassTable(plan.classes))
            logger.info("settings imported from {} ({}): {} changes", params.path, plan.source, len(changes))
        return ps.SettingsImportResult(
            source=plan.source,
            changes=changes,
            classes=classes.shown(classes.ClassTable(plan.classes)) if plan.classes is not None else None,
            skipped=plan.skipped,
            applied=params.apply,
        )

    @router.method(SYSTEM_MAINTENANCE)
    async def system_maintenance(session: Session, _: Empty) -> ps.MaintenanceInfo:
        return await info()

    @router.method(SYSTEM_HELPER_STOP)
    async def system_helper_stop(session: Session, _: Empty) -> ps.MaintenanceInfo:
        await server.scans.helper.close()
        return await info()

    @router.method(SYSTEM_CACHE_CLEAR)
    async def system_cache_clear(session: Session, _: Empty) -> ps.MaintenanceInfo:
        await asyncio.to_thread(_clear_cache, paths.cache_dir())
        return await info()

    @router.method(TAGS_CLEAR_SAVED)
    async def tags_clear_saved(session: Session, _: Empty) -> ps.MaintenanceInfo:
        await asyncio.to_thread(shutil.rmtree, paths.data_dir() / "tags", True)
        await server.views.forget_tags()
        return await info()

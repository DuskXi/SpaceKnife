"""Handlers for volume.*, scan.*, tree.*, snapshot.* and io.* plus the ``scan`` and ``io`` topics."""

from __future__ import annotations

import asyncio
import os
from typing import TYPE_CHECKING, Any

from pydantic import ValidationError

from spaceknife_protocol import io as pio
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EVENT_IO_LOAD,
    EVENT_SCAN_PROGRESS,
    IO_STATUS,
    SCAN_CANCEL,
    SCAN_CHECK_PATHS,
    SCAN_CLOSE,
    SCAN_CONTINUE,
    SCAN_LIST,
    SCAN_PAUSE,
    SCAN_RESCAN,
    SCAN_RESUME,
    SCAN_SET_LOAD,
    SCAN_START,
    SCAN_STATUS,
    SCAN_USE_MFT,
    SNAPSHOT_LIST,
    SNAPSHOT_LOAD,
    SNAPSHOT_SAVE,
    TREE_CHILDREN,
    TREE_FIND,
    TREE_NODE,
    TREE_SUMMARY,
    VOLUME_LIST,
)

from ..scan.media import media_of
from ..scan.scanner import normalise_root
from ..scan.volume import VolumeInfo, list_volumes, volume_for
from ..store import snapshot
from .compute import Latest
from .rpc import RpcError
from .scans import node_info, on_disk, snapshot_info, summary_of, volume_model
from .topics import Subscription

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def install(server: Server) -> None:
    router = server.router
    latest = Latest()
    scans = server.scans

    @router.method(VOLUME_LIST)
    async def volume_list(session: Session, _: Empty) -> p.VolumeListResult:
        volumes = await asyncio.to_thread(list_volumes)
        await asyncio.to_thread(warm_media, volumes)  # media probes may block (diskutil): not on the loop
        return p.VolumeListResult(
            volumes=[v for v in map(volume_model, volumes) if v is not None], elevated=scans.mft_ready()
        )

    @router.method(SCAN_CHECK_PATHS)
    async def scan_check_paths(session: Session, params: p.PathCheckParams) -> p.PathCheckResult:
        return await asyncio.to_thread(check_paths, params.paths)

    @router.method(SCAN_START)
    async def scan_start(session: Session, params: p.ScanStartParams) -> p.ScanStatus:
        return (await scans.start(params)).status()

    @router.method(SCAN_CANCEL)
    async def scan_cancel(session: Session, params: p.ScanIdParams) -> p.ScanStatus:
        return scans.cancel(params.scan_id).status()

    @router.method(SCAN_STATUS)
    async def scan_status(session: Session, params: p.ScanIdParams) -> p.ScanStatus:
        return scans.get(params.scan_id).status()

    @router.method(SCAN_PAUSE)
    async def scan_pause(session: Session, params: p.ScanIdParams) -> p.ScanStatus:
        return scans.pause(params.scan_id).status()

    @router.method(SCAN_RESUME)
    async def scan_resume(session: Session, params: p.ScanIdParams) -> p.ScanStatus:
        return scans.resume(params.scan_id).status()

    @router.method(SCAN_USE_MFT)
    async def scan_use_mft(session: Session, params: p.ScanIdParams) -> p.ScanStatus:
        return scans.use_mft(params.scan_id).status()

    @router.method(SCAN_CONTINUE)
    async def scan_continue(session: Session, params: p.ScanIdParams) -> p.ScanStatus:
        return (await scans.continue_scan(params.scan_id)).status()

    @router.method(SCAN_RESCAN)
    async def scan_rescan(session: Session, params: p.ScanRescanParams) -> p.ScanStatus:
        handle = scans.get(params.scan_id)
        scans.check_node(handle, params.node)
        await server.monitors.rescan(handle, params.node)
        return handle.status()

    @router.method(SCAN_SET_LOAD)
    async def scan_set_load(session: Session, params: p.ScanSetLoadParams) -> p.ScanStatus:
        return scans.set_load(params).status()

    @router.method(IO_STATUS)
    async def io_status(session: Session, _: Empty) -> pio.IoStatusList:
        return pio.IoStatusList(disks=scans.io.statuses(history=True))

    @router.method(SCAN_LIST)
    async def scan_list(session: Session, _: Empty) -> p.ScanListResult:
        return p.ScanListResult(scans=[h.status() for h in scans.scans.values()])

    @router.method(SCAN_CLOSE)
    async def scan_close(session: Session, params: p.ScanIdParams) -> EmptyResult:
        server.views.close_scan(params.scan_id)
        server.compares.close_scan(params.scan_id)
        await scans.close_scan(params.scan_id)
        return EmptyResult()

    @router.method(TREE_NODE)
    async def tree_node(session: Session, params: p.TreeNodeParams) -> p.TreeNodeResult:
        cmp = server.compares.lookup(params.scan_id)  # compare views ask about change tree rows (W6)
        if cmp is not None and cmp.tree is not None:
            if not 0 <= params.node < len(cmp.tree):
                raise RpcError(ErrorCode.UNKNOWN_NODE, f"unknown node: {params.node}")
            return p.TreeNodeResult(
                node=node_info(cmp.store, params.node),
                path=cmp.store.path(params.node),
                on_disk=server.compares.on_disk(cmp, cmp.tree, params.node),
            )
        handle = scans.get(params.scan_id)
        scans.check_node(handle, params.node)
        return p.TreeNodeResult(
            node=node_info(handle.store, params.node),
            path=handle.store.path(params.node),
            on_disk=on_disk(handle.store, params.node),
        )

    @router.method(TREE_CHILDREN)
    async def tree_children(session: Session, params: p.TreeChildrenParams) -> p.TreeChildrenResult:
        handle = scans.get(params.scan_id)
        scans.check_node(handle, params.node)
        store = handle.store
        ordered = store.sorted_children(params.node, by=params.sort)
        page = ordered[params.offset : params.offset + params.limit]
        return p.TreeChildrenResult(
            parent=node_info(store, params.node),
            total=len(ordered),
            items=[node_info(store, int(i)) for i in page],
        )

    @router.method(TREE_FIND)
    async def tree_find(session: Session, params: p.TreeFindParams) -> p.TreeFindResult:
        handle = scans.get(params.scan_id)
        node = handle.store.find(params.path)
        return p.TreeFindResult(node=node_info(handle.store, node) if node is not None else None)

    @router.method(TREE_SUMMARY)
    async def tree_summary(session: Session, params: p.TreeSummaryParams) -> p.TreeSummaryResult:
        handle = scans.get(params.scan_id)
        scans.check_node(handle, params.node)
        return await latest.run(
            ("tree.summary", session.id), summary_of, server, handle, params.node, params.top
        )

    @router.method(SNAPSHOT_LIST)
    async def snapshot_list(session: Session, _: Empty) -> p.SnapshotListResult:
        found = await asyncio.to_thread(snapshot.list_snapshots)
        return p.SnapshotListResult(snapshots=[snapshot_info(path, header) for path, header in found])

    @router.method(SNAPSHOT_SAVE)
    async def snapshot_save(session: Session, params: p.SnapshotSaveParams) -> p.SnapshotInfo:
        return await scans.save_snapshot(params.scan_id, params.path)

    @router.method(SNAPSHOT_LOAD)
    async def snapshot_load(session: Session, params: p.SnapshotLoadParams) -> p.ScanStatus:
        return (await scans.load_snapshot(params.path)).status()

    async def open_scan_topic(
        session: Session, params: dict[str, Any] | None, rate_hz: float | None
    ) -> Subscription:
        try:
            opts = p.ScanTopicParams.model_validate(params or {})
        except ValidationError as exc:
            raise RpcError(ErrorCode.INVALID_PARAMS, "scan topic needs {scan_id}") from exc
        handle = scans.get(opts.scan_id)
        sub = Subscription(topic=p.TOPIC_SCAN)

        def push(status: p.ScanStatus) -> None:
            if not sub.closed:
                session.emit(EVENT_SCAN_PROGRESS, p.ScanProgressEvent(sub=sub.id, status=status))

        # capture the listener table, not the handle: a subscription outliving scan.close must not
        # keep the closed scan (its whole node store) alive
        listeners = handle.listeners
        listeners[sub.id] = push
        sub.on_close = lambda: listeners.pop(sub.id, None) and None
        push(handle.status())
        return sub

    server.topics.register(p.TOPIC_SCAN, open_scan_topic)

    async def open_io_topic(
        session: Session, params: dict[str, Any] | None, rate_hz: float | None
    ) -> Subscription:
        """Load state of every disk (or one), twice a second: the start page and the status bar."""
        try:
            opts = pio.IoTopicParams.model_validate(params or {})
        except ValidationError as exc:
            raise RpcError(ErrorCode.INVALID_PARAMS, "io topic takes {disk?}") from exc
        sub = Subscription(topic=pio.TOPIC_IO)
        govs = [g for g in scans.io.governors() if opts.disk in (None, g.disk.id)]

        def push(status: pio.IoStatus) -> None:
            if not sub.closed:
                session.emit(EVENT_IO_LOAD, pio.IoStatusEvent(sub=sub.id, status=status))

        undo = [g.subscribe(push) for g in govs]

        def close() -> None:
            for u in undo:
                u()

        sub.on_close = close
        for g in govs:
            push(g.status(history=True))
        return sub

    server.topics.register(pio.TOPIC_IO, open_io_topic)

    def start_io() -> None:
        server.background.append(asyncio.get_running_loop().create_task(scans.start_io(), name="io-start"))

    server.on_start.append(start_io)


def warm_media(volumes: list[VolumeInfo]) -> None:
    for v in volumes:
        if not v.reason:
            media_of(v.mount_point, v.device, v.is_remote)


def check_paths(paths: list[str]) -> p.PathCheckResult:
    """Validate typed scan targets (UI-W1-25): existence, folder, access; merge nested ones."""
    items: list[p.PathCheck] = []
    for raw in paths:
        text = raw.strip()
        if not text:
            continue
        norm = normalise_root(text)
        state, message, volume = "ok", "", ""
        try:
            if not os.path.isdir(norm):
                state, message = (
                    ("not_dir", "不是文件夹") if os.path.exists(norm) else ("missing", "路径不存在")
                )
            else:
                with os.scandir(norm) as it:
                    next(it, None)
                volume = volume_for(norm).mount_point
        except PermissionError:
            state, message = "denied", "无法访问：拒绝访问"
        except OSError as exc:
            state, message = "error", f"无法访问：{exc.strerror or exc}"
        items.append(p.PathCheck(path=text, normalized=norm, state=state, message=message, volume=volume))  # type: ignore[arg-type]
    ok = sorted((i for i in items if i.state == "ok"), key=lambda i: len(i.normalized))
    kept: list[str] = []
    merged: dict[int, str] = {}
    for item in ok:
        key = os.path.normcase(item.normalized)
        parent = next(
            (
                k
                for k in kept
                if key == os.path.normcase(k) or key.startswith(os.path.normcase(k).rstrip(r"\/") + os.sep)
            ),
            None,
        )
        if parent is None:
            kept.append(item.normalized)
        else:
            merged[id(item)] = parent
    out = [i.model_copy(update={"merged_into": merged[id(i)]}) if id(i) in merged else i for i in items]
    return p.PathCheckResult(items=out, roots=kept)

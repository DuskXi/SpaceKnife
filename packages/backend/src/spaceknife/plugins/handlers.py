"""Protocol handlers: plugin management, ``extensions.get``, ``plugin.fields`` and ``insight.node``."""

from __future__ import annotations

import asyncio
import contextlib
import time
from typing import TYPE_CHECKING, Any

from spaceknife_protocol import CAP_COLORS, CAP_PANELS, CAP_PLUGINS
from spaceknife_protocol import plugins as pp
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EXTENSIONS_GET,
    INSIGHT_NODE,
    PLUGIN_DESCRIBE,
    PLUGIN_DISABLE,
    PLUGIN_ENABLE,
    PLUGIN_FIELDS,
    PLUGIN_INSTALL,
    PLUGIN_LINK,
    PLUGIN_LIST,
    PLUGIN_RESTART,
    PLUGIN_UNINSTALL,
)
from spaceknife_protocol.text import LANG

from .. import i18n
from ..extensions import EXTENSIONS
from ..server.compute import compute
from ..server.rpc import RpcError

if TYPE_CHECKING:
    from ..server.app import Server
    from ..server.session import Session
    from .host import PluginHost

FIELD_LIMIT = {"inspector": 16, "hover": 4}
FIELD_CACHE = 2048


def _need(session: Session) -> None:
    if CAP_PLUGINS not in session.capabilities:
        raise RpcError(ErrorCode.INVALID_REQUEST, "plugins.v1 capability was not negotiated")


def extensions_state(host: PluginHost, caps: frozenset[str]) -> pp.ExtensionsState:
    lang = i18n.language()
    plugins = [
        pp.PluginBrief(
            id=r.id,
            name=r.name,
            icon=r.manifest.icon if r.manifest else "",
            state=r.state,
            version=r.manifest.version if r.manifest else "",
        )
        for r in sorted(host.records.values(), key=lambda r: (not r.builtin, r.id))
    ]
    reports = []
    for c in EXTENSIONS.of("report"):
        s = c.spec
        reports.append(
            pp.ReportInfo(
                id=c.id,
                plugin=c.owner,
                title=s.title.get(lang),
                icon=s.icon,
                slot=s.slot,
                refresh=s.refresh_list(),
                when=list(s.when),
                order=s.order,
                method=pp.wire_name(c.owner, c.local),
                static=s.blocks is not None,
            )
        )
    panels = []
    if CAP_PANELS in caps:
        for c in EXTENSIONS.of("panel"):
            s = c.spec
            rec = host.records.get(c.owner)
            panels.append(
                pp.PanelInfo(
                    id=c.id,
                    plugin=c.owner,
                    title=s.title.get(lang),
                    icon=s.icon,
                    slot=s.slot,
                    entry=s.entry,
                    height=s.height,
                    when=list(s.when),
                    permissions=sorted(rec.granted) if rec is not None else [],  # type: ignore[arg-type]
                )
            )
    fields = [
        pp.FieldInfo(
            id=c.id,
            plugin=c.owner,
            title=c.spec.title.get(lang),
            slots=list(c.spec.slots),
            order=c.spec.order,
        )
        for c in EXTENSIONS.of("node_field")
    ]
    cleanups = [
        pp.CleanupInfo(
            id=c.id,
            plugin=c.owner,
            title=c.spec.title.get(lang),
            description=c.spec.description.get(lang) if c.spec.description else "",
            icon=c.spec.icon,
            when=list(c.spec.when),
            method=pp.wire_name(c.owner, c.local),
        )
        for c in EXTENSIONS.of("cleanup")
    ]
    colors = []
    if CAP_COLORS in caps:
        for c in EXTENSIONS.of("color_mode"):
            s = c.spec
            colors.append(
                pp.ColorModeInfo(
                    id=c.id,
                    plugin=c.owner,
                    title=s.title.get(lang),
                    palette=list(s.palette),
                    legend=[t.get(lang) for t in s.legend],
                    folders=s.folders,
                    layouts=list(s.layouts),
                )
            )
    sources = [
        pp.SourceInfo(
            id=c.id,
            plugin=c.owner,
            title=c.spec.title.get(lang),
            icon=c.spec.icon,
            pick=c.spec.pick,
            extensions=list(c.spec.extensions),
            placeholder=c.spec.placeholder.get(lang) if c.spec.placeholder else "",
            on_disk=c.spec.on_disk,
            ops=c.spec.ops,
            watch=c.spec.watch,
        )
        for c in EXTENSIONS.of("scan_source")
    ]
    presets = [
        pp.PresetInfo(id=c.id, plugin=c.owner, title=c.spec.title.get(lang), expr=c.spec.expr)
        for c in EXTENSIONS.of("filter_preset")
    ]
    icons = {f"{c.owner}:{c.local}": str(c.impl) for c in EXTENSIONS.of("icon") if c.impl}
    return pp.ExtensionsState(
        version=EXTENSIONS.version,
        plugins=plugins,
        reports=reports,
        panels=panels,
        node_fields=fields,
        cleanups=cleanups,
        color_modes=colors,
        sources=sources,
        filter_presets=presets,
        icons=icons,
    )


async def node_fields(
    host: PluginHost, session: Session | None, p: pp.PluginFieldsParams
) -> pp.PluginFieldsResult:
    """Each enabled plugin's fields for a node, in parallel, each with its own time limit; a field
    that is late is left out (the hover card only shows what is cached, and asks for the rest)."""
    store = host.api.store_of(p.scan_id)
    if store is None:
        raise RpcError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {p.scan_id}")
    if not 0 <= p.node < len(store):
        raise RpcError(ErrorCode.UNKNOWN_NODE, f"node {p.node} does not exist")
    lang = i18n.language()
    wanted = [c for c in EXTENSIONS.of("node_field") if p.slot in c.spec.slots][: FIELD_LIMIT[p.slot]]
    version = int(store.version)
    out: list[pp.PluginField] = []
    pending = False
    todo = []
    for c in wanted:
        key = (c.id, p.scan_id, p.node, version)
        if key in host._field_cache:
            hit = host._field_cache[key]
            if hit is not None:
                out.append(hit.model_copy(update={"title": c.spec.title.get(lang)}))
            continue
        todo.append(c)

    async def one(c: Any) -> pp.PluginField | None:
        value, _frames = await host.call(
            c.owner,
            "field",
            c.local,
            {"node": p.node, "scan_id": p.scan_id},
            session=session,
            ctx={"scan_id": p.scan_id, "node": p.node},
            timeout=1.0,
        )
        if value is None:
            return None
        try:
            fv = pp.FieldValue.model_validate(value)
        except ValueError:
            return None
        text = fv.value
        if isinstance(text, str) and len(text) > 200:
            text = text[:200]
        return pp.PluginField(
            id=c.id,
            plugin=c.owner,
            title=c.spec.title.get(lang),
            value=text,
            unit=fv.unit,
            tone=fv.tone,
            node=fv.node,
            action=fv.action,
        )

    async def fetch(c: Any) -> None:
        key = (c.id, p.scan_id, p.node, version)
        try:
            host._field_cache[key] = await one(c)
        except (RpcError, asyncio.CancelledError):
            return
        if len(host._field_cache) > FIELD_CACHE:
            for k in list(host._field_cache)[: FIELD_CACHE // 4]:
                host._field_cache.pop(k, None)

    if p.slot == "hover":
        if todo:
            pending = True
            for c in todo:
                host._spawn(fetch(c))
    else:
        await asyncio.gather(*(fetch(c) for c in todo), return_exceptions=True)
        for c in todo:
            hit = host._field_cache.get((c.id, p.scan_id, p.node, version))
            if hit is not None:
                out.append(hit)
    order = {c.id: i for i, c in enumerate(wanted)}
    out.sort(key=lambda f: order.get(f.id, 0))
    return pp.PluginFieldsResult(fields=out, pending=pending)


async def insight_cards(
    host: PluginHost, session: Session | None, p: pp.InsightNodeParams
) -> pp.InsightNodeResult:
    from ..insights import match_node
    from ..insights.rules import refresh_user_rules

    store = host.api.store_of(p.scan_id)
    if store is None:
        raise RpcError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {p.scan_id}")
    if not 0 < p.node < len(store):
        raise RpcError(ErrorCode.UNKNOWN_NODE, f"node {p.node} does not exist")
    refresh_user_rules()
    lang = i18n.language()
    cards: list[pp.InsightCard] = []
    found = await compute(match_node, store, p.node, lang=lang)
    if found is not None:
        cards.append(
            pp.InsightCard(
                source="rule",
                rule=found.rule,
                title=found.title,
                reason=found.reason,
                kind=str(found.kind),
                cleanup=str(found.cleanup),
                app=found.app,
                note=found.note,
                command=found.command,
                path=found.path,
            )
        )

    async def one(c: Any) -> pp.InsightCard | None:
        try:
            value, _ = await host.call(
                c.owner,
                "insight",
                c.local,
                {"node": p.node, "scan_id": p.scan_id},
                session=session,
                ctx={"scan_id": p.scan_id, "node": p.node},
                timeout=1.0,
            )
        except RpcError:
            return None
        if not isinstance(value, dict):
            return None
        with contextlib.suppress(ValueError, TypeError):
            return pp.InsightCard(
                source="plugin",
                plugin=c.owner,
                title=str(value["title"]),
                reason=str(value["reason"]),
                kind=str(value.get("kind", "other")),
                cleanup=str(value.get("cleanup", "review")),
                app=str(value.get("app", "")),
                note=str(value.get("note", "")),
                command=str(value.get("command", "")),
                path=store.path(p.node),
            )
        return None

    got = await asyncio.gather(*(one(c) for c in EXTENSIONS.of("insight")))
    cards += [g for g in got if g is not None]
    return pp.InsightNodeResult(cards=cards)


def install(server: Server) -> None:
    router = server.router
    host: PluginHost = server.plugins

    @router.method(PLUGIN_LIST)
    async def plugin_list(session: Session, _: Empty) -> pp.PluginListResult:
        _need(session)
        return host.list_plugins()

    @router.method(PLUGIN_DESCRIBE)
    async def plugin_describe(session: Session, params: pp.PluginIdParams) -> pp.PluginDescribeResult:
        _need(session)
        return await host.describe(params.id)

    @router.method(PLUGIN_ENABLE)
    async def plugin_enable(session: Session, params: pp.PluginEnableParams) -> pp.PluginInfo:
        _need(session)
        return await host.enable(params.id, params.hash, list(params.grant))

    @router.method(PLUGIN_DISABLE)
    async def plugin_disable(session: Session, params: pp.PluginIdParams) -> pp.PluginInfo:
        _need(session)
        return await host.disable(params.id)

    @router.method(PLUGIN_RESTART)
    async def plugin_restart(session: Session, params: pp.PluginIdParams) -> pp.PluginInfo:
        _need(session)
        return await host.restart(params.id)

    @router.method(PLUGIN_INSTALL)
    async def plugin_install(session: Session, params: pp.PluginInstallParams) -> pp.PluginInstallResult:
        _need(session)
        return await host.install(params.path, params.replace)

    @router.method(PLUGIN_UNINSTALL)
    async def plugin_uninstall(session: Session, params: pp.PluginUninstallParams) -> EmptyResult:
        _need(session)
        await host.uninstall(params.id, params.remove_data)
        return EmptyResult()

    @router.method(PLUGIN_LINK)
    async def plugin_link(session: Session, params: pp.PluginLinkParams) -> pp.PluginInfo:
        _need(session)
        return await host.link(params.path)

    @router.method(EXTENSIONS_GET)
    async def extensions_get(session: Session, _: Empty) -> pp.ExtensionsState:
        _need(session)
        return extensions_state(host, session.capabilities)

    @router.method(PLUGIN_FIELDS)
    async def plugin_fields(session: Session, params: pp.PluginFieldsParams) -> pp.PluginFieldsResult:
        _need(session)
        return await node_fields(host, session, params)

    @router.method(INSIGHT_NODE)
    async def insight_node(session: Session, params: pp.InsightNodeParams) -> pp.InsightNodeResult:
        return await insight_cards(host, session if CAP_PLUGINS in session.capabilities else None, params)

    _ = (LANG, time)


__all__ = ["extensions_state", "insight_cards", "install", "node_fields"]

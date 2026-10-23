"""Protocol handlers: plugin management, ``extensions.get``, ``plugin.fields`` and ``insight.node``."""

from __future__ import annotations

import asyncio
import contextlib
from typing import TYPE_CHECKING, Any

from spaceknife_protocol import CAP_COLORS, CAP_PANELS, CAP_PLUGINS
from spaceknife_protocol import plugins as pp
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EXTENSIONS_GET,
    INSIGHT_NODE,
    INSIGHT_NODES,
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
FIELD_TIMEOUT_S = 1.0  # the details panel waits this long (queueing for the plugin included)
LATE_FIELD_S = 10.0  # a late field is still fetched in the background, for the next ask


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
                    short_title=s.short_title.get(lang) if s.short_title else "",
                    icon=s.icon,
                    slot=s.slot,
                    entry=s.entry,
                    height=s.height,
                    when=list(s.when),
                    permissions=sorted(rec.granted) if rec is not None else [],  # type: ignore[arg-type]
                    params=dict(s.params),
                    keep=s.keep,
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
            legend = host.legend_of(c, lang)
            colors.append(
                pp.ColorModeInfo(
                    id=c.id,
                    plugin=c.owner,
                    title=s.title.get(lang),
                    palette=list(s.palette),
                    legend=[x[0] for x in legend],
                    legend_colors=[x[1] for x in legend] if any(x[1] for x in legend) else [],
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
            size=c.spec.size,
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
    wanted = [c for c in wanted if _field_wanted(c, store, p.node)]
    version = int(store.version)
    out: list[pp.PluginField] = []
    pending = False
    todo = []
    for c in wanted:
        key = (c.id, p.scan_id, p.node, version, host.epochs[c.owner])
        if key in host._field_cache:
            hit = host._field_cache[key]
            if hit is not None:
                out.append(hit.model_copy(update={"title": c.spec.title.get(lang)}))
            continue
        todo.append(c)

    async def one(c: Any, timeout: float) -> pp.PluginField | None:  # noqa: ASYNC109
        value, _frames = await host.call(
            c.owner,
            "field",
            c.local,
            {"node": p.node, "scan_id": p.scan_id, "slot": p.slot},
            session=session,
            ctx={"scan_id": p.scan_id, "node": p.node, "slot": p.slot},
            timeout=timeout,
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

    late: list[Any] = []

    async def fetch(c: Any, timeout: float = FIELD_TIMEOUT_S) -> None:  # noqa: ASYNC109
        key = (c.id, p.scan_id, p.node, version, host.epochs[c.owner])
        if key in host._field_inflight:
            return
        host._field_inflight.add(key)
        try:
            value = await one(c, timeout)
        except RpcError as exc:
            if exc.code == ErrorCode.PLUGIN_TIMEOUT:
                late.append(c)  # busy or slow: answered later (the frontend asks again)
            return
        except asyncio.CancelledError:
            return
        finally:
            host._field_inflight.discard(key)
        if key[4] != host.epochs[c.owner]:
            return  # its settings changed meanwhile: this answer is stale
        host._field_cache[key] = value
        if len(host._field_cache) > FIELD_CACHE:
            for k in list(host._field_cache)[: FIELD_CACHE // 4]:
                host._field_cache.pop(k, None)

    if p.slot == "hover":
        if todo:
            pending = True
            for c in todo:
                host._spawn(fetch(c, LATE_FIELD_S))
    else:
        await asyncio.gather(*(fetch(c) for c in todo), return_exceptions=True)
        for c in todo:
            hit = host._field_cache.get((c.id, p.scan_id, p.node, version, host.epochs[c.owner]))
            if hit is not None:
                out.append(hit)
        if late:
            pending = True
            for c in late:
                host._spawn(fetch(c, LATE_FIELD_S))
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
        cards.append(_card(found))

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

    path = store.path(p.node).replace("\\", "/").lower()
    asked = [c for c in EXTENSIONS.of("insight") if _wanted(c, path)]
    got = await asyncio.gather(*(one(c) for c in asked))
    cards += [g for g in got if g is not None]
    return pp.InsightNodeResult(cards=cards)


def _field_wanted(c: Any, store: Any, node: int) -> bool:
    """A field with ``kinds``, ``exts`` or ``globs`` is asked only about the nodes they name."""
    spec = c.spec
    kinds, exts, globs = (
        getattr(spec, "kinds", None),
        getattr(spec, "exts", None),
        getattr(spec, "globs", None),
    )
    if not (kinds or exts or globs):
        return True
    from ..store.nodes import Kind

    kind = int(store.cols["kind"][node])
    if kinds and not (("file" in kinds and kind == Kind.FILE) or ("dir" in kinds and kind == Kind.DIR)):
        return False
    if exts:
        if kind != Kind.FILE:
            return False
        name = store.name(node).lower()
        if not any(name.endswith("." + x) for x in exts):
            return False
    return not globs or _wanted(c, store.path(node).replace("\\", "/").lower())


def _wanted(c: Any, path: str) -> bool:
    """A code insight with ``globs`` is asked only about the nodes that match one of them."""
    globs = getattr(c.spec, "globs", None)
    if not globs:
        return True
    from ..pathclass import compile_glob

    pats = _GLOBS.get(c.id)
    if pats is None or pats[0] != tuple(globs):
        pats = (tuple(globs), [compile_glob(g) for g in globs])
        _GLOBS[c.id] = pats
    return any(r.match(path) for r in pats[1])


_GLOBS: dict[str, tuple[tuple[str, ...], list[Any]]] = {}


def _card(found: Any) -> pp.InsightCard:
    return pp.InsightCard(
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


async def insight_many(host: PluginHost, p: pp.InsightNodesParams) -> pp.InsightNodesResult:
    """The rules' cards of many nodes at once (the built-in rules and the plugins' rule files)."""
    from ..insights import match_many
    from ..insights.rules import refresh_user_rules
    from ..store.nodes import Kind

    store = host.api.store_of(p.scan_id)
    if store is None:
        raise RpcError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {p.scan_id}")
    n = len(store)
    bad = [x for x in p.nodes if not 0 < x < n]
    if bad:
        raise RpcError(ErrorCode.UNKNOWN_NODE, f"node {bad[0]} does not exist")
    refresh_user_rules()
    lang = i18n.language()

    def work() -> list[pp.InsightCard | None]:
        c = store.cols
        paths = [store.path(x) for x in p.nodes]
        kinds = ["dir" if int(c["kind"][x]) == Kind.DIR else "file" for x in p.nodes]
        sizes: list[int | None] = [
            int(c["tot_alloc"][x]) if kinds[i] == "dir" else int(c["alloc"][x]) for i, x in enumerate(p.nodes)
        ]
        found = match_many(paths, kinds, sizes=sizes, lang=lang)
        return [None if f is None else _card(f) for f in found]

    return pp.InsightNodesResult(cards=await compute(work))


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

    @router.method(INSIGHT_NODES)
    async def insight_nodes(session: Session, params: pp.InsightNodesParams) -> pp.InsightNodesResult:
        return await insight_many(host, params)


__all__ = ["extensions_state", "insight_cards", "insight_many", "install", "node_fields"]

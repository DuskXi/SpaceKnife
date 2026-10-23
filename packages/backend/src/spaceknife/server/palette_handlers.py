"""Handlers for palette.*: the command palette's lists, path search, filter previews and recents (W4)."""

from __future__ import annotations

import asyncio
import threading
from collections import OrderedDict
from typing import TYPE_CHECKING

from spaceknife_protocol import palette as pp
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.filter_syntax import parse
from spaceknife_protocol.methods import (
    PALETTE_CHECK,
    PALETTE_CLEAR,
    PALETTE_COMMANDS,
    PALETTE_PATHS,
    PALETTE_PREVIEW,
    PALETTE_QUERY,
    PALETTE_REMEMBER,
)
from spaceknife_protocol.update import UpdateStatus

from .. import paths
from ..filter.engine import FilterContext, evaluate
from ..filter.vocab import platform_vocab
from ..layout.basis import LazySizes
from ..palette import paths as ppaths
from ..palette.commands import Context, label, match, registry
from ..palette.commands import evaluate as command_state
from ..palette.fuzzy import score, utf16_spans
from ..palette.recents import Recents
from ..store import snapshot
from ..store.nodes import Kind
from ..view.state import View
from .compute import Latest
from .rpc import RpcError

if TYPE_CHECKING:
    from .app import Server
    from .scans import ScanHandle
    from .session import Session

RECENT_SHOWN = 5
COMMANDS_SHOWN = 20
FILTER_CHARS = frozenset('*?;|<>:"')


def looks_like_filter(text: str) -> bool:
    """Filter syntax in the input (UI-W4-31): wildcards, separators, negation, comparisons, keywords,
    quotes, or a single leading backslash (folder mask; a double one is a UNC path)."""
    return any(ch in FILTER_CHARS for ch in text) or (text.startswith("\\") and not text.startswith("\\\\"))


def spans(text: str, runs: list[tuple[int, int]]) -> list[pp.Span]:
    return [pp.Span(start=a, end=b) for a, b in utf16_spans(text, runs)]


def target_of(handle: ScanHandle | None) -> str | None:
    return snapshot.root_key([r.path for r in handle.roots]) if handle is not None else None


def snapshot_count(target: str | None) -> int:
    """Saved snapshots of one scan target: the files in its auto-save folder."""
    if target is None:
        return 0
    folder = paths.snapshots_dir() / target
    try:
        return sum(1 for _ in folder.glob(f"*{snapshot.SUFFIX}"))
    except OSError:
        return 0


def install(server: Server) -> None:
    router = server.router
    latest = Latest()
    recents = Recents()
    previews: OrderedDict[tuple[object, ...], pp.PalettePreview] = OrderedDict()
    previews_lock = threading.Lock()

    def view_of(session: Session, view_id: str | None) -> View | None:
        host = server.views.views.get(view_id) if view_id else None
        return host.view if host is not None and host.session is session else None

    def handle_of(scan_id: str | None) -> ScanHandle | None:
        return server.scans.scans.get(scan_id) if scan_id else None

    def sizes_of(handle: ScanHandle) -> LazySizes:
        shared = server.views.shared(handle.id)
        assert shared is not None
        return LazySizes(shared.basis)  # the nodes asked for only, not an array of every node

    # ------------------------------------------------------------------ palette.query

    def context_of(
        session: Session, c: pp.PaletteContext, update: UpdateStatus
    ) -> tuple[Context, ScanHandle | None, View | None]:
        """What the commands' availability and status texts are computed from (palette.query and
        palette.check alike)."""
        scan_tab = c.tab == "scan"
        handle = handle_of(c.scan_id) if scan_tab else None
        view = view_of(session, c.view_id) if scan_tab else None
        status = handle.status() if handle is not None else None
        scanning = status is not None and status.state in ("queued", "scanning")
        ctx = Context(
            tab=c.tab,
            scanning=scanning,
            paused=status is not None and status.paused,
            stopped=status is not None and status.state == "cancelled" and status.pending_dirs > 0,
            method=status.method if status is not None else "hard",
            has_scan=handle is not None,
            source=handle.source if handle is not None else "scan",
            elapsed_s=status.elapsed_s if status is not None and status.state == "done" else None,
            view=view,
            snapshots=snapshot_count(target_of(handle)),
            basket=c.basket,
            caps=session.capabilities,
            app_version=update.current,
            update_version=update.latest.version if update.latest is not None else "",
            update_checking=update.checking,
            dupes=server.dupes.palette_status(c.scan_id) if scan_tab and c.scan_id else "",
            has_dupes=scan_tab and bool(c.scan_id) and server.dupes.has_dupes(c.scan_id or ""),
        )
        return ctx, handle, view

    def build(session: Session, params: pp.PaletteQueryParams, update: UpdateStatus) -> pp.PaletteResult:
        ctx, handle, view = context_of(session, params.context, update)
        target = target_of(handle)
        q = params.text.strip()
        store = handle.store if handle is not None else None
        sizes = sizes_of(handle) if handle is not None else None

        direct: list[pp.PaletteDirect] = []
        if q and ppaths.looks_like_path(q):
            r = ppaths.resolve(store, sizes, q)
            direct.append(
                pp.PaletteDirect(
                    kind="path",
                    text=r.path,
                    state=r.state,
                    node=r.node,
                    is_dir=r.is_dir,
                    alloc=r.alloc,
                    complete=r.complete,
                )
            )
        elif q and looks_like_filter(q):
            parsed = parse(q, None, platform_vocab())
            errors = [d for d in parsed.diagnostics if d.severity == "error"]
            direct.append(
                pp.PaletteDirect(
                    kind="filter",
                    text=q,
                    tokens=parsed.tokens,
                    diagnostics=errors[:4],
                    valid=parsed.normal is not None and not parsed.normal.empty and not errors,
                )
            )

        recent: list[tuple[float, pp.PaletteRecent]] = []
        for e in recents.entries(target):
            got = score(q, e.text) if q else (0, [])
            if got is None:
                continue
            fields: dict[str, object] = {"kind": e.kind, "text": e.text, "hits": spans(e.text, got[1])}
            if e.kind == "jump":
                node = store.find(e.text) if store is not None else None
                if node is None or store is None or sizes is None:
                    fields["exists"] = False
                else:
                    fields |= {
                        "node": node,
                        "is_dir": bool(store.cols["kind"][node] == Kind.DIR),
                        # found after ``sizes`` was taken (scan running): no size yet
                        "alloc": int(sizes[node]) if node < len(sizes) else None,
                    }
            else:
                fields["tokens"] = parse(e.text, None, platform_vocab()).tokens
            recent.append((-got[0], pp.PaletteRecent.model_validate(fields)))
        recent.sort(key=lambda t: t[0])  # stable: newest first among equals

        commands: list[pp.PaletteCommand] = []
        if q:
            ranked = []
            for order, cmd in enumerate(registry()):
                m = match(cmd, q)
                if m is not None:
                    ranked.append((-m[0], order, cmd, m[1]))
            ranked.sort(key=lambda t: (t[0], t[1]))
            chosen = [(cmd, hits) for _, _, cmd, hits in ranked[:COMMANDS_SHOWN]]
        else:
            chosen = [
                (cmd, []) for cmd in sorted((c for c in registry() if c.pinned), key=lambda c: c.pinned)
            ]
        for cmd, hits in chosen:
            enabled, text = command_state(cmd, ctx)
            name = label(cmd)  # M7.5: the current language; ``hits`` refer to it
            commands.append(
                pp.PaletteCommand(
                    id=cmd.id,
                    name=name,
                    hits=spans(name, hits),
                    status=text,
                    enabled=enabled,
                    keys=list(cmd.keys),
                    icon=cmd.icon,
                    more=cmd.more,
                )
            )

        seed = None
        if not q and view is not None and view.selected is not None and 0 < view.selected < len(view.store):
            seed = ppaths.seed_text(view.store, view.selected) or None
        return pp.PaletteResult(
            text=params.text,
            direct=direct,
            recent=[r for _, r in recent[:RECENT_SHOWN]],
            commands=commands,
            paths=handle is not None and bool(q or seed),
            seed=seed,
        )

    @router.method(PALETTE_COMMANDS)
    async def palette_commands(session: Session, _: Empty) -> pp.PaletteCommandList:
        return pp.PaletteCommandList(
            commands=[
                pp.PaletteCommand(id=c.id, name=label(c), keys=list(c.keys), icon=c.icon, more=c.more)
                for c in registry()
                if not c.typed_only
            ]
        )

    @router.method(PALETTE_QUERY)
    async def palette_query(session: Session, params: pp.PaletteQueryParams) -> pp.PaletteResult:
        update = server.updates.status()  # on the loop: the update state is not shared with threads
        return await latest.run(("palette.query", session.id), build, session, params, update)

    # ------------------------------------------------------------------ palette.check

    def check(session: Session, params: pp.PaletteCheckParams, update: UpdateStatus) -> pp.PaletteCommand:
        cmd = next((c for c in registry() if c.id == params.id), None)
        if cmd is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, f"unknown command: {params.id}")
        ctx, _, _ = context_of(session, params.context, update)
        enabled, text = command_state(cmd, ctx)
        return pp.PaletteCommand(
            id=cmd.id,
            name=label(cmd),
            status=text,
            enabled=enabled,
            keys=list(cmd.keys),
            icon=cmd.icon,
            more=cmd.more,
        )

    @router.method(PALETTE_CHECK)
    async def palette_check(session: Session, params: pp.PaletteCheckParams) -> pp.PaletteCommand:
        update = server.updates.status()  # on the loop, as for palette.query
        # off the loop: counting the target's snapshots lists a folder
        return await asyncio.to_thread(check, session, params, update)

    # ------------------------------------------------------------------ palette.paths

    def find_paths(handle: ScanHandle, view: View | None, params: pp.PalettePathsParams) -> pp.PalettePaths:
        store = handle.store
        sizes = sizes_of(handle)
        q = params.text.strip()
        seed = folder = None
        hits: list[ppaths.PathHit] = []
        total = 0
        if not q:
            sel = view.selected if view is not None else None
            if sel is not None and 0 < sel < len(store):
                seed = ppaths.seed_text(store, sel) or None
            if seed is not None:  # literally: a selected POSIX file "what?.txt" is no glob
                hits, total = ppaths.search(
                    store, sizes, seed, params.limit, exclude=sel, contains_only=True, literal=True
                )
        elif ppaths.looks_like_path(q):
            base, partial = ppaths.split_last(ppaths.expand(q))
            node = store.find(base) if base else None
            if node is not None and store.cols["kind"][node] == Kind.DIR:
                folder = store.path(node)
                hits, total = ppaths.children(store, sizes, node, partial, params.limit)
        else:
            hits, total = ppaths.search(store, sizes, q, params.limit)
        return pp.PalettePaths(
            text=params.text,
            items=[
                pp.PalettePath(
                    node=h.node,
                    name=h.name,
                    parent=h.parent,
                    is_dir=h.is_dir,
                    alloc=h.alloc,
                    hits=spans(h.name, h.hits),
                    tier=h.tier,
                    complete=h.complete,
                )
                for h in hits
            ],
            total=total,
            partial=handle.running,
            seed=seed,
            folder=folder,
        )

    @router.method(PALETTE_PATHS)
    async def palette_paths(session: Session, params: pp.PalettePathsParams) -> pp.PalettePaths:
        handle = server.scans.get(params.scan_id)
        return await latest.run(
            ("palette.paths", session.id), find_paths, handle, view_of(session, params.view_id), params
        )

    # ------------------------------------------------------------------ palette.preview

    def preview(handle: ScanHandle, text: str) -> pp.PalettePreview:
        parsed = parse(text, None, platform_vocab())
        normal = parsed.normal
        if normal is None or normal.empty:
            return pp.PalettePreview(text=text, valid=False)
        shared = server.views.shared(handle.id)
        assert shared is not None
        store = handle.store
        key = (handle.id, normal.canonical, store.version, len(store), shared.tags.version)
        cacheable = not normal.ages  # an age condition's result moves with the clock
        with previews_lock:  # previews run in worker threads
            hit = previews.get(key) if cacheable else None
            if hit is not None:
                previews.move_to_end(key)
        if hit is not None:
            return hit.model_copy(update={"text": text})
        res = evaluate(store, normal, FilterContext.current(tags=shared.tags.bits(store)))
        files, alloc, size = res.totals(0)
        out = pp.PalettePreview(text=text, valid=True, files=files, alloc=alloc, size=size)
        if cacheable:
            with previews_lock:
                previews[key] = out
                while len(previews) > 64:
                    previews.popitem(last=False)
        return out

    @router.method(PALETTE_PREVIEW)
    async def palette_preview(session: Session, params: pp.PalettePreviewParams) -> pp.PalettePreview:
        handle = server.scans.get(params.scan_id)
        return await latest.run(("palette.preview", session.id), preview, handle, params.text)

    # ------------------------------------------------------------------ recents

    @router.method(PALETTE_REMEMBER)
    async def palette_remember(session: Session, params: pp.PaletteRememberParams) -> EmptyResult:
        if params.kind == "filter":
            await asyncio.to_thread(recents.add_filter, params.text.strip())
        else:
            target = target_of(server.scans.get(params.scan_id) if params.scan_id else None)
            if target is not None:
                await asyncio.to_thread(recents.add_jump, target, ppaths.trimmed(ppaths.expand(params.text)))
        return EmptyResult()

    @router.method(PALETTE_CLEAR)
    async def palette_clear(session: Session, params: pp.PaletteClearParams) -> EmptyResult:
        target = target_of(handle_of(params.scan_id))
        await asyncio.to_thread(recents.clear, target)
        return EmptyResult()

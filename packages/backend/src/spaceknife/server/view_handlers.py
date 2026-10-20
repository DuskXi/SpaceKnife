"""Handlers for view.*: open/close, navigation, selection, settings, filter and tags; visual.list."""

from __future__ import annotations

from collections.abc import Callable
from typing import TYPE_CHECKING

from spaceknife_protocol import view as pv
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.methods import (
    VIEW_BACK,
    VIEW_CLOSE,
    VIEW_FILTER,
    VIEW_FORWARD,
    VIEW_HOME,
    VIEW_LIST,
    VIEW_OPEN,
    VIEW_RENDER,
    VIEW_RESULTS,
    VIEW_REVEAL,
    VIEW_SELECT,
    VIEW_SET,
    VIEW_TAG,
    VIEW_UP,
    VIEW_ZOOM,
    VISUAL_LIST,
)

from ..i18n import _
from ..layout import registry
from ..view.results import effective_tag, relative_path, sorted_rows
from ..view.state import View, showable
from .compute import Latest, compute
from .views import ViewHost, ViewManager, apply_settings, view_state

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def install(server: Server) -> None:
    router = server.router
    latest = Latest()
    views: ViewManager = server.views

    async def act(
        session: Session, view_id: str, action: Callable[[View], bool], reason: str
    ) -> pv.ViewUpdate:
        """Run a state change; send a frame only when it changed something."""
        host = views.get(session, view_id)
        async with host.lock:
            changed = action(host.view)
            frame = await views.render(host, reason) if changed else None
        return views.update(host, frame, changed)

    @router.method(VIEW_OPEN)
    async def view_open(session: Session, params: pv.ViewOpenParams) -> pv.ViewUpdate:
        host = views.open(session, params)
        async with host.lock:
            frame = await views.render(host, "open")
        return views.update(host, frame)

    @router.method(VIEW_CLOSE)
    async def view_close(session: Session, params: pv.ViewIdParams) -> EmptyResult:
        views.get(session, params.view_id)
        views.close(params.view_id)
        return EmptyResult()

    @router.method(VIEW_LIST)
    async def view_list(session: Session, params: pv.ViewListParams) -> pv.ViewListResult:
        hosts: list[ViewHost] = [
            h
            for h in views.views.values()
            if h.session is session and (params.scan_id is None or h.scan_id == params.scan_id)
        ]
        return pv.ViewListResult(views=[view_state(h) for h in hosts])

    @router.method(VIEW_RENDER)
    async def view_render(session: Session, params: pv.ViewIdParams) -> pv.ViewUpdate:
        return await act(session, params.view_id, lambda v: True, "render")

    @router.method(VIEW_ZOOM)
    async def view_zoom(session: Session, params: pv.ViewZoomParams) -> pv.ViewUpdate:
        return await act(session, params.view_id, lambda v: v.zoom_into(params.key), "zoom")

    @router.method(VIEW_BACK)
    async def view_back(session: Session, params: pv.ViewIdParams) -> pv.ViewUpdate:
        return await act(session, params.view_id, View.go_back, "back")

    @router.method(VIEW_FORWARD)
    async def view_forward(session: Session, params: pv.ViewIdParams) -> pv.ViewUpdate:
        return await act(session, params.view_id, View.go_forward, "forward")

    @router.method(VIEW_UP)
    async def view_up(session: Session, params: pv.ViewIdParams) -> pv.ViewUpdate:
        return await act(session, params.view_id, View.go_up, "up")

    @router.method(VIEW_HOME)
    async def view_home(session: Session, params: pv.ViewIdParams) -> pv.ViewUpdate:
        return await act(session, params.view_id, View.go_home, "home")

    @router.method(VIEW_SELECT)
    async def view_select(session: Session, params: pv.ViewSelectParams) -> pv.ViewUpdate:
        def select(v: View) -> bool:
            if v.selected == params.key:
                return False
            v.select(params.key)
            return True

        return await act(session, params.view_id, select, "select")

    @router.method(VIEW_SET)
    async def view_set(session: Session, params: pv.ViewSetParams) -> pv.ViewUpdate:
        host = views.get(session, params.view_id)
        async with host.lock:
            v = host.view
            before, visual = view_state_tuple(v), v.visual
            apply_settings(v, params.settings, session.capabilities)
            changed = view_state_tuple(v) != before
            # another visualization: the frontend cross-fades instead of re-laying out in place
            reason = "visual" if v.visual != visual else "settings"
            frame = await views.render(host, reason) if changed else None
        return views.update(host, frame, changed)

    @router.method(VISUAL_LIST)
    async def visual_list(session: Session, _params: Empty) -> pv.VisualListResult:
        caps = session.capabilities
        out: list[pv.VisualInfo] = []
        for v in registry.all_visuals():
            available = showable(v.id, caps) is not None
            out.append(
                pv.VisualInfo(
                    id=v.id,
                    layout=v.layout,
                    name=v.title,
                    icon=v.icon,
                    order=v.order,
                    compare=v.compare,
                    free_space=v.free_space,
                    default_color=v.default_color,
                    available=available,
                    reason=None if available else _("这种可视化在当前界面不可用"),
                )
            )
        return pv.VisualListResult(visuals=out)

    @router.method(VIEW_TAG)
    async def view_tag(session: Session, params: pv.ViewTagParams) -> pv.ViewUpdate:
        return await act(session, params.view_id, lambda v: v.tag_selected(params.color), "tag")

    @router.method(VIEW_REVEAL)
    async def view_reveal(session: Session, params: pv.ViewRevealParams) -> pv.ViewUpdate:
        host = views.get(session, params.view_id)
        async with host.lock:
            changed = await compute(host.view.reveal, params.key)
            frame = await views.render(host, "reveal") if changed else None
        return views.update(host, frame, changed)

    @router.method(VIEW_RESULTS)
    async def view_results(session: Session, params: pv.ViewResultsParams) -> pv.ViewResults:
        host = views.get(session, params.view_id)
        async with host.lock:
            view = host.view
            if params.sort is not None:
                view.results_sort = params.sort
            return await latest.run(
                ("view.results", view.id, params.offset, params.limit),
                results_page,
                view,
                host.seq,
                params.offset,
                params.limit,
            )

    @router.method(VIEW_FILTER)
    async def view_filter(session: Session, params: pv.ViewFilterParams) -> pv.ViewUpdate:
        host = views.get(session, params.view_id)
        host.filter_gen += 1
        gen = host.filter_gen
        async with host.lock:
            if gen != host.filter_gen:  # a newer expression is already waiting: skip this one
                return views.update(host, None, changed=False)
            view = host.view
            canon_before = view.filter.normal.canonical if view.filter.normal else None
            result_before = view.filter.result
            await compute(view.set_filter, params.text, params.cursor)
            canon_after = view.filter.normal.canonical if view.filter.normal else None
            changed = canon_after != canon_before or view.filter.result is not result_before
            frame = await views.render(host, "filter") if changed else None
        return views.update(host, frame, changed)


def view_state_tuple(v: View) -> tuple[object, ...]:
    """What ``view.set`` can change: a frame is sent when any of it did."""
    return (
        v.detail,
        v.min_element,
        v.sort_by_size,
        v.show_free,
        v.show_unknown,
        v.visual,
        v.color_mode,
        v.filter_mode,
        v.viewport,
        v.direction,
    )


def results_page(view: View, seq: int, offset: int, limit: int) -> pv.ViewResults:
    rows = sorted_rows(view)
    totals = view.last.filter_totals if view.last is not None else None
    files, alloc, size = totals if totals is not None else (0, 0, 0)
    out: list[pv.ResultRow] = []
    for r in rows[offset : offset + limit]:
        tag, inherited = effective_tag(view, r.node) if r.kind in ("file", "folder") else (None, False)
        out.append(
            pv.ResultRow(
                key=r.key,
                node=r.node,
                kind=r.kind,
                name=r.name,
                count=r.count,
                alloc=r.alloc,
                size=r.size,
                path=relative_path(view, r.parent),
                tag=tag,  # type: ignore[arg-type]
                tag_inherited=inherited,
            )
        )
    return pv.ViewResults(
        view_id=view.id,
        seq=seq,
        active=view.filter.active,
        sort=view.results_sort,
        total=len(rows),
        files=files,
        alloc=alloc,
        size=size,
        max_alloc=max((r.alloc for r in rows), default=0),
        offset=offset,
        rows=out,
    )

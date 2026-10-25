"""Views on the server: ownership, rendering off the event loop, frame encoding, scan-driven pushes.

A view belongs to the session that opened it and dies with it. Rendering and filter evaluation run
in worker threads (numpy releases the GIL for the heavy parts), one at a time per view. Views of a
scan share its tags, layout caches and engines; while the scan runs they are re-rendered at most 4
times a second and their filters re-evaluated at most once a second.

A session gets only the visualizations whose wire layout it negotiated (``showable``): new views and
``view.open`` fall back to the treemap, ``view.set`` refuses with INVALID_PARAMS; the same goes for
the ``branch`` colour mode without ``visuals.v1``.
"""

from __future__ import annotations

import asyncio
import itertools
import time
from collections.abc import Callable, Collection
from dataclasses import asdict, dataclass, field
from typing import TYPE_CHECKING, Any, cast

import numpy as np
from loguru import logger

from spaceknife_protocol import CAP_VISUALS
from spaceknife_protocol import view as pv
from spaceknife_protocol.binary import encode_frame
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EVENT_VIEW_FRAME

from .. import paths
from ..extensions.colors import color_unavailable
from ..i18n import tr
from ..layout import registry
from ..layout.api import Engine
from ..layout.engine import Layout
from ..layout.rows import COMMON_COLUMNS
from ..settings import SettingsStore
from ..store import snapshot
from ..view.state import BASELINE, ColorMode, Engines, Frame, RootInfoLike, View, ViewSettings, showable
from ..view.tags import TagStore
from .compute import compute
from .rpc import CURRENT_REQUEST, RpcError

if TYPE_CHECKING:
    from spaceknife_protocol.scan import ScanStatus

    from ..plugins.host import PluginHost
    from ..store.nodes import NodeStore
    from .compares import ChangeTree, CompareHandle, CompareManager
    from .ops_service import OpsService
    from .scans import ScanHandle, ScanManager
    from .session import Session

PUSH_INTERVAL = 0.25  # s between frames pushed during a scan
REFILTER_INTERVAL = 1.0  # s between filter re-evaluations during a scan
MAX_VIEWS = 64


@dataclass(slots=True)
class ViewHost:
    view: View
    scan_id: str
    session: Session
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)
    seq: int = 0
    filter_gen: int = 0
    last_push: float = 0.0
    last_refilter: float = 0.0
    push_task: asyncio.Task[None] | None = None
    push_final: bool = False  # the running push renders the scan's final frame
    final_pending: bool = False  # the scan ended while a progress push was rendering
    change_pending: bool = False  # file changes arrived while a push was rendering (M5)


@dataclass(slots=True)
class _Shared:
    tags: TagStore
    basis: Layout  # sizes and colour classes of the scan's store
    listener: str
    engines: Engines = field(init=False)  # one engine per visualization over ``basis``

    def __post_init__(self) -> None:
        self.engines = Engines(self.basis)

    @property
    def layout(self) -> Layout:
        """The older name of :attr:`basis`."""
        return self.basis

    def engine_for(self, visual: str) -> Engine:
        return self.engines.get(visual)

    def rebuild(self, basis: Layout) -> Engines:
        """The store was replaced: new caches and engines (a view keeps its own until it moves)."""
        self.basis = basis
        self.engines = Engines(basis)
        return self.engines


class ViewManager:
    def __init__(self, scans: ScanManager) -> None:
        self.scans = scans
        self.views: dict[str, ViewHost] = {}
        self._shared: dict[str, _Shared] = {}
        self._ids = itertools.count(1)
        self.settings: SettingsStore | None = None  # set by the server (W7 defaults and live changes)
        self.compares: CompareManager | None = None  # set by CompareManager: compare views (W6)
        self.ops: OpsService | None = None  # set by the server: the basket follows tree changes (W5)
        self.plugins: PluginHost | None = None  # set by the server: node sets of :set: filters
        scans.in_use = self.has_views  # MAX_SCANS eviction must not pull a scan from under its tabs
        scans.on_evict = self._release

    # ------------------------------------------------------------- lifecycle

    def _scan(self, scan_id: str) -> ScanHandle:
        handle: Any = self.scans.scans.get(scan_id)
        if handle is None and self.compares is not None:
            handle = self.compares.lookup(scan_id)  # a ready comparison opens like a scan
        if handle is None:
            raise RpcError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {scan_id}")
        return cast("ScanHandle", handle)

    def _shared_for(self, handle: ScanHandle) -> _Shared:
        shared = self._shared.get(handle.id)
        cmp = _compare_of(handle)
        if shared is None and cmp is not None:
            # a comparison shows the tags of its scan (same roots: tags address root + path)
            own = self.shared(cmp.scan_id)
            shared = _Shared(own.tags if own else TagStore(None), Layout(handle.store), f"views-{handle.id}")
            self._shared[handle.id] = shared
        if shared is None:
            remember = self.settings is None or bool(self.settings.get("tags.remember"))
            file = (
                paths.data_dir() / "tags" / (snapshot.root_key([r.path for r in handle.roots]) + ".json")
                if remember
                else None
            )
            listener = f"views-{handle.id}"
            shared = _Shared(TagStore(file), Layout(handle.store), listener)
            scan_id = handle.id

            def on_status(status: ScanStatus) -> None:
                self._on_scan(scan_id, status.state)

            handle.listeners[listener] = on_status
            self._shared[handle.id] = shared
        return shared

    def shared(self, scan_id: str) -> _Shared | None:
        """Tags and layout cache of a scan (also used by tree.summary)."""
        handle = self.scans.scans.get(scan_id)
        return self._shared_for(handle) if handle is not None else None

    async def replace_store(self, compare_id: str, old: NodeStore, tree: ChangeTree) -> None:
        """A live comparison was computed again: its views move to the new change tree."""
        shared = self._shared.get(compare_id)
        layout = Layout(tree.store)
        engines = shared.rebuild(layout) if shared is not None else Engines(layout)
        roots = [RootInfoLike(node=r, whole_volume=False) for r in tree.store.roots()]
        for host in self._hosts(compare_id):
            async with host.lock:  # a render still running keeps the old store's engines
                if host.view.store is old:
                    host.view.rebase(tree.store, tree, layout, roots, engines.get)
            await self._repaint(host, "change")

    def open(self, session: Session, params: pv.ViewOpenParams) -> ViewHost:
        if len(self.views) >= MAX_VIEWS:
            raise RpcError(ErrorCode.INVALID_REQUEST, tr("打开的视图太多了，先关掉一些标签或窗格"))
        handle = self._scan(params.scan_id)
        shared = self._shared_for(handle)
        roots = _volume_roots(handle)
        view_id = f"view-{next(self._ids)}"
        caps = session.capabilities
        cmp = _compare_of(handle)
        view = View(
            view_id,
            handle.store,
            roots,
            tags=shared.tags,
            layout=shared.basis,
            settings=self.new_view_settings(caps, compare=cmp is not None),
            engines=shared.engines.get,
        )
        view.set_viewport(params.viewport.width, params.viewport.height, params.viewport.dpr)
        view.scanning = bool(getattr(handle, "running", False)) and self._marks_scanning()
        if self.plugins is not None:
            plugins, scan_id, store = self.plugins, handle.id, handle.store
            view.sets_for = lambda nf: plugins.set_masks(scan_id, store, [*nf.sets, *nf.not_sets])
        if cmp is not None:
            view.compare = cmp.tree  # before the settings: the direction only applies to compare views
        if params.settings is not None:
            apply_settings(view, fall_back(params.settings, caps, compare=cmp is not None), caps)
        host = ViewHost(view, handle.id, session)
        self.views[view_id] = host
        return host

    def has_views(self, scan_id: str) -> bool:
        return any(h.scan_id == scan_id for h in self.views.values())

    def get(self, session: Session, view_id: str) -> ViewHost:
        host = self.views.get(view_id)
        if host is None or host.session is not session:
            raise RpcError(ErrorCode.UNKNOWN_VIEW, f"unknown view: {view_id}")
        return host

    def close(self, view_id: str) -> None:
        host = self.views.pop(view_id, None)
        if host is None:
            return
        if host.push_task is not None:
            host.push_task.cancel()
        if not any(h.scan_id == host.scan_id for h in self.views.values()):
            self._release(host.scan_id)

    def _release(self, scan_id: str) -> None:
        shared = self._shared.pop(scan_id, None)
        handle = self.scans.scans.get(scan_id)
        if shared is not None and handle is not None:
            handle.listeners.pop(shared.listener, None)

    def close_session(self, session: Session) -> None:
        for view_id in [v for v, h in self.views.items() if h.session is session]:
            self.close(view_id)

    def close_scan(self, scan_id: str) -> None:
        for view_id in [v for v, h in self.views.items() if h.scan_id == scan_id]:
            self.close(view_id)
        self._release(scan_id)

    def new_view_settings(self, caps: Collection[str], *, compare: bool = False) -> ViewSettings:
        """Defaults of a new view from the settings (W7 树图 · 外观 · 可视化) for a session that
        announced ``caps``: a visualization it can't show falls back to the treemap, the ``branch``
        colour mode without ``visuals.v1`` to class colouring."""
        st = self.settings
        if st is None:
            return ViewSettings()
        full = CAP_VISUALS in caps

        def mode(key: str) -> ColorMode:
            value = st.get(key)
            return value if value != "branch" or full else "class"  # type: ignore[return-value]

        # treemaps start with 「树图默认配色」, flame graphs with 「火焰图和火焰-极坐标图默认配色」,
        # visualizations with a layout of their own with their default
        starts = {pv.DRAWLIST_LAYOUT: mode("appearance.default_color_mode")}
        starts |= dict.fromkeys((pv.FLAME_LAYOUT, pv.POLAR_LAYOUT), mode("appearance.partition_color_mode"))
        colors = tuple(
            (v.id, starts[v.layout])
            for v in registry.all_visuals()
            if v.layout in starts and v.plugin is None
        )
        vis = showable(str(st.get("appearance.default_visual")), caps)
        visual = vis.id if vis is not None and (vis.compare or not compare) else BASELINE
        return ViewSettings(
            detail=int(st.get("treemap.initial_detail")),
            min_element=float(st.get("treemap.min_element")),
            sort_by_size=bool(st.get("treemap.sort_by_size")),
            show_free=bool(st.get("treemap.show_free")),
            show_unknown=bool(st.get("treemap.show_unknown")),
            show_trash=bool(st.get("treemap.show_trash")),
            visual=visual,
            color_mode=dict(colors).get(visual, registry.get(visual).default_color),
            colors=colors,
            font_scale=int(st.get("treemap.font_scale")) / 100,
            aspect=aspect_of(int(st.get("treemap.aspect"))),
            options=visual_options(st),
        )

    def apply_settings_live(self, st: SettingsStore) -> None:
        """Geometry settings re-lay out every open view at once (UI-W7-32); the flame graph's
        direction the views showing a flame graph."""
        min_element = float(st.get("treemap.min_element"))
        sort_by_size = bool(st.get("treemap.sort_by_size"))
        font_scale = int(st.get("treemap.font_scale")) / 100
        aspect = aspect_of(int(st.get("treemap.aspect")))
        geometry = (min_element, sort_by_size, font_scale, aspect)
        options = dict(visual_options(st))
        for host in list(self.views.values()):
            v = host.view
            changed = (v.min_element, v.sort_by_size, v.font_scale, v.aspect) != geometry
            v.min_element, v.sort_by_size, v.font_scale, v.aspect = geometry
            for vis, opts in options.items():
                if v.visual_options.get(vis) != opts:
                    v.visual_options[vis] = opts
                    changed = changed or v.visual == vis
            if changed:
                asyncio.get_running_loop().create_task(self._repaint(host, "settings"))

    async def forget_tags(self) -> None:
        """「清除所有已保存的标签」: every scan's tags go; the views redraw without them."""
        for shared in self._shared.values():
            shared.tags.forget()
        for host in list(self.views.values()):
            async with host.lock:
                if host.view.filter.active:
                    await compute(host.view.refilter)
            await self._repaint(host, "tags")

    async def reclassify(self) -> None:
        """The file class table changed: every view re-runs its filter (``:class:``) and recolours."""
        for host in list(self.views.values()):
            async with host.lock:
                if host.view.filter.active:
                    await compute(host.view.refilter)
            await self._repaint(host, "classes")

    async def plugin_changed(self, plugin: str) -> None:
        """A plugin's cached answers went (its settings changed, ``ctx.invalidate``): views that
        filter by its node sets filter again; those showing its visualization or colours draw again."""
        prefix = plugin + "."
        for host in list(self.views.values()):
            v = host.view
            nf = v.filter.normal
            uses_set = nf is not None and any(s.startswith(prefix) for s in (*nf.sets, *nf.not_sets))
            shows = v.visual.startswith(prefix) or str(v.color_mode).startswith(prefix)
            if not (uses_set or shows):
                continue
            if uses_set:
                async with host.lock:
                    await compute(v.refilter)
            await self._repaint(host, "plugin")

    async def repaint_color(self, mode: str) -> None:
        """Views painted in a plugin's colour mode draw again (its answer came late)."""
        for host in list(self.views.values()):
            if str(host.view.color_mode) == mode:
                await self._repaint(host, "plugin")

    async def _repaint(self, host: ViewHost, reason: str) -> None:
        try:
            async with host.lock:
                info = await self.render(host, reason)
            host.session.emit(EVENT_VIEW_FRAME, pv.ViewFrameEvent(frame=info))
        except Exception:
            logger.exception("repainting {} failed", host.view.id)

    # ------------------------------------------------------------- rendering

    async def render(self, host: ViewHost, reason: str) -> pv.FrameInfo:
        """Render in a worker thread and queue the binary frame. Call with ``host.lock`` held."""
        frame = await compute(host.view.render)
        host.seq += 1
        info = frame_info(host, frame, reason)
        host.session.send_binary(encode_draw(frame, host.view.id, host.seq, CURRENT_REQUEST.get()))
        return info

    def update(self, host: ViewHost, frame: pv.FrameInfo | None, changed: bool = True) -> pv.ViewUpdate:
        return pv.ViewUpdate(
            state=view_state(host),
            changed=changed,
            frame=frame,
            filter=filter_info(host.view, None),
        )

    # ------------------------------------------------------------- file changes (M5)

    def _hosts(self, scan_id: str) -> list[ViewHost]:
        return [h for h in self.views.values() if h.scan_id == scan_id]

    def chains(self, scan_id: str) -> dict[str, list[int]]:
        """Each view's zoom node with its ancestors, taken before a change: a node that goes
        loses its parent, so the nearest one still there is found from this list afterwards."""
        out: dict[str, list[int]] = {}
        for host in self._hosts(scan_id):
            store, z = host.view.store, host.view.zoom.node
            out[host.view.id] = [z, *store.ancestors(z)] if 0 <= z < len(store) else []
        return out

    def remap(
        self, scan_id: str, chains: dict[str, list[int]], new_ids: Callable[[np.ndarray], np.ndarray]
    ) -> None:
        """Node ids changed: every view of the scan follows (zoom, selection, history, flashes)."""
        for host in self._hosts(scan_id):
            chain = chains.get(host.view.id, [])
            moved = new_ids(np.array(chain, dtype=np.int64)) if chain else np.empty(0, dtype=np.int64)
            fallback = next((int(m) for m in moved.tolist() if m >= 0), host.view.home)
            host.view.remap(new_ids, fallback)
        if self.plugins is not None:  # the folders plugins have not heard of yet (tree_changed)
            self.plugins.live.tree_remapped(scan_id, new_ids)

    def flash(self, scan_id: str, changes: dict[int, int]) -> None:
        """The tree changed: views flash what changed (setting 「元素变化闪烁」) and get a new frame."""
        if self.compares is not None:
            self.compares.on_tree_changed(scan_id)  # live comparisons follow (UI-W6-38)
        if self.ops is not None:
            self.ops.on_tree_changed(scan_id)  # basket sizes and missing items (UI-W5-48)
        if self.plugins is not None:
            self.plugins.live.tree_changed(scan_id, changes)  # the plugins' tree_changed hooks
        st = self.settings
        flash_on = st is None or bool(st.get("treemap.flash_changes"))
        self._follow_volumes(scan_id)
        for host in self._hosts(scan_id):
            if flash_on and changes:
                host.view.add_flash(changes)
            self._push_change(host)

    def volumes_changed(self, scan_id: str) -> None:
        """The free space of the scan's volumes moved without a change to the tree (read again a
        moment after the last change): free and unknown space are drawn again."""
        self._follow_volumes(scan_id)
        for host in self._hosts(scan_id):
            self._push_change(host)

    def _follow_volumes(self, scan_id: str) -> None:
        """The views' free and used space of each root: the volume as the scan last read it
        (``ScanMonitor._refresh_free``), not as it was when the view opened. Otherwise whatever
        is deleted while monitoring leaves the tree but not the used space, and shows as unknown."""
        handle = self.scans.scans.get(scan_id)
        if handle is None:
            return
        roots = _volume_roots(handle)
        for host in self._hosts(scan_id):
            if host.view.store is handle.store:
                host.view.roots = roots

    def _push_change(self, host: ViewHost) -> None:
        if host.push_task is not None and not host.push_task.done():
            host.change_pending = True  # the running push renders once more
            return
        host.push_final = False
        host.push_task = asyncio.get_running_loop().create_task(self._push(host, False, "change"))

    # ------------------------------------------------------------- scan-driven pushes

    def _marks_scanning(self) -> bool:
        """Folders still being scanned breathe (UI-TM-42): the 「元素变化闪烁」 setting covers it."""
        return self.settings is None or bool(self.settings.get("treemap.flash_changes"))

    def _on_scan(self, scan_id: str, state: str) -> None:
        final = state in ("done", "cancelled", "failed")
        marks = state == "scanning" and self._marks_scanning()
        for host in self.views.values():
            if host.scan_id != scan_id:
                continue
            host.view.scanning = marks
            if host.push_task is not None and not host.push_task.done():
                if final and not host.push_final:
                    host.final_pending = True  # the running push renders the final frame next
                continue
            if not final and time.monotonic() - host.last_push < PUSH_INTERVAL:
                continue
            host.push_final = final
            host.push_task = asyncio.get_running_loop().create_task(self._push(host, final))

    async def _push(self, host: ViewHost, final: bool, reason: str = "") -> None:
        while True:
            try:
                async with host.lock:
                    now = time.monotonic()
                    view = host.view
                    if view.filter.active and (final or now - host.last_refilter >= REFILTER_INTERVAL):
                        await compute(view.refilter)
                        host.last_refilter = now
                    info = await self.render(host, reason or ("scan-final" if final else "scan"))
                    host.last_push = time.monotonic()
                host.session.emit(EVENT_VIEW_FRAME, pv.ViewFrameEvent(frame=info))
            except Exception:
                logger.exception("pushing a frame of {} failed", host.view.id)
            if host.final_pending:
                host.final_pending = False
                host.push_final = final = True
                reason = ""
            elif host.change_pending:
                host.change_pending = False
                reason = "change"
            else:
                return


def _volume_roots(handle: ScanHandle) -> list[RootInfoLike]:
    """What views need of the scan's roots, with the free and used space of their volumes."""
    return [
        RootInfoLike(
            node=r.node,
            whole_volume=r.whole_volume,
            free=r.volume.free if r.volume else None,
            used=r.volume.used if r.volume else None,
        )
        for r in handle.roots
    ]


def _compare_of(handle: object) -> CompareHandle | None:
    """The comparison behind a scan-like handle (views open both)."""
    from .compares import CompareHandle

    return handle if isinstance(handle, CompareHandle) else None


def aspect_of(setting: int) -> float:
    """「元素比例」 −60…60 → the width / height cells aim for: 0.5 (taller) … 1 (square) … 2 (wider)."""
    return float(2 ** (setting / 60))


def visual_options(st: SettingsStore | None) -> tuple[tuple[str, tuple[tuple[str, object], ...]], ...]:
    """Each visualization's own options from the settings they map to (``Visualization.options``)."""
    if st is None:
        return ()
    out = []
    for v in registry.all_visuals():
        if v.options:
            out.append((v.id, tuple((name, st.get(key)) for name, key in v.options if st.has(key))))
    return tuple(out)


def fall_back(s: pv.ViewSettingsParams, caps: Collection[str], *, compare: bool) -> pv.ViewSettingsParams:
    """``view.open``: a visualization the session can't show becomes the treemap; the ``branch``
    colour mode without ``visuals.v1`` is left out (the view keeps its default)."""
    update: dict[str, object] = {}
    if s.visual is not None:
        vis = showable(s.visual, caps)
        if vis is None or (compare and not vis.compare):
            update["visual"] = BASELINE
    if s.color_mode == "branch" and CAP_VISUALS not in caps:
        update["color_mode"] = None
    if s.color_mode is not None and color_unavailable(s.color_mode, caps) is not None:
        update["color_mode"] = None
    if s.colors and (
        CAP_VISUALS not in caps or any(color_unavailable(m, caps) is not None for m in s.colors.values())
    ):
        update["colors"] = {
            vis: mode
            for vis, mode in s.colors.items()
            if (mode != "branch" or CAP_VISUALS in caps) and color_unavailable(mode, caps) is None
        }
    return s.model_copy(update=update) if update else s


def apply_settings(view: View, s: pv.ViewSettingsParams, caps: Collection[str]) -> None:
    """Apply what ``s`` sets. A visualization or colour mode the session that announced ``caps``
    can't show is INVALID_PARAMS, before anything changes. The visualization changes before the
    colour mode, which belongs to it."""
    if s.visual is not None:
        vis = showable(s.visual, caps)
        if vis is None or (view.compare is not None and not vis.compare):
            raise RpcError(ErrorCode.INVALID_PARAMS, tr("这种可视化在当前界面不可用"))
    if CAP_VISUALS not in caps and (
        s.color_mode == "branch" or (s.colors is not None and "branch" in s.colors.values())
    ):
        raise RpcError(ErrorCode.INVALID_PARAMS, tr("这种配色在当前界面不可用"))
    for mode in [s.color_mode, *(s.colors or {}).values()]:
        reason = color_unavailable(mode, caps) if mode is not None else None
        if reason is not None:
            raise RpcError(ErrorCode.INVALID_PARAMS, reason)
    if s.detail is not None:
        view.set_detail(s.detail)
    if s.min_element is not None:
        view.min_element = s.min_element
    if s.sort_by_size is not None:
        view.sort_by_size = s.sort_by_size
    if s.show_free is not None:
        view.show_free = s.show_free
    if s.show_unknown is not None:
        view.show_unknown = s.show_unknown
    if s.show_trash is not None:
        view.show_trash = s.show_trash
    if s.colors is not None:
        view.set_colors(s.colors)
    if s.visual is not None:
        view.set_visual(s.visual)
    if s.color_mode is not None:
        view.color_mode = s.color_mode
    if s.filter_mode is not None:
        view.filter_mode = s.filter_mode
    if s.viewport is not None:
        view.set_viewport(s.viewport.width, s.viewport.height, s.viewport.dpr)
    if s.direction is not None and view.compare is not None:
        view.direction = s.direction


def view_state(host: ViewHost) -> pv.ViewState:
    v = host.view
    w, h, dpr = v.viewport
    return pv.ViewState(
        view_id=v.id,
        scan_id=host.scan_id,
        zoom_node=v.zoom.node,
        zoom_key=v.zoom.key,
        detail=v.detail,
        min_element=v.min_element,
        sort_by_size=v.sort_by_size,
        show_free=v.show_free,
        show_unknown=v.show_unknown,
        show_trash=v.show_trash,
        has_trash=v.has_trash,
        visual=v.visual,
        color_mode=v.color_mode,
        colors=v.colors,
        filter_mode=v.filter_mode,
        viewport=pv.Viewport(width=w, height=h, dpr=dpr),
        selected=v.selected,
        results_sort=v.results_sort,
        compare=v.compare is not None,
        direction=v.direction,  # type: ignore[arg-type]
        can_back=bool(v.back),
        can_forward=bool(v.forward),
        can_up=v.can_go_up(),
    )


def filter_info(view: View, frame: Frame | None) -> pv.FilterInfo | None:
    f = view.filter
    if not f.text and not f.active:
        return None
    files = alloc = size = 0
    if frame is not None and frame.filter_totals is not None:
        files, alloc, size = frame.filter_totals
    elif f.result is not None and view.zoom.members is None and view.zoom.node < f.result.n:
        files, alloc, size = f.result.totals(view.zoom.node)
    return pv.FilterInfo(
        text=f.text,
        active=f.active,
        stale=f.stale,
        mode=view.filter_mode,
        files=files,
        alloc=alloc,
        size=size,
        diagnostics=f.diagnostics[:8],
    )


def frame_info(host: ViewHost, frame: Frame, reason: str) -> pv.FrameInfo:
    d = frame.draw
    # the zoom root is drawn too: the root bar, the centre disc, a plugin layout's root shape
    drawn = d.layout != pv.DRAWLIST_LAYOUT and (
        not isinstance(d.meta, pv.ShapesGeometry) or d.meta.root is not None
    )
    return pv.FrameInfo(
        view_id=host.view.id,
        seq=host.seq,
        width=d.width,
        height=d.height,
        dpr=host.view.viewport[2],
        rows=len(d),
        selected_row=frame.selected_row,
        breadcrumb=[pv.Crumb(key=k, label=label) for k, label in frame.breadcrumb],
        scanned=frame.scanned_total,
        filter=filter_info(host.view, frame),
        layout_ms=round(d.elapsed_s * 1000, 2),
        reason=reason,
        flash=[pv.FlashItem(row=row, delta=delta) for row, delta in frame.flash],
        visual=d.visual,
        layout=d.layout,
        geometry=d.meta,
        root=pv.FrameRoot(**asdict(frame.root)) if drawn else None,
        palette=frame.palette if frame.pal is not None else None,
        size_unit=frame.size_unit,  # type: ignore[arg-type]
        total=max(0, int(frame.root.size)),
    )


def encode_draw(frame: Frame, view_id: str, seq: int, request_id: int | str | None) -> bytes:
    """The binary frame: the common columns, the layout's geometry columns, the frame's own columns
    and the names blob (docs/protocol.md §4)."""
    d = frame.draw
    encoded = [name.encode("utf-8", "replace") for name in d.names]
    columns: dict[str, np.ndarray] = {name: getattr(d, name) for name, _ in COMMON_COLUMNS}
    columns |= d.geometry
    columns |= {
        "flags": frame.flags,
        "tag": frame.tag,
        "tone": frame.tone,
        "branch": frame.branch,
        "name_len": np.fromiter((len(b) for b in encoded), dtype=np.uint32, count=len(encoded)),
    }
    if frame.delta is not None and frame.dstat is not None:  # compare views (W6)
        columns["delta"] = frame.delta
        columns["dstat"] = frame.dstat
    if frame.pal is not None:  # colors.v1: a plugin colour mode's palette index per row
        columns["pal"] = frame.pal
    blobs = {"names": b"".join(encoded)}
    if d.points is not None:  # shapes.v1: the polygons' points
        blobs["points"] = np.ascontiguousarray(d.points, dtype="<i4").tobytes()
    return encode_frame(
        stream=pv.view_stream(view_id),
        layout=d.layout,
        columns=columns,
        seq=seq,
        request_id=request_id,
        blobs=blobs,
    )

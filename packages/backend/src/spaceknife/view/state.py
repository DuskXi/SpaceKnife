"""View state machine: one tab or split pane looking at one scan (UI-TM-33…38, UI-TM-55, W3, W8).

Each view owns its zoom root, back/forward history (every entry remembers its selection),
detail level, viewport, free/unknown switches, visualization, colour mode (one per visualization),
filter and selection. Views of the same scan share the node store, the tags and the layout caches;
rendering returns a :class:`Frame` — the draw list plus per-block flags, tag, time tone, branch
colour, the zoom root, the selected row and the breadcrumb — that the protocol layer turns into one
binary frame.

The visualization is a registry id (``layout.registry``); switching keeps the zoom root, history,
selection, filter and detail level, only the engine that lays out the next frame changes.
"""

from __future__ import annotations

import threading
from collections.abc import Callable, Collection, Mapping
from dataclasses import dataclass, field, replace
from typing import Any, Literal

import numpy as np

from spaceknife_protocol import CAP_VISUALS
from spaceknife_protocol.filter import FilterDiagnostic, FilterNormal, FilterVocab
from spaceknife_protocol.filter_syntax import parse
from spaceknife_protocol.view import DRAWLIST_LAYOUT

from ..filter.engine import FilterContext, FilterResult, evaluate
from ..filter.vocab import platform_vocab
from ..layout import registry
from ..layout.api import ArraySizes, Engine, SizeSource, StoreSizes, Visualization
from ..layout.basis import Basis
from ..layout.engine import (
    KEY_MORE,
    KEY_NODE_MASK,
    KEY_SMALL,
    DrawList,
    Layout,
    LayoutParams,
    RectKind,
    RootSpec,
)
from ..store.nodes import Flag, Kind, NodeStore
from .branch import BranchHues
from .tags import TagStore
from .trash import WithoutBins, find_bins, trash_marks

ColorMode = Literal["class", "flat", "time", "branch", "dupes"]
FilterMode = Literal["dim", "hide"]
ResultsSort = Literal["size", "name", "count"]
DETAIL_MIN, DETAIL_MAX = 1, 15
HISTORY = 200  # back steps kept per view
MAX_FLASH = 200  # changes flashed per frame (the largest)
SCANNING_LEVELS = 3  # levels below the view root that breathe while their folder is being scanned

# Per-block flags of a rendered frame.
F_MATCH = 1  # filter active: this file matches / this block contains matches
F_DIM = 2  # filter active and nothing here matches (dim mode)
F_FOLDER_HIT = 4  # folder named by a positive folder mask (bold title)
F_TAGGED = 8  # carries its own tag (see the tag column)
F_SELECTED = 16  # the selection, or its nearest visible ancestor
F_ON_PATH = 32  # an ancestor of the selection (halo header)
F_SCANNING = 64  # scan running and this folder's subtree is not fully listed yet (top levels only)
F_TRASH = 128  # in a recycle bin / trash, or the bin itself (view/trash.py)
F_TRASH_BIN = 256  # the bin folder itself (its badge: what emptying it frees)

DAY = 86400
TONE_LIMITS = (DAY, 7 * DAY, 30 * DAY, 365 * DAY)  # time colouring: < 1 d, < 1 w, < 1 mo, < 1 y, older
_TAG_BITS = 3 << 30  # the tag part of a key (KEY_SMALL / KEY_MORE / FREE and UNKNOWN)
BASELINE = "treemap"  # the visualization every session can show (drawlist.v1, no capability)


def showable(visual: str, caps: Collection[str]) -> Visualization | None:
    """The visualization ``visual`` if a session that announced ``caps`` can show it: registered, its
    capability announced, and without ``visuals.v1`` only the baseline treemap."""
    try:
        vis = registry.get(visual)
    except KeyError:
        return None
    if CAP_VISUALS not in caps:
        return vis if vis.id == BASELINE else None
    return vis if registry.is_available(vis, caps) else None


@dataclass(frozen=True, slots=True)
class RootInfoLike:
    """What a view needs to know about a scan root (``server.scans.RootInfo`` fits)."""

    node: int
    whole_volume: bool
    free: int | None = None
    used: int | None = None


@dataclass(frozen=True, slots=True)
class Zoom:
    node: int  # folder or file filling the view; for aggregates the container
    key: int  # the block that was zoomed into (node id or SMALL/MORE key)
    members: np.ndarray | None = None
    count: int = 0  # aggregate size for the breadcrumb ("18,406 small items")
    parent: Zoom | None = None  # the aggregate zoom of the same container this one was opened in


@dataclass(slots=True)
class Entry:
    zoom: Zoom
    selected: int | None


@dataclass(frozen=True, slots=True)
class ViewSettings:
    detail: int = 5
    min_element: float = 14.0
    sort_by_size: bool = True
    show_free: bool = True
    show_unknown: bool = True
    show_trash: bool = True  # the recycle bins (「回收站」 chip)
    visual: str = "treemap"  # a registered visualization
    color_mode: ColorMode = "class"  # of ``visual``, the one the view starts with
    colors: tuple[tuple[str, ColorMode], ...] = ()  # the colour mode each other visualization starts with
    filter_mode: FilterMode = "dim"
    font_scale: float = 1.0
    aspect: float = 1.0
    flame_origin: str = "bottom"  # flame graph: the root bar at the "top" or the "bottom"


class Engines:
    """One engine per visualization over one basis, built when first asked for (views of a scan
    share it; a view on its own builds its own)."""

    __slots__ = ("_built", "basis")

    def __init__(self, basis: Basis) -> None:
        self.basis = basis
        self._built: dict[str, Engine] = {}

    def get(self, visual: str) -> Engine:
        """The engine of ``visual`` (KeyError: no such visualization)."""
        engine = self._built.get(visual)
        if engine is None:  # two renders racing build one each: one of them is kept
            engine = self._built.setdefault(visual, registry.get(visual).engine(self.basis))
        return engine


@dataclass(frozen=True, slots=True)
class RootRow:
    """The zoom root as the facts of a row (``FrameInfo.root``: the root bar, the centre disc)."""

    key: int
    node: int
    kind: int  # RectKind: FOLDER, PENDING, FILE, or SMALL / MORE for an aggregate zoom
    size: int  # bytes on this frame's basis (matched bytes in hide mode, changes in a compare view)
    count: int = 0  # members of an aggregate zoom
    flags: int = 0  # F_SCANNING, F_MATCH / F_DIM, F_FOLDER_HIT, F_TAGGED, F_TRASH
    tag: int = 0
    tone: int = 4
    branch: int = 0
    delta: int | None = None  # compare views
    dstat: int | None = None


@dataclass(slots=True)
class FilterState:
    text: str = ""
    normal: FilterNormal | None = None  # last valid, non-empty normal form (None: no filtering)
    result: FilterResult | None = None
    diagnostics: list[FilterDiagnostic] = field(default_factory=list)
    stale: bool = False  # the text has errors: ``normal``/``result`` belong to an earlier text

    @property
    def active(self) -> bool:
        return self.normal is not None


@dataclass(slots=True)
class Frame:
    draw: DrawList
    flags: np.ndarray  # u2 F_* bits per row
    tag: np.ndarray  # u1 own tag per row: 0 none, 1–4 red, yellow, green, blue
    tone: np.ndarray  # u1 time-colouring band per row (0 newest … 4 oldest)
    branch: np.ndarray  # u1 top-level branch colour slot per row (0 neutral, 1–8)
    root: RootRow  # the zoom root itself
    selected_row: int | None
    breadcrumb: list[tuple[int, str]]  # (key, label) from the home root to the zoom root
    filter_totals: tuple[int, int, int] | None  # (files, allocated, logical) below the zoom root
    scanned_total: int  # allocated bytes below the zoom root
    flash: list[tuple[int, int]] = field(default_factory=list)  # (row, allocation change): file changes
    delta: np.ndarray | None = None  # i8 compare views: signed net change per row (W6)
    dstat: np.ndarray | None = None  # u1 compare views: 0 unchanged, 1 new, 2 deleted, 3 changed
    dup: np.ndarray | None = None  # u1 dupes colouring: file state / folder tier (docs/dupes.md §10.2)
    dupg: np.ndarray | None = None  # u4 dupes colouring: group id + 1


class View:
    def __init__(
        self,
        view_id: str,
        store: NodeStore,
        roots: list[RootInfoLike],
        *,
        tags: TagStore | None = None,
        layout: Layout | None = None,
        settings: ViewSettings | None = None,
        vocab: FilterVocab | None = None,
        engines: Callable[[str], Engine] | None = None,
    ) -> None:
        self.id = view_id
        self.store = store
        self.roots = roots
        self.tags = tags or TagStore()
        self.layout = layout or Layout(store)
        self.basis: Basis = self.layout  # what the engines lay out (``layout``: older name)
        self._engines = engines or Engines(self.basis).get  # visualization id -> its engine
        s = settings or ViewSettings()
        registry.get(s.visual)  # KeyError: not a registered visualization
        self.visual = s.visual
        self._colors: dict[str, ColorMode] = dict(s.colors)  # visualization -> its colour mode
        # the dupes colouring's frame columns: set by the server while results cover this scan
        self.dupes_annotate: Callable[[NodeStore, Any], tuple[np.ndarray, np.ndarray] | None] | None = None
        self._colors[s.visual] = s.color_mode
        self.visual_options: dict[str, tuple[tuple[str, object], ...]] = {  # LayoutParams.options
            "flame": (("origin", s.flame_origin),),
        }
        self.hues = BranchHues()
        self.detail = s.detail
        self.min_element = s.min_element
        self.sort_by_size = s.sort_by_size
        self.show_free = s.show_free
        self.show_unknown = s.show_unknown
        self.show_trash = s.show_trash
        self.font_scale = s.font_scale
        self.aspect = s.aspect
        self.filter_mode: FilterMode = s.filter_mode
        self._vocab = vocab  # None: the platform vocabulary with the current class table
        self.viewport = (1280.0, 800.0, 1.0)
        self.home = roots[0].node if len(roots) == 1 else 0
        self.zoom = Zoom(self.home, self.home)
        self.back: list[Entry] = []
        self.forward: list[Entry] = []
        self.selected: int | None = None  # a node id, or a SMALL/MORE key
        self.filter = FilterState()
        self.results_sort: ResultsSort = "size"  # W3 result list order, remembered per view
        self.last: Frame | None = None
        self.compare: Any = None  # server.compares.ChangeTree: areas are changes (W6)
        self.direction: str = "all"  # compare views: 「显示的变化」 all / grow / shrink
        self.scanning = False  # its scan is running: frames mark the folders still being listed
        self._flashes: dict[int, int] = {}  # node -> allocation change, shown with the next frame (M5)
        self._flash_lock = threading.Lock()  # added on the event loop, taken by renders on the pool

    # ------------------------------------------------------------- visualization

    @property
    def color_mode(self) -> ColorMode:
        """Colour mode of the current visualization (each one remembers its own)."""
        mode = self._colors.get(self.visual)
        return mode if mode is not None else registry.get(self.visual).default_color

    @color_mode.setter
    def color_mode(self, mode: ColorMode) -> None:
        self._colors[self.visual] = mode

    @property
    def colors(self) -> dict[str, ColorMode]:
        """The colour modes set so far, per visualization (``view.state``; split and reconnect)."""
        return dict(self._colors)

    def set_colors(self, colors: Mapping[str, ColorMode]) -> None:
        """Take over another view's colour modes; ids of unknown visualizations are left out."""
        known = {v.id for v in registry.all_visuals()}
        self._colors.update({vis: mode for vis, mode in colors.items() if vis in known})

    def set_visual(self, visual: str) -> bool:
        """Show another visualization (KeyError: not registered). Zoom root, history, selection and
        filter stay; switching is no history step. Returns whether it changed."""
        registry.get(visual)
        if visual == self.visual:
            return False
        self.visual = visual
        return True

    def _engine(self) -> Engine:
        return self._engines(self.visual)

    def _params(self) -> LayoutParams:
        w, h, dpr = self.viewport
        return LayoutParams(
            width=w,
            height=h,
            dpr=dpr,
            detail=self.detail,
            min_element=self.min_element,
            sort_by_size=self.sort_by_size,
            font_scale=self.font_scale,
            aspect=self.aspect,
            options=self.visual_options.get(self.visual, ()),
        )

    # ------------------------------------------------------------- navigation

    def _push(self) -> None:
        self.back.append(Entry(self.zoom, self.selected))
        if len(self.back) > HISTORY:  # aggregate zooms hold member arrays: keep the history bounded
            del self.back[0]
        self.forward.clear()

    def _valid(self, zoom: Zoom) -> bool:
        return 0 <= zoom.node < len(self.store)

    def remap(self, new_ids: Callable[[np.ndarray], np.ndarray], fallback: int) -> None:
        """Node ids changed (a folder listed again after a file change, the store compacted):
        ``new_ids`` maps old ids to new ones, -1 for a node that is gone. A zoom onto a gone node
        falls back to ``fallback`` (the nearest folder that is still there); a gone selection is
        dropped; history keeps its order."""

        def one(key: int | None) -> int | None:
            if key is None:
                return None
            tag, node = (key & ~(KEY_SMALL - 1), key & (KEY_SMALL - 1)) if key >= KEY_SMALL else (0, key)
            new = int(new_ids(np.array([node], dtype=np.int64))[0])
            return None if new < 0 else tag | new

        def zoom(z: Zoom | None) -> Zoom | None:
            if z is None:
                return None
            node = one(z.node)
            if node is None:
                return Zoom(fallback, fallback)
            members = None
            if z.members is not None:
                moved = new_ids(z.members.astype(np.int64))
                members = moved[moved >= 0]
            return Zoom(node, one(z.key) or node, members, z.count, zoom(z.parent))

        self.home = one(self.home) or fallback
        self.zoom = zoom(self.zoom) or Zoom(self.home, self.home)
        self.selected = one(self.selected)
        for entries in (self.back, self.forward):
            for i, e in enumerate(entries):
                entries[i] = Entry(zoom(e.zoom) or Zoom(fallback, fallback), one(e.selected))
        with self._flash_lock:
            if self._flashes:
                moved = new_ids(np.array(list(self._flashes), dtype=np.int64))
                self._flashes = {
                    int(m): d for m, d in zip(moved.tolist(), self._flashes.values(), strict=True) if m >= 0
                }
        self.hues.remap(new_ids)
        self.last = None

    def add_flash(self, changes: dict[int, int]) -> None:
        """Blocks to flash in the next frame: node -> change of its allocation (UI-TM-43)."""
        with self._flash_lock:
            for node, delta in changes.items():
                self._flashes[node] = self._flashes.get(node, 0) + delta

    def _flash_rows(self, draw: DrawList) -> list[tuple[int, int]]:
        """Rows for the pending flashes: the block itself, else its aggregate or nearest drawn
        ancestor (several changes in one row add up). The largest ``MAX_FLASH`` changes only.
        Partition layouts draw the zoom root too: a change below it that lands on no row is row -1."""
        with self._flash_lock:
            pending, self._flashes = self._flashes, {}
        if not pending:
            return []
        n = len(self.store)
        rooted = draw.layout != DRAWLIST_LAYOUT
        rows: dict[int, int] = {}
        for node, delta in sorted(pending.items(), key=lambda kv: -abs(kv[1]))[:MAX_FLASH]:
            if not 0 <= node < n:
                continue
            row = draw.row(node)
            if row is None:
                row = self._hidden_row(draw, node)
            if row is None and rooted and self._in_zoom(node):
                row = -1
            if row is not None:
                rows[row] = rows.get(row, 0) + delta
        return sorted(rows.items())

    def _in_zoom(self, node: int) -> bool:
        """Whether ``node`` is the zoom root or below it (an aggregate zoom: one of its members)."""
        z = self.zoom
        chain = [node, *self.store.ancestors(node)]
        if z.members is not None:
            return bool(np.isin(chain, z.members).any())
        return z.node == 0 or z.node in chain

    def _scanning_rows(self, draw: DrawList) -> tuple[np.ndarray, bool]:
        """Rows of the folders, down to ``SCANNING_LEVELS`` below the view root, whose subtree still
        holds folders not listed yet (UI-TM-42), and whether the zoom root (a folder) is one or is
        not listed itself. The not-listed folders climb to their ancestors one level per step; a
        folder reached before stops the climb there (5 M nodes: about 20 ms)."""
        kind, node = draw.kind, draw.node
        folders = (kind == RectKind.FOLDER) | (kind == RectKind.COLLAPSED)
        rows = np.flatnonzero(folders & (draw.depth <= SCANNING_LEVELS) & (node >= 0))
        c = self.store.cols
        n = len(self.store)
        z = self.zoom
        root = z.node if z.members is None and 0 <= z.node < n and c["kind"][z.node] == Kind.DIR else -1
        if not len(rows) and root < 0:
            return rows, False
        flags = c["flags"][:n]
        wanted = np.uint32(Flag.PENDING | Flag.GONE)
        listing = (flags & wanted) == np.uint32(Flag.PENDING)
        cur = np.flatnonzero(listing)
        if not len(cur):
            return cur, False
        depth, parent = c["depth"][:n], c["parent"][:n]
        targets = node[rows].astype(np.int64)
        # nothing above this depth can be one of the targets (the root is above all the rows)
        low = int(depth[root]) if root >= 0 else int(depth[targets].min())
        reached = np.zeros(n, dtype=bool)
        cur = parent[cur]  # the folders themselves are drawn as PENDING blocks, not marked
        while len(cur):
            cur = cur[cur >= 0]
            cur = cur[~reached[cur]]
            reached[cur] = True
            cur = parent[cur[depth[cur] > low]]
        return rows[reached[targets]], root >= 0 and bool(reached[root] or listing[root])

    def zoom_into(self, key: int) -> bool:
        """Zoom into a block of the last frame (double-click / Enter). False if it cannot be zoomed."""
        frame = self.last
        row = frame.draw.row(key) if frame is not None else None
        if frame is None or row is None:
            if 0 < key < len(self.store) and key == int(key):  # a node not drawn right now
                return self.zoom_to_node(key)
            return False
        kind = RectKind(int(frame.draw.kind[row]))
        if kind in (RectKind.FREE, RectKind.UNKNOWN):
            return False
        if kind in (RectKind.SMALL, RectKind.MORE):
            node = int(frame.draw.node[row])
            z = self.zoom
            outer = z if z.members is not None and z.node == node else None  # "+N items" of a zoom
            zoom = Zoom(node, key, frame.draw.members[key].copy(), int(frame.draw.count[row]), outer)
        else:
            node = int(frame.draw.node[row])
            if self.store.cols["kind"][node] != Kind.DIR:
                return self._zoom_to_file(node)
            zoom = Zoom(node, node)
        if zoom.key == self.zoom.key and zoom.node == self.zoom.node:
            return False
        self._push()
        self.zoom = zoom
        self.selected = None
        return True

    def _zoom_to_file(self, node: int) -> bool:
        """Double-clicking a file zooms to its folder and selects the file (UI-MAIN-31)."""
        parent = int(self.store.cols["parent"][node])
        if parent == self.zoom.node and self.zoom.members is None:
            if self.selected == node:
                return False
            self.selected = node
            return True
        if parent <= 0 and self.home != 0:
            return False
        self._push()
        self.zoom = Zoom(parent, parent)
        self.selected = node
        return True

    def zoom_to_node(self, node: int) -> bool:
        if not 0 <= node < len(self.store) or (node == self.zoom.node and self.zoom.members is None):
            return False
        if self.store.cols["kind"][node] != Kind.DIR:
            return self._zoom_to_file(node)
        self._push()
        self.zoom = Zoom(node, node)
        self.selected = None
        return True

    def go_back(self) -> bool:
        while self.back:
            entry = self.back.pop()
            if self._valid(entry.zoom):
                self.forward.append(Entry(self.zoom, self.selected))
                self.zoom, self.selected = entry.zoom, entry.selected
                return True
        return False

    def go_forward(self) -> bool:
        while self.forward:
            entry = self.forward.pop()
            if self._valid(entry.zoom):
                self.back.append(Entry(self.zoom, self.selected))
                self.zoom, self.selected = entry.zoom, entry.selected
                return True
        return False

    def can_go_up(self) -> bool:
        return self.zoom.members is not None or self.zoom.node != self.home

    def go_up(self) -> bool:
        """Zoom to the parent; the block just left becomes the selection (UI-TM-37)."""
        if not self.can_go_up():
            return False
        z = self.zoom
        if z.members is not None:
            up = z.parent if z.parent is not None else Zoom(z.node, z.node)
        else:
            parent = int(self.store.cols["parent"][z.node])
            if parent < 0 or (self.home != 0 and parent == 0):
                return False
            up = Zoom(parent, parent)
        self._push()
        self.zoom = up
        self.selected = z.key
        return True

    def go_home(self) -> bool:
        if self.zoom.node == self.home and self.zoom.members is None:
            return False
        self._push()
        self.zoom = Zoom(self.home, self.home)
        self.selected = None
        return True

    def set_detail(self, detail: int) -> int:
        self.detail = max(DETAIL_MIN, min(DETAIL_MAX, detail))
        return self.detail

    def set_viewport(self, width: float, height: float, dpr: float = 1.0) -> None:
        self.viewport = (max(1.0, width), max(1.0, height), max(0.5, dpr))

    def select(self, key: int | None) -> None:
        self.selected = key

    def reveal(self, key: int) -> bool:
        """Select a block and make sure it is drawn (W3 result click, UI-W3-33).

        A block of the last frame is just selected, and so is a node merged into one of its
        aggregate blocks (under the zoom root, or a member of the aggregate zoomed into). A node
        that is not drawn — below the detail level, or outside the zoom root — makes the view
        zoom (with history) to the highest
        ancestor under which it is within the detail level (or the levels the visualization fits in
        the viewport, when fewer); if it is still not drawn there (too small), to its parent folder,
        where it is drawn or sits in a small-items block.
        """
        frame = self.last
        if frame is not None and frame.draw.row(key) is not None:
            if self.selected == key:
                return False
            self.selected = key
            return True
        store = self.store
        if key >= KEY_SMALL or not 0 < key < len(store) or key == self.zoom.node:
            return False
        c = store.cols
        chain = [int(a) for a in store.ancestors(key)]  # parent first, up to a scan root
        if self.home != 0:
            if self.home not in chain:
                return False
            chain = chain[: chain.index(self.home) + 1]
        elif not chain or chain[-1] != 0:
            chain.append(0)
        here = self.zoom.members is None
        inside = here and self.zoom.node in chain
        member = not here and bool(np.any(self.zoom.members == key))  # in the aggregate zoomed into
        if (inside or member) and frame is not None:
            row = self._hidden_row(frame.draw, key)
            if row is not None and int(frame.draw.kind[row]) in (RectKind.SMALL, RectKind.MORE):
                changed = self.selected != key
                self.selected = key  # drawn inside an aggregate block: select that block
                return changed
        base = self.zoom.node if inside else chain[-1]
        levels = max(1, min(self.detail, self._engine().levels(self._params())))  # what this viewport shows
        want = max(int(c["depth"][base]) if base else 0, int(c["depth"][key]) - levels)
        target = next((a for a in chain if (int(c["depth"][a]) if a else 0) == want), chain[0])
        if here and target == self.zoom.node:
            target = chain[0]
        if here and target == self.zoom.node:  # a direct child that is not drawn (zero size)
            changed = self.selected != key
            self.selected = key
            return changed
        self._push()
        self.zoom = Zoom(target, target)
        self.selected = key
        if target != chain[0] and self.render().draw.row(key) is None:
            self.zoom = Zoom(chain[0], chain[0])  # too small under the higher root: its folder
        return True

    def _hidden_row(self, draw: DrawList, node: int) -> int | None:
        """The drawn aggregate row holding an undrawn node, else its nearest drawn ancestor's row."""
        for key, members in draw.members.items():
            if (key & (3 << 30)) in (KEY_SMALL, KEY_MORE) and np.any(members == node):
                row = draw.row(key)
                if row is not None:
                    return row
                break  # merged into a block too small to draw: a node sits in one aggregate only
        for anc in self.store.ancestors(node):
            row = draw.row(anc)
            if row is not None:
                return row
        return None

    # ------------------------------------------------------------- tags

    def tag_selected(self, color: str | None) -> bool:
        """Ctrl+1..4 / Ctrl+0 on the selection (a real node). Returns whether anything changed."""
        node = self.selected
        if node is None or node >= KEY_SMALL or node <= 0:
            return False
        if color is None:
            changed = self.tags.clear_subtree(self.store, node) > 0
        else:
            changed = self.tags.set(self.store, node, color)  # type: ignore[arg-type]
        if changed:
            self.tags.save()
            if self.filter.normal is not None and (
                self.filter.normal.tags is not None or self.filter.normal.not_tags
            ):
                self.refilter()
        return changed

    # ------------------------------------------------------------- filter

    def set_filter(self, text: str, cursor: int | None = None) -> FilterState:
        """Apply a filter text. With errors the previous valid filter stays in effect (UI-TM-48)."""
        parsed = parse(text, cursor, self._vocab or platform_vocab())
        f = self.filter
        f.text = text
        f.diagnostics = parsed.diagnostics
        if parsed.normal is None:
            f.stale = True
            return f
        f.stale = False
        if parsed.normal.empty:
            f.normal, f.result = None, None
            return f
        f.normal = parsed.normal
        self.refilter()
        return f

    def refilter(self, now: int | None = None) -> None:
        """Re-evaluate the current filter (data changed, time passed, tags changed)."""
        if self.filter.normal is None:
            return
        ctx = FilterContext.current(tags=self.tags.bits(self.store))
        if now is not None:
            ctx = replace(ctx, now=now)
        self.filter.result = evaluate(self.store, self.filter.normal, ctx)

    # ------------------------------------------------------------- rendering

    def _root_spec(self, vis: Visualization) -> RootSpec:
        z = self.zoom
        if z.members is not None:
            return RootSpec(node=z.node, members=z.members, key=z.key)
        free = unknown = None
        hide = self.filter.active and self.filter_mode == "hide"
        root = next((r for r in self.roots if r.node == z.node and r.whole_volume), None)
        if root is not None and not hide and vis.free_space:
            if self.show_free and root.free:
                free = root.free
            if self.show_unknown and root.used is not None:
                unknown = root.used - int(self.store.cols["tot_alloc"][z.node])
        return RootSpec(node=z.node, free=free, unknown=unknown)

    def render(self) -> Frame:
        vis = registry.get(self.visual)
        engine = self._engine()
        res = self.filter.result if self.filter.active else None
        hide = res is not None and self.filter_mode == "hide"
        sizes: SizeSource
        if self.compare is not None and self.direction != "all":
            sizes = ArraySizes(
                self.compare.amounts(self.direction, res.match if hide and res is not None else None)
            )
        elif hide and res is not None:
            matched = np.zeros(len(self.store), dtype=np.int64)
            matched[: res.n] = res.alloc
            sizes = ArraySizes(matched)
        else:
            sizes = StoreSizes(self.basis)
        if not self.show_trash and self.compare is None:
            bins = find_bins(self.store)
            if len(bins) and not self._in_bin(bins):  # zoomed into a bin: it stays
                sizes = WithoutBins(sizes, self.store, bins)
        draw = engine.run(self._params(), self._root_spec(vis), sizes)
        draw.visual = vis.id  # one engine may lay out several visualizations
        frame = self._annotate(draw, res, sizes)
        if self.scanning:
            rows, root = self._scanning_rows(draw)
            frame.flags[rows] |= F_SCANNING
            if root:
                frame.root = replace(frame.root, flags=frame.root.flags | F_SCANNING)
        frame.flash = self._flash_rows(draw)
        self.last = frame
        return frame

    @property
    def has_trash(self) -> bool:
        """The scan holds a recycle bin (the 「回收站」 chip is offered)."""
        return self.compare is None and len(find_bins(self.store)) > 0

    def _in_bin(self, bins: np.ndarray) -> bool:
        node = self.zoom.node
        chain = [node, *self.store.ancestors(node)] if 0 < node < len(self.store) else []
        return bool(np.isin(np.array(chain, dtype=np.int64), bins).any())

    def _annotate(self, draw: DrawList, res: FilterResult | None, sizes: SizeSource) -> Frame:
        n = len(draw)
        store = self.store
        c = store.cols
        node = draw.node.astype(np.int64)
        kind = draw.kind
        real = (kind <= RectKind.PENDING) & (node >= 0)
        flags = np.zeros(n, dtype=np.uint16)
        tag = np.zeros(n, dtype=np.uint8)
        tone = np.full(n, 4, dtype=np.uint8)
        # own tags
        tag_index = self.tags.index(store)
        if real.any():
            tag[real] = tag_index[node[real]]
        flags[tag > 0] |= F_TAGGED
        # in a recycle bin: real blocks by their node, aggregates by their container
        holder = np.where(real | (kind == RectKind.SMALL) | (kind == RectKind.MORE), node, -1)
        inside, is_bin = trash_marks(store, holder)
        flags[inside] |= F_TRASH
        flags[is_bin & real] |= F_TRASH_BIN
        # time bands (files: own mtime; aggregates: the band holding most of their bytes)
        now = FilterContext.current().now
        files = real & (kind == RectKind.FILE)
        if files.any():
            tone[files] = _bands(now, c["mtime"][node[files]].astype(np.int64))
        for row in np.nonzero((kind == RectKind.SMALL) | (kind == RectKind.MORE))[0]:
            members = draw.members.get(int(draw.key[row]))
            if members is not None and len(members):
                tone[row] = _tone_of(store, now, members)
        # filter state
        totals = None
        if res is not None:
            match = np.zeros(n, dtype=bool)
            inside = real & (node < res.n)
            match[inside] = res.count[node[inside]] > 0
            for row in np.nonzero((kind == RectKind.SMALL) | (kind == RectKind.MORE))[0]:
                members = draw.members.get(int(draw.key[row]))
                if members is not None:
                    members = members[members < res.n]
                    match[row] = bool(res.count[members].sum() > 0)
            flags[match] |= F_MATCH
            flags[~match] |= F_DIM
            hit = np.zeros(n, dtype=bool)
            hit[inside] = res.folder_hit[node[inside]]
            flags[hit] |= F_FOLDER_HIT
            if self.zoom.members is None and self.zoom.node < res.n:
                totals = res.totals(self.zoom.node)
            elif self.zoom.members is not None:
                m = self.zoom.members[self.zoom.members < res.n]
                totals = (int(res.count[m].sum()), int(res.alloc[m].sum()), int(res.size[m].sum()))
        # selection: the selected block, or its nearest visible ancestor / aggregate
        selected_row = self._selected_row(draw)
        if selected_row is not None:
            flags[selected_row] |= F_SELECTED
            p = int(draw.parent[selected_row])
            while p >= 0:
                flags[p] |= F_ON_PATH
                p = int(draw.parent[p])
        if self.zoom.members is not None:
            scanned = int(Layout.effective_sizes_of(store, self.zoom.members).sum())
        else:
            scanned = int(Layout.effective_sizes_of(store, np.array([self.zoom.node]))[0])
        delta = dstat = None
        if self.compare is not None:
            delta, dstat = self._changes(draw, real)
        dup = dupg = None
        if self.color_mode == "dupes" and self.dupes_annotate is not None:
            cols = self.dupes_annotate(store, draw)
            if cols is not None:
                dup, dupg = cols
        return Frame(
            draw=draw,
            flags=flags,
            tag=tag,
            tone=tone,
            branch=self.hues.hues(self.zoom.key, draw),
            root=self._root_row(res, sizes, tag_index, now),
            selected_row=selected_row,
            breadcrumb=self.breadcrumb(),
            filter_totals=totals,
            scanned_total=scanned,
            delta=delta,
            dstat=dstat,
            dup=dup,
            dupg=dupg,
        )

    def _root_row(
        self, res: FilterResult | None, sizes: SizeSource, tag_index: np.ndarray, now: int
    ) -> RootRow:
        """The zoom root as a row: its kind, bytes on this frame's basis, filter and tag flags, time
        band and change (compare views). :meth:`render` adds the scanning flag."""
        z = self.zoom
        store = self.store
        c = store.cols
        members = z.members
        flags = tag = 0
        tone = 4
        delta = dstat = None
        if members is not None:  # an aggregate zoom: the block's members
            kind = RectKind.SMALL if z.key & _TAG_BITS == KEY_SMALL else RectKind.MORE
            ids = members[members < sizes.n]
            size = int(sizes.of(ids).sum())
            if len(members):
                tone = _tone_of(store, now, members)
            if self.compare is not None:
                delta, dstat = self._aggregate_change(members)
        else:
            node = z.node
            ids = np.array([node], dtype=np.int64)
            if c["kind"][node] != Kind.DIR:
                kind = RectKind.FILE
                tone = int(_bands(now, c["mtime"][ids].astype(np.int64))[0])
            else:
                kind = RectKind.PENDING if c["flags"][node] & Flag.PENDING else RectKind.FOLDER
            size = int(sizes.of(ids)[0]) if node < sizes.n else 0
            tag = int(tag_index[node]) if 0 < node < len(tag_index) else 0
            if tag:
                flags |= F_TAGGED
            t = self.compare
            if t is not None and node < len(t.delta):
                delta, dstat = int(t.delta[node]), int(t.status[node])
        if res is not None:
            inside = ids[ids < res.n]
            flags |= F_MATCH if int(res.count[inside].sum()) > 0 else F_DIM
            if members is None and z.node < res.n and res.folder_hit[z.node]:
                flags |= F_FOLDER_HIT
        inside, is_bin = trash_marks(store, np.array([z.node], dtype=np.int64))  # aggregates: the container
        if inside[0]:
            flags |= F_TRASH
        if is_bin[0] and members is None:
            flags |= F_TRASH_BIN
        return RootRow(
            key=z.key,
            node=z.node,
            kind=int(kind),
            size=size,
            count=z.count if members is not None else 0,
            flags=flags,
            tag=tag,
            tone=tone,
            delta=delta,
            dstat=dstat,
        )

    def _changes(self, draw: DrawList, real: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
        """Compare views: each block's signed net change and status (aggregates: their members)."""
        t = self.compare
        n = len(draw)
        node = draw.node.astype(np.int64)
        delta = np.zeros(n, dtype=np.int64)
        dstat = np.zeros(n, dtype=np.uint8)
        ok = real & (node < len(t.delta))
        delta[ok] = t.delta[node[ok]]
        dstat[ok] = t.status[node[ok]]
        kind = draw.kind
        for row in np.nonzero((kind == RectKind.SMALL) | (kind == RectKind.MORE))[0]:
            members = draw.members.get(int(draw.key[row]))
            if members is None or not len(members):
                continue
            delta[row], dstat[row] = self._aggregate_change(members)
        return delta, dstat

    def _aggregate_change(self, members: np.ndarray) -> tuple[int, int]:
        """Compare views: an aggregate's net change (the shown direction's part) and status."""
        t = self.compare
        d = t.delta[members]
        if self.direction == "grow":
            d = d[d > 0]
        elif self.direction == "shrink":
            d = d[d < 0]
        st = t.status[members]
        return int(d.sum()), int(st[0]) if bool((st == st[0]).all()) and st[0] in (1, 2) else 3

    def rebase(
        self,
        store: NodeStore,
        compare: Any,
        layout: Layout,
        roots: list[RootInfoLike],
        engines: Callable[[str], Engine] | None = None,
    ) -> None:
        """A compare view moves to a recomputed change tree (live target): zoom root and selection
        follow by path, the history starts anew. ``engines`` lay out ``layout`` (None: its own)."""
        old = self.store

        def path_of(node: int | None) -> str | None:
            return old.path(node) if node is not None and 0 < node < len(old) else None

        zoom_path = path_of(self.zoom.node)
        sel_path = path_of(self.selected) if self.selected is not None and self.selected >= 0 else None
        self.store, self.compare, self.layout, self.roots = store, compare, layout, roots
        self.basis = layout
        self._engines = engines or Engines(layout).get
        self.hues.clear()
        self.home = roots[0].node if len(roots) == 1 else 0
        node = store.find(zoom_path) if zoom_path else None
        self.zoom = Zoom(node, node) if node is not None else Zoom(self.home, self.home)
        self.selected = store.find(sel_path) if sel_path else None
        self.back.clear()
        self.forward.clear()
        self.last = None
        self.refilter()

    def _selected_row(self, draw: DrawList) -> int | None:
        """Row of the selection: the selected block, or the aggregate / nearest ancestor holding it.
        An aggregate block that is not drawn (another visualization merges differently) falls back
        to its node: the container of "N small items", the largest member of an aggregate zoom's
        "+N items"; free and unknown space and the zoom root itself have no fallback."""
        sel = self.selected
        if sel is None:
            return None
        row = draw.row(sel)
        if row is not None:
            return row
        if sel >= KEY_SMALL:
            if sel & _TAG_BITS not in (KEY_SMALL, KEY_MORE):
                return None
            sel = sel & KEY_NODE_MASK
            if sel == self.zoom.node or not 0 < sel < len(self.store):
                return None
            row = draw.row(sel)
            if row is not None:
                return row
        return self._hidden_row(draw, sel)

    def breadcrumb(self) -> list[tuple[int, str]]:
        store = self.store
        z = self.zoom
        chain: list[int] = []
        cur = z.node
        while cur > 0:
            chain.append(cur)
            if cur == self.home:
                break
            cur = int(store.cols["parent"][cur])
        if self.home == 0:
            chain.append(0)
        out = [(k, store.name(k) if k else "") for k in reversed(chain)]
        aggregates: list[Zoom] = []
        agg: Zoom | None = z
        while agg is not None and agg.members is not None:  # "+N items" opened inside an aggregate zoom
            aggregates.append(agg)
            agg = agg.parent
        for a in reversed(aggregates):
            out.append((a.key, ("small" if a.key & (3 << 30) == KEY_SMALL else "more") + f":{a.count}"))
        return out


def _bands(now: int, mtime: np.ndarray) -> np.ndarray:
    age = now - mtime
    out = np.searchsorted(np.array(TONE_LIMITS), age, side="right").astype(np.uint8)
    out[mtime == 0] = 4
    return out


def _tone_of(store: NodeStore, now: int, members: np.ndarray) -> int:
    """Time band of an aggregate: the one holding most of its members' bytes."""
    weights = np.bincount(
        _bands(now, store.cols["mtime"][members].astype(np.int64)),
        weights=Layout.effective_sizes_of(store, members),
        minlength=5,
    )
    return int(np.argmax(weights))


__all__ = [
    "BASELINE",
    "F_DIM",
    "F_FOLDER_HIT",
    "F_MATCH",
    "F_ON_PATH",
    "F_SCANNING",
    "F_SELECTED",
    "F_TAGGED",
    "F_TRASH",
    "F_TRASH_BIN",
    "Engines",
    "Frame",
    "RootRow",
    "View",
    "ViewSettings",
    "Zoom",
    "showable",
]

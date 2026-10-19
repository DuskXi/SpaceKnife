"""View state machine: one tab or split pane looking at one scan (UI-TM-33…38, UI-TM-55, W3, W8).

Each view owns its zoom root, back/forward history (every entry remembers its selection),
detail level, viewport, free/unknown switches, colour mode, filter and selection. Views of the
same scan share the node store, the tags and the layout caches; rendering returns a
:class:`Frame` — the draw list plus per-block flags, tag, time tone, the selected row and the
breadcrumb — that the protocol layer turns into one binary frame.
"""

from __future__ import annotations

import threading
from collections.abc import Callable
from dataclasses import dataclass, field, replace
from typing import Any, Literal

import numpy as np

from spaceknife_protocol.filter import FilterDiagnostic, FilterNormal, FilterVocab
from spaceknife_protocol.filter_syntax import parse

from ..filter.engine import FilterContext, FilterResult, evaluate
from ..filter.vocab import platform_vocab
from ..layout.engine import KEY_MORE, KEY_SMALL, DrawList, Layout, LayoutParams, RectKind, RootSpec
from ..store.nodes import Flag, Kind, NodeStore
from .tags import TagStore

ColorMode = Literal["class", "flat", "time"]
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

DAY = 86400
TONE_LIMITS = (DAY, 7 * DAY, 30 * DAY, 365 * DAY)  # time colouring: < 1 d, < 1 w, < 1 mo, < 1 y, older


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
    color_mode: ColorMode = "class"
    filter_mode: FilterMode = "dim"
    font_scale: float = 1.0
    aspect: float = 1.0


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
    selected_row: int | None
    breadcrumb: list[tuple[int, str]]  # (key, label) from the home root to the zoom root
    filter_totals: tuple[int, int, int] | None  # (files, allocated, logical) below the zoom root
    scanned_total: int  # allocated bytes below the zoom root
    flash: list[tuple[int, int]] = field(default_factory=list)  # (row, allocation change): file changes
    delta: np.ndarray | None = None  # i8 compare views: signed net change per row (W6)
    dstat: np.ndarray | None = None  # u1 compare views: 0 unchanged, 1 new, 2 deleted, 3 changed


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
    ) -> None:
        self.id = view_id
        self.store = store
        self.roots = roots
        self.tags = tags or TagStore()
        self.layout = layout or Layout(store)
        s = settings or ViewSettings()
        self.detail = s.detail
        self.min_element = s.min_element
        self.sort_by_size = s.sort_by_size
        self.show_free = s.show_free
        self.show_unknown = s.show_unknown
        self.color_mode: ColorMode = s.color_mode
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
        self.last = None

    def add_flash(self, changes: dict[int, int]) -> None:
        """Blocks to flash in the next frame: node -> change of its allocation (UI-TM-43)."""
        with self._flash_lock:
            for node, delta in changes.items():
                self._flashes[node] = self._flashes.get(node, 0) + delta

    def _flash_rows(self, draw: DrawList) -> list[tuple[int, int]]:
        """Rows for the pending flashes: the block itself, else its aggregate or nearest drawn
        ancestor (several changes in one row add up). The largest ``MAX_FLASH`` changes only."""
        with self._flash_lock:
            pending, self._flashes = self._flashes, {}
        if not pending:
            return []
        n = len(self.store)
        rows: dict[int, int] = {}
        for node, delta in sorted(pending.items(), key=lambda kv: -abs(kv[1]))[:MAX_FLASH]:
            if not 0 <= node < n:
                continue
            row = draw.row(node)
            if row is None:
                row = self._hidden_row(draw, node)
            if row is not None:
                rows[row] = rows.get(row, 0) + delta
        return sorted(rows.items())

    def _scanning_rows(self, draw: DrawList) -> np.ndarray:
        """Rows of the folders, down to ``SCANNING_LEVELS`` below the view root, whose subtree still
        holds folders not listed yet (UI-TM-42). The not-listed folders climb to their ancestors one
        level per step; a folder reached before stops the climb there (5 M nodes: about 20 ms)."""
        kind, node = draw.kind, draw.node
        folders = (kind == RectKind.FOLDER) | (kind == RectKind.COLLAPSED)
        rows = np.flatnonzero(folders & (draw.depth <= SCANNING_LEVELS) & (node >= 0))
        if not len(rows):
            return rows
        c = self.store.cols
        n = len(self.store)
        flags = c["flags"][:n]
        wanted = np.uint32(Flag.PENDING | Flag.GONE)
        cur = np.flatnonzero((flags & wanted) == np.uint32(Flag.PENDING))
        if not len(cur):
            return cur
        depth, parent = c["depth"][:n], c["parent"][:n]
        targets = node[rows].astype(np.int64)
        low = int(depth[targets].min())  # nothing above this depth can be one of the rows
        reached = np.zeros(n, dtype=bool)
        cur = parent[cur]  # the folders themselves are drawn as PENDING blocks, not marked
        while len(cur):
            cur = cur[cur >= 0]
            cur = cur[~reached[cur]]
            reached[cur] = True
            cur = parent[cur[depth[cur] > low]]
        return rows[reached[targets]]

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
        ancestor under which it is within the detail level; if it is still not drawn there (too
        small), to its parent folder, where it is drawn or sits in a small-items block.
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
        want = max(int(c["depth"][base]) if base else 0, int(c["depth"][key]) - self.detail)
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

    def _root_spec(self) -> RootSpec:
        z = self.zoom
        if z.members is not None:
            return RootSpec(node=z.node, members=z.members, key=z.key)
        free = unknown = None
        hide = self.filter.active and self.filter_mode == "hide"
        root = next((r for r in self.roots if r.node == z.node and r.whole_volume), None)
        if root is not None and not hide:
            if self.show_free and root.free:
                free = root.free
            if self.show_unknown and root.used is not None:
                unknown = root.used - int(self.store.cols["tot_alloc"][z.node])
        return RootSpec(node=z.node, free=free, unknown=unknown)

    def render(self) -> Frame:
        w, h, dpr = self.viewport
        params = LayoutParams(
            width=w,
            height=h,
            dpr=dpr,
            detail=self.detail,
            min_element=self.min_element,
            sort_by_size=self.sort_by_size,
            font_scale=self.font_scale,
            aspect=self.aspect,
        )
        res = self.filter.result if self.filter.active else None
        sizes = None
        hide = res is not None and self.filter_mode == "hide"
        if self.compare is not None and self.direction != "all":
            sizes = self.compare.amounts(self.direction, res.match if hide and res is not None else None)
        elif hide and res is not None:
            sizes = np.zeros(len(self.store), dtype=np.int64)
            sizes[: res.n] = res.alloc
        draw = self.layout.run(params, self._root_spec(), sizes)
        frame = self._annotate(draw, res)
        if self.scanning:
            frame.flags[self._scanning_rows(draw)] |= F_SCANNING
        frame.flash = self._flash_rows(draw)
        self.last = frame
        return frame

    def _annotate(self, draw: DrawList, res: FilterResult | None) -> Frame:
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
        # time bands (files: own mtime; aggregates: the band holding most of their bytes)
        now = FilterContext.current().now
        mtime = c["mtime"][: len(store)].astype(np.int64)
        files = real & (kind == RectKind.FILE)
        if files.any():
            tone[files] = _bands(now, mtime[node[files]])
        for row in np.nonzero((kind == RectKind.SMALL) | (kind == RectKind.MORE))[0]:
            members = draw.members.get(int(draw.key[row]))
            if members is not None and len(members):
                weights = np.bincount(
                    _bands(now, mtime[members]),
                    weights=Layout.effective_sizes_of(store, members),
                    minlength=5,
                )
                tone[row] = int(np.argmax(weights))
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
        return Frame(
            draw=draw,
            flags=flags,
            tag=tag,
            tone=tone,
            selected_row=selected_row,
            breadcrumb=self.breadcrumb(),
            filter_totals=totals,
            scanned_total=scanned,
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
            d = t.delta[members]
            if self.direction == "grow":
                d = d[d > 0]
            elif self.direction == "shrink":
                d = d[d < 0]
            delta[row] = int(d.sum())
            st = t.status[members]
            dstat[row] = int(st[0]) if bool((st == st[0]).all()) and st[0] in (1, 2) else 3
        return delta, dstat

    def rebase(self, store: NodeStore, compare: Any, layout: Layout, roots: list[RootInfoLike]) -> None:
        """A compare view moves to a recomputed change tree (live target): zoom root and selection
        follow by path, the history starts anew."""
        old = self.store

        def path_of(node: int | None) -> str | None:
            return old.path(node) if node is not None and 0 < node < len(old) else None

        zoom_path = path_of(self.zoom.node)
        sel_path = path_of(self.selected) if self.selected is not None and self.selected >= 0 else None
        self.store, self.compare, self.layout, self.roots = store, compare, layout, roots
        self.home = roots[0].node if len(roots) == 1 else 0
        node = store.find(zoom_path) if zoom_path else None
        self.zoom = Zoom(node, node) if node is not None else Zoom(self.home, self.home)
        self.selected = store.find(sel_path) if sel_path else None
        self.back.clear()
        self.forward.clear()
        self.last = None
        self.refilter()

    def _selected_row(self, draw: DrawList) -> int | None:
        """Row of the selection: the selected block, or the aggregate / nearest ancestor holding it."""
        sel = self.selected
        if sel is None:
            return None
        row = draw.row(sel)
        if row is not None or sel >= KEY_SMALL:
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


__all__ = [
    "F_DIM",
    "F_FOLDER_HIT",
    "F_MATCH",
    "F_ON_PATH",
    "F_SELECTED",
    "F_TAGGED",
    "Frame",
    "View",
    "ViewSettings",
    "Zoom",
]

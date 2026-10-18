"""Nested treemap layout: the zoom root's subtree as a flat, pre-ordered list of pixel rectangles.

Geometry follows docs/UI-SPEC.md (T section, tokens ``treemap.geometry``): every block keeps a
2 px gap to its neighbours; an expanded folder has a 1 px border, 2 px padding and an 18 px
header; blocks under 2 px are not drawn and content under 4 px is not subdivided. All lengths
are scaled by the device pixel ratio and snapped to whole physical pixels, so seams are crisp at
125 % / 150 % and identical in every frontend.

Small items (G5): in each container, files whose share of the area is below the minimum element
size are merged into one "N small items" block (coloured by a class that holds ≥ 50 % of its
bytes, else "other"); siblings that are still too small afterwards — folders, files, that block —
are merged into "+N items", placed last. Merging needs at least two members.

Aggregate zoom (UI-TM-13): the members of such a block fill the view; the largest are drawn one by
one and the rest form one "+N items" block, shrunk when that lets more members through, so every
zoom into an aggregate shows members of its own.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import IntEnum, IntFlag

import numpy as np

from ..classes import DEFAULT_CLASSES, ClassTable
from ..store.nodes import Flag, Kind, NodeStore
from .squarify import squarify

# Stable keys: node ids for real nodes; tagged ids for synthetic blocks.
KEY_SMALL = 1 << 30  # | container id: "N small items" of that container
KEY_MORE = 2 << 30  # | container id: "+N items" of that container
KEY_FREE = (3 << 30) | 1
KEY_UNKNOWN = (3 << 30) | 2
KEY_NODE_MASK = (1 << 30) - 1
# In an aggregate zoom the leftover "+N items" is KEY_MORE | its largest member instead: members are
# children of the container, so it differs from the container's blocks, and the zoomed block's own
# largest member is drawn on its own (RootSpec.key guards the rest), so it differs from that too.

PAGE_REST = 0.125  # aggregate zoom: the share of the view a shrunk "+N items" block keeps


class RectKind(IntEnum):
    FILE = 0
    FOLDER = 1  # expanded: frame + header, children inside
    COLLAPSED = 2  # folder drawn as a solid block (detail level reached or too small)
    PENDING = 3  # folder not scanned yet
    SMALL = 4  # "N small items"
    MORE = 5  # "+N items"
    FREE = 6
    UNKNOWN = 7


FILE, FOLDER, COLLAPSED, PENDING_RECT = 0, 1, 2, 3  # RectKind values as plain ints (hot loop)
DIR = int(Kind.DIR)
PENDING = int(Flag.PENDING)


class Label(IntFlag):
    NONE = 0
    NAME = 1
    SIZE = 2  # second line with the size
    BIG = 4  # area > 52,000 px²: larger type
    SHORT = 8  # height < 22 px: name only, smaller type, centred vertically


L_NAME, L_SIZE, L_BIG, L_SHORT = int(Label.NAME), int(Label.SIZE), int(Label.BIG), int(Label.SHORT)


@dataclass(frozen=True, slots=True)
class LayoutParams:
    width: float  # CSS px
    height: float
    dpr: float = 1.0
    detail: int = 5  # levels drawn below the zoom root (1–15)
    min_element: float = 14.0  # CSS px: blocks below min_element² are merged
    sort_by_size: bool = True
    gap: float = 2.0
    border: float = 1.0
    padding: float = 2.0
    header: float = 18.0
    min_draw: float = 2.0
    min_content: float = 4.0
    expand_min_w: float = 48.0
    expand_min_h: float = 40.0


@dataclass(frozen=True, slots=True)
class RootSpec:
    """What fills the view: a node's children, or the members of an aggregate block."""

    node: int
    members: np.ndarray | None = None  # zoomed into a SMALL/MORE block: just these nodes
    free: int | None = None  # whole-volume root: free and unknown space blocks
    unknown: int | None = None
    key: int | None = None  # the SMALL/MORE block zoomed into: no block inside reuses its key


@dataclass(slots=True)
class DrawList:
    """Columns of the draw list: one row per rectangle, parents before their children."""

    key: np.ndarray  # u4 stable id
    node: np.ndarray  # i4 node id (container id for SMALL/MORE, -1 for FREE/UNKNOWN)
    parent: np.ndarray  # i4 row of the enclosing FOLDER rectangle, -1 at the top
    kind: np.ndarray  # u1 RectKind
    depth: np.ndarray  # u1 nesting level, 1 = child of the zoom root
    x: np.ndarray  # i4 physical px
    y: np.ndarray
    w: np.ndarray
    h: np.ndarray
    cls: np.ndarray  # u1 index into the class table (files, SMALL)
    label: np.ndarray  # u1 Label bits
    size: np.ndarray  # i8 bytes (area basis, except a shrunk "+N items" in an aggregate zoom)
    count: np.ndarray  # u4 files in SMALL, entries in MORE (SMALL counted by its files)
    names: list[str]  # per row; "" where no label is drawn (SMALL/MORE/FREE/UNKNOWN: frontend text)
    members: dict[int, np.ndarray] = field(default_factory=dict)  # SMALL/MORE key -> node ids
    width: int = 0  # physical px
    height: int = 0
    elapsed_s: float = 0.0

    def __len__(self) -> int:
        return len(self.key)

    def row(self, key: int) -> int | None:
        hit = np.nonzero(self.key == key)[0]
        return int(hit[0]) if hit.size else None


class _Geom:
    """Lengths in physical pixels."""

    def __init__(self, p: LayoutParams) -> None:
        d = p.dpr
        self.dpr = d
        self.gap = max(1, round(p.gap * d))
        self.border = max(1, round(p.border * d))
        self.pad = round(p.padding * d)
        self.header = round(p.header * d)
        self.min_draw = p.min_draw * d
        self.min_content = p.min_content * d
        self.expand_w = p.expand_min_w * d
        self.expand_h = p.expand_min_h * d
        self.threshold = (p.min_element * d) ** 2

    def label(self, w: int, h: int) -> Label:
        cw, ch = w / self.dpr, h / self.dpr
        if cw < 30 or ch < 15:
            return Label.NONE
        if ch < 22:
            return Label.NAME | Label.SHORT
        lbl = Label.NAME
        if cw >= 42 and ch >= 31:
            lbl |= Label.SIZE
        if cw * ch > 52_000:
            lbl |= Label.BIG
        return lbl


class Layout:
    """Lays out one store; reusable across calls."""

    def __init__(self, store: NodeStore, classes: ClassTable = DEFAULT_CLASSES) -> None:
        self.store = store
        self.classes = classes
        self.class_ids = [c.id for c in classes.classes]
        self.other = self.class_ids.index("other")
        self._ext_cls = np.zeros(0, dtype=np.uint8)
        self._sizes: tuple[int, int, np.ndarray] | None = None  # (store version, node count, sizes)

    def ext_classes(self) -> np.ndarray:
        """Colour class index per extension-table id."""
        table = self.store.ext_table
        if len(self._ext_cls) != len(table):
            index = {cid: i for i, cid in enumerate(self.class_ids)}
            self._ext_cls = np.fromiter(
                (index[self.classes.color_class(e)] if e else self.other for e in table), np.uint8, len(table)
            )
        return self._ext_cls

    @staticmethod
    def effective_sizes(store: NodeStore) -> np.ndarray:
        """Area basis per node: subtree allocation for folders, own allocation for files."""
        n = len(store)
        c = store.cols
        is_dir = c["kind"][:n] == Kind.DIR
        dup = (c["flags"][:n] & np.uint32(Flag.HARDLINK_DUP)) != 0
        out: np.ndarray = np.where(is_dir, c["tot_alloc"][:n], np.where(dup, 0, c["alloc"][:n])).astype(
            np.int64
        )
        return out

    @staticmethod
    def effective_sizes_of(store: NodeStore, ids: np.ndarray) -> np.ndarray:
        """:meth:`effective_sizes` for some nodes only."""
        c = store.cols
        is_dir = c["kind"][ids] == Kind.DIR
        dup = (c["flags"][ids] & np.uint32(Flag.HARDLINK_DUP)) != 0
        out: np.ndarray = np.where(is_dir, c["tot_alloc"][ids], np.where(dup, 0, c["alloc"][ids])).astype(
            np.int64
        )
        return out

    def sizes(self) -> np.ndarray:
        """:meth:`effective_sizes`, cached until the store changes."""
        store = self.store
        cached = self._sizes
        if cached is None or cached[0] != store.version or cached[1] != len(store):
            cached = (store.version, len(store), self.effective_sizes(store))
            self._sizes = cached
        return cached[2]

    def run(self, params: LayoutParams, root: RootSpec, sizes: np.ndarray | None = None) -> DrawList:
        """Lay out ``root`` in the viewport. ``sizes`` overrides the area basis (filter: hide mode)."""
        t0 = time.perf_counter()
        run = _Run(self, params, self.sizes() if sizes is None else sizes)
        W, H = round(params.width * params.dpr), round(params.height * params.dpr)
        c = self.store.cols
        if root.members is not None:
            run.avoid = -1 if root.key is None else int(root.key)
            members = np.asarray(root.members, dtype=np.int64)
            run.container(root.node, members, [], (0, 0, W, H), 1, -1, page=True)
        elif c["kind"][root.node] == Kind.DIR:
            extra: list[tuple[int, int]] = []
            if root.free:
                extra.append((KEY_FREE, int(root.free)))
            if root.unknown is not None and root.unknown > 0:
                extra.append((KEY_UNKNOWN, int(root.unknown)))
            run.container(root.node, run.children(root.node), extra, (0, 0, W, H), 1, -1)
        else:  # a file as the zoom root: its block fills the view
            run.container(-1, np.array([root.node], dtype=np.int64), [], (0, 0, W, H), 1, -1)
        return run.result(W, H, time.perf_counter() - t0)


@dataclass(slots=True)
class _Item:
    key: int
    node: int
    kind: RectKind
    size: int
    cls: int
    count: int
    weight: float = 0.0  # area basis when it is not ``size`` (a shrunk "+N items")


class _Run:
    def __init__(self, layout: Layout, params: LayoutParams, sizes: np.ndarray) -> None:
        self.layout = layout
        self.store = layout.store
        self.p = params
        self.g = _Geom(params)
        self.sizes = sizes
        n = len(sizes)
        c = self.store.cols
        self.kind = c["kind"][:n]
        self.flags = c["flags"][:n]
        self.child_start = c["child_start"][:n]
        self.child_count = c["child_count"][:n]
        self.cls = layout.ext_classes()[c["ext"][:n]]
        self.rows: list[tuple[int, ...]] = []
        self.names: list[str] = []
        self.members: dict[int, np.ndarray] = {}
        self.avoid = -1  # aggregate zoom: key of the block zoomed into

    def children(self, node: int) -> np.ndarray:
        # a scan may list this folder while we read: count before start, a range never starts at
        # node 0 (else: not listed yet, see NodeStore.child_range), and never past the node count
        count = int(self.child_count[node])
        start = int(self.child_start[node]) if count > 0 else 0
        if count <= 0 or start < 1:
            return np.empty(0, dtype=np.int64)
        stop = min(start + count, len(self.sizes))
        return np.arange(start, max(start, stop), dtype=np.int64)

    # ------------------------------------------------------------- items

    def items(
        self, container: int, ids: np.ndarray, extra: list[tuple[int, int]], area: float
    ) -> list[_Item]:
        """Ordered items of one container after the small-item and "+N" merges.

        Only blocks that will be drawn become Python objects; everything too small is counted and
        merged with vectorised operations, so a huge folder costs one numpy pass.
        """
        s = self.sizes[ids]
        keep = s > 0
        ids, s = ids[keep], s[keep]
        total = float(s.sum()) + sum(v for _, v in extra)
        if total <= 0:
            return []
        thr = self.g.threshold * total / area if area > 0 else float("inf")  # bytes below → too small
        below = s < thr
        is_file = self.kind[ids] != Kind.DIR
        small = below & is_file
        tag = container & KEY_NODE_MASK
        agg: _Item | None = None
        n_small = int(np.count_nonzero(small))
        if n_small >= 2:
            sm_ids, sm_s = ids[small], s[small]
            by_cls = np.bincount(self.cls[sm_ids], weights=sm_s, minlength=len(self.layout.class_ids))
            top = int(np.argmax(by_cls))
            sm_total = int(sm_s.sum())
            cls = top if by_cls[top] * 2 >= sm_total else self.layout.other
            agg = _Item(KEY_SMALL | tag, container, RectKind.SMALL, sm_total, cls, n_small)
            self.members[KEY_SMALL | tag] = sm_ids
            rest = ~small
        else:
            rest = np.ones(len(ids), dtype=bool)
        # "+N items": everything still below the threshold (folders, a lone small file, the aggregate)
        tiny = rest & below
        agg_tiny = agg is not None and agg.size < thr
        n_tiny = int(np.count_nonzero(tiny)) + (1 if agg_tiny else 0)
        more: _Item | None = None
        if n_tiny >= 2:
            t_ids, t_s = ids[tiny], s[tiny]
            members = [t_ids]
            size, count = int(t_s.sum()), len(t_ids)
            if agg is not None and agg_tiny:
                members.append(self.members.pop(KEY_SMALL | tag))
                size, count = size + agg.size, count + agg.count
                agg = None
            self.members[KEY_MORE | tag] = np.concatenate(members)
            more = _Item(KEY_MORE | tag, container, RectKind.MORE, size, self.layout.other, count)
            rest &= ~tiny
        ids, s = ids[rest], s[rest]
        if self.p.sort_by_size:
            order = np.argsort(-s, kind="stable")
        else:
            names = [self.store.name(int(i)).casefold() for i in ids]
            order = np.array(sorted(range(len(ids)), key=names.__getitem__), dtype=np.int64)
        cls_of = self.cls
        out = [
            _Item(int(ids[i]), int(ids[i]), RectKind.FILE, int(s[i]), int(cls_of[ids[i]]), 1) for i in order
        ]
        for key, size in extra:
            out.append(_Item(key, -1, RectKind.FREE if key == KEY_FREE else RectKind.UNKNOWN, size, 0, 0))
        if agg is not None:
            out.append(agg)
        if self.p.sort_by_size and (extra or agg is not None):
            out.sort(key=lambda it: -it.size)  # stable: equal sizes keep their order
        if more is not None:
            out.append(more)
        return out

    def page_items(self, container: int, ids: np.ndarray, area: float) -> list[_Item]:
        """Items of an aggregate zoom: the largest members one by one, the rest as one "+N items".

        Members are drawn on their own while each still gets the minimum element size. When the
        leftover's share of the view is what keeps them merged, that block is shrunk to
        ``PAGE_REST`` of the view, so at least the largest member always gets through and zooming
        into the leftover again shows the next members.
        """
        s = self.sizes[ids]
        keep = s > 0
        ids, s = ids[keep], s[keep]
        n = len(ids)
        if n == 0:
            return []
        order = np.lexsort((ids, -s))  # largest first, ties by id: a stable representative
        ids, s = ids[order], s[order]
        sf = s.astype(np.float64)
        total = float(sf.sum())
        need = self.g.threshold / area if area > 0 else float("inf")  # share of the view per member
        fits = sf >= need * total  # proportional: a prefix, as sizes descend
        prefix = np.cumsum(sf)
        basis = np.minimum(total, prefix / (1 - PAGE_REST))  # area basis with the leftover shrunk
        shrunk = sf >= need * basis
        k_prop = n if fits.all() else int(np.argmin(fits))
        k = max(k_prop, n if shrunk.all() else int(np.argmin(shrunk)), 1)
        if n - k == 1:
            k = n  # merging needs at least two members
        page, page_s = ids[:k], s[:k]
        if not self.p.sort_by_size:
            names = [self.store.name(int(i)).casefold() for i in page]
            by_name = np.array(sorted(range(k), key=names.__getitem__), dtype=np.int64)
            page, page_s = page[by_name], page_s[by_name]
        cls_of = self.cls
        out = [
            _Item(i, i, RectKind.FILE, z, int(cls_of[i]), 1)
            for i, z in zip(page.tolist(), page_s.tolist(), strict=True)
        ]
        if k < n:
            rest = ids[k:]
            key = KEY_MORE | (int(rest[0]) & KEY_NODE_MASK)
            if key == self.avoid:  # sizes changed since that zoom: the next member represents it
                key = KEY_MORE | (int(rest[1]) & KEY_NODE_MASK)
            weight = float((total if k == k_prop else basis[k - 1]) - prefix[k - 1])
            self.members[key] = rest
            out.append(
                _Item(key, container, RectKind.MORE, int(s[k:].sum()), self.layout.other, len(rest), weight)
            )
        return out

    # ------------------------------------------------------------- placement

    def container(
        self,
        container: int,
        ids: np.ndarray,
        extra: list[tuple[int, int]],
        rect: tuple[int, int, int, int],
        level: int,
        parent_row: int,
        page: bool = False,
    ) -> None:
        g = self.g
        x0, y0, x1, y1 = rect
        # cells cover the content rectangle extended by one gap; each block keeps its gap right/bottom
        ex1, ey1 = x1 + g.gap, y1 + g.gap
        area = float((ex1 - x0) * (ey1 - y0))
        items = self.page_items(container, ids, area) if page else self.items(container, ids, extra, area)
        if not items:
            return
        weights = [it.weight or float(it.size) for it in items]
        if page and len(items) > 1 and items[-1].kind == RectKind.MORE:
            # the leftover gets a strip of its own: no member shares a squarify row with a far larger block
            body, rest = squarify([sum(weights[:-1]), weights[-1]], x0, y0, ex1, ey1)
            cells = [*squarify(weights[:-1], *body), rest]
        else:
            cells = squarify(weights, x0, y0, ex1, ey1)
        rows, names = self.rows, self.names
        gap, min_draw, dpr = g.gap, g.min_draw, g.dpr
        expandable = level < self.p.detail
        depth = min(level, 255)
        kind, flags, child_count, store = self.kind, self.flags, self.child_count, self.store
        for it, (fx0, fy0, fx1, fy1) in zip(items, cells, strict=True):
            bx0, by0 = round(fx0), round(fy0)
            bw, bh = round(fx1) - gap - bx0, round(fy1) - gap - by0
            if bw < min_draw or bh < min_draw:
                continue
            rk = int(it.kind)
            node = it.node
            if rk == FILE and kind[node] == DIR:
                if flags[node] & PENDING:
                    rk = PENDING_RECT
                elif expandable and bw >= g.expand_w and bh >= g.expand_h and child_count[node] > 0:
                    rk = FOLDER
                else:
                    rk = COLLAPSED
            if rk == FOLDER:
                lbl = L_NAME | L_SIZE
            else:  # UI-TM-14..16, in CSS px
                cw, ch = bw / dpr, bh / dpr
                if cw < 30 or ch < 15:
                    lbl = 0
                elif ch < 22:
                    lbl = L_NAME | L_SHORT
                else:
                    lbl = (
                        L_NAME | (L_SIZE if cw >= 42 and ch >= 31 else 0) | (L_BIG if cw * ch > 52_000 else 0)
                    )
            row = len(rows)
            rows.append(
                (it.key, node, parent_row, rk, depth, bx0, by0, bw, bh, it.cls, lbl, it.size, it.count)
            )
            names.append(store.name(node) if lbl and rk <= PENDING_RECT else "")
            if rk == FOLDER:
                cx0, cy0 = bx0 + g.border + g.pad, by0 + g.border + g.header
                cx1, cy1 = bx0 + bw - g.border - g.pad, by0 + bh - g.border - g.pad
                if cx1 - cx0 >= g.min_content and cy1 - cy0 >= g.min_content:
                    self.container(node, self.children(node), [], (cx0, cy0, cx1, cy1), level + 1, row)

    def result(self, width: int, height: int, elapsed: float) -> DrawList:
        table = np.array(self.rows, dtype=np.int64).reshape(-1, len(_COLUMNS))
        arrays = {name: table[:, i].astype(dt) for i, (name, dt) in enumerate(_COLUMNS)}
        return DrawList(
            **arrays,
            names=self.names,
            members=self.members,
            width=width,
            height=height,
            elapsed_s=elapsed,
        )


_COLUMNS: tuple[tuple[str, type[np.generic]], ...] = (
    ("key", np.uint32),
    ("node", np.int32),
    ("parent", np.int32),
    ("kind", np.uint8),
    ("depth", np.uint8),
    ("x", np.int32),
    ("y", np.int32),
    ("w", np.int32),
    ("h", np.int32),
    ("cls", np.uint8),
    ("label", np.uint8),
    ("size", np.int64),
    ("count", np.uint32),
)

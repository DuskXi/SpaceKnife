"""The row contract every layout engine keeps (docs/protocol.md §4.1), checked on one draw list.

:func:`check_draw` returns the broken rules, worded for a test failure; an empty list means the draw
list is fine. The rules:

1. parents come first: ``parent[i] < i``; a parent row is an expanded folder one level up;
2. keys are unique;
3. rows of kind ≤ PENDING are real nodes (key = node id, file / folder / pending as in the store);
   synthetic rows carry their tagged keys;
4. a node belongs to one aggregate block at most and is then not a row itself; a block's count is
   its number of members;
5. (``full``) every node below the zoom root is a row, a member of an aggregate block or below a
   drawn row, except blocks too small to draw: a node left out spans at most a few pixels along its
   container (``SLACK_PX``; in a partition two gaps); a partition draws a file root, or a root
   with no band or ring that fits, from ``FrameRoot`` alone and has no rows; every row's size is
   its nodes' bytes;
6. only real rows with label bits have a name, and it is the node's name;
7. partition layouts (flame.v1, polar.v1) have at most ``MAX_ROWS`` rows;
8. the geometry matches the layout: its columns (i4, one value per row) and ``meta``; blocks lie on
   the canvas and inside their parent; in a partition the rows of one level share their band or
   ring and follow each other along it without overlapping.
"""

from __future__ import annotations

from collections.abc import Callable

import numpy as np

from spaceknife_protocol import view as pv

from ..store.nodes import Flag, Kind, NodeStore
from .api import MAX_ROWS, RootSpec, SizeSource
from .basis import Basis
from .rows import (
    COMMON_COLUMNS,
    FOLDER,
    GEOMETRY_COLUMNS,
    KEY_FREE,
    KEY_MORE,
    KEY_NODE_MASK,
    KEY_SMALL,
    KEY_UNKNOWN,
    PENDING_RECT,
    DrawList,
    RectKind,
)

SLACK_PX = 20  # rule 5, treemap: what a block too small to draw spans at most along its container
SHOWN = 5  # offending rows quoted per problem

_TAG = ~KEY_NODE_MASK & 0xFFFFFFFF


def check_draw(
    draw: DrawList, store: NodeStore, root: RootSpec, *, full: bool = True, sizes: SizeSource | None = None
) -> list[str]:
    """Problems of ``draw``, laid out for ``root`` with ``sizes`` (None: the store's own sizes).

    ``full`` adds rule 5, which walks the children of every expanded row.
    """
    problems: list[str] = []
    if not _well_formed(draw, problems):
        return problems
    n = len(draw)

    def report(rule: int, what: str, rows: np.ndarray) -> None:
        bad = np.flatnonzero(rows) if rows.dtype == bool else np.asarray(rows, dtype=np.int64)
        if bad.size:
            more = f" (+{bad.size - SHOWN} more)" if bad.size > SHOWN else ""
            problems.append(f"rule {rule}: {what}: rows {bad[:SHOWN].tolist()}{more}")

    key = draw.key.astype(np.int64)
    node = draw.node.astype(np.int64)
    parent = draw.parent.astype(np.int64)
    kind = draw.kind.astype(np.int64)
    depth = draw.depth.astype(np.int64)
    rows = np.arange(n, dtype=np.int64)
    top = parent == -1
    inner = (parent >= 0) & (parent < rows)
    real = kind <= PENDING_RECT
    nodes = len(store)

    # 1. parents first
    report(1, "parent is not an earlier row", ~top & ~inner)
    up = np.where(inner, parent, 0)
    report(1, "parent row is not an expanded folder", inner & (kind[up] != FOLDER))
    want_depth = np.where(inner, np.minimum(depth[up] + 1, 255), 1)
    report(1, "depth is not one below the parent's", (top | inner) & (depth != want_depth))

    # 2. unique keys
    _, first, counts = np.unique(key, return_index=True, return_counts=True)
    dup = np.ones(n, dtype=bool)
    dup[first] = False
    report(2, f"{int(np.count_nonzero(counts > 1))} keys repeat", dup)

    # 3. real nodes, tagged synthetic keys
    in_store = real & (node >= 1) & (node < nodes)
    report(3, "real row without a node of the store", real & ~in_store)
    report(3, "real row keyed other than by its node", in_store & (key != node))
    safe = np.where(in_store, node, 0)
    c = store.cols
    is_dir = c["kind"][safe] == Kind.DIR
    pending = (c["flags"][safe] & np.uint32(Flag.PENDING)) != 0
    report(3, "file row of a folder", in_store & (kind == RectKind.FILE) & is_dir)
    report(3, "folder row of a file", in_store & (kind != RectKind.FILE) & ~is_dir)
    report(3, "pending row of a listed folder", in_store & (kind == RectKind.PENDING) & ~pending)
    report(
        3, "folder row of a folder not listed yet", in_store & (kind != RectKind.PENDING) & is_dir & pending
    )
    tag = key & _TAG
    report(3, "unknown kind", kind > RectKind.UNKNOWN)
    small, more = kind == RectKind.SMALL, kind == RectKind.MORE
    report(
        3,
        "SMALL row keyed other than KEY_SMALL | container",
        small & (key != (KEY_SMALL | (node & KEY_NODE_MASK))),
    )
    report(3, "MORE row without a KEY_MORE key", more & (tag != KEY_MORE))
    free, unknown = kind == RectKind.FREE, kind == RectKind.UNKNOWN
    report(3, "FREE row keyed other than KEY_FREE on node -1", free & ((key != KEY_FREE) | (node != -1)))
    report(
        3,
        "UNKNOWN row keyed other than KEY_UNKNOWN on node -1",
        unknown & ((key != KEY_UNKNOWN) | (node != -1)),
    )

    # 4. aggregate blocks
    blocks = {int(k): np.asarray(m, dtype=np.int64) for k, m in draw.members.items()}
    odd = [k for k in blocks if k & _TAG not in (KEY_SMALL, KEY_MORE)]
    if odd:
        problems.append(f"rule 4: member blocks with keys that are not SMALL / MORE: {odd[:SHOWN]}")
    every = np.concatenate([np.empty(0, dtype=np.int64), *blocks.values()])
    if every.size:
        if np.unique(every).size != every.size:
            problems.append("rule 4: a node is a member of more than one block (or twice of one)")
        if ((every < 1) | (every >= nodes)).any():
            problems.append("rule 4: a member is not a node of the store")
        report(4, "row of a node that is also a member", real & np.isin(node, every))
    agg = small | more
    lost = np.array([i for i in np.flatnonzero(agg).tolist() if int(key[i]) not in blocks], dtype=np.int64)
    report(4, "aggregate row without its members", lost)
    counted = np.array(
        [
            i
            for i in np.flatnonzero(agg).tolist()
            if int(key[i]) in blocks and int(draw.count[i]) != len(blocks[int(key[i])])
        ],
        dtype=np.int64,
    )
    report(4, "aggregate row whose count is not its number of members", counted)

    # 5. coverage and sizes
    if full and not any(p.startswith(("rule 1", "rule 3")) for p in problems):
        _coverage(draw, store, root, sizes, blocks, every, report, problems)

    # 6. names
    names = np.array(
        [
            draw.names[i] != (store.name(int(node[i])) if draw.label[i] and in_store[i] else "")
            for i in range(n)
        ],
        dtype=bool,
    )
    report(6, "name is not the node's (labelled real rows) or empty (the rest)", names)

    # 7. row budget
    if draw.layout != pv.DRAWLIST_LAYOUT and n > MAX_ROWS:
        problems.append(f"rule 7: {n} rows, more than {MAX_ROWS}")

    # 8. geometry
    _geometry(draw, parent, inner, depth, report)
    return problems


def _well_formed(draw: DrawList, problems: list[str]) -> bool:
    """Columns of the right type and length, the layout's geometry and meta (part of rule 8)."""
    n = len(draw)
    for name, dt in COMMON_COLUMNS:
        col = getattr(draw, name)
        if col.dtype != np.dtype(dt) or col.shape != (n,):
            problems.append(f"rule 8: column {name} is {col.dtype} × {col.shape}, not {np.dtype(dt)} × {n}")
    if len(draw.names) != n:
        problems.append(f"rule 8: {len(draw.names)} names for {n} rows")
    want = GEOMETRY_COLUMNS.get(draw.layout)
    if want is None:
        problems.append(f"rule 8: unknown layout {draw.layout!r}")
        return False
    if sorted(draw.geometry) != sorted(want):
        problems.append(f"rule 8: geometry columns {sorted(draw.geometry)}, {draw.layout} has {list(want)}")
    for name, col in draw.geometry.items():
        if col.dtype != np.dtype(np.int32) or col.shape != (n,):
            problems.append(f"rule 8: column {name} is {col.dtype} × {col.shape}, not int32 × {n}")
    meta: dict[str, type] = {
        pv.DRAWLIST_LAYOUT: type(None),
        pv.FLAME_LAYOUT: pv.FlameGeometry,
        pv.POLAR_LAYOUT: pv.PolarGeometry,
    }
    if not isinstance(draw.meta, meta[draw.layout]):
        problems.append(
            f"rule 8: meta is {type(draw.meta).__name__}, {draw.layout} has {meta[draw.layout].__name__}"
        )
    return not problems


def _coverage(
    draw: DrawList,
    store: NodeStore,
    root: RootSpec,
    sizes: SizeSource | None,
    blocks: dict[int, np.ndarray],
    every: np.ndarray,
    report: Callable[[int, str, np.ndarray], None],
    problems: list[str],
) -> None:
    n_nodes = len(store) if sizes is None else min(len(store), sizes.n)

    def size_of(ids: np.ndarray) -> np.ndarray:
        out = Basis.effective_sizes_of(store, ids) if sizes is None else sizes.of(ids)
        return np.asarray(out, dtype=np.int64)

    def children(of: int) -> np.ndarray:
        start, stop = store.child_range(of)
        return np.arange(start, min(stop, n_nodes), dtype=np.int64)

    node = draw.node.astype(np.int64)
    kind = draw.kind
    real = kind <= PENDING_RECT
    agg = (kind == RectKind.SMALL) | (kind == RectKind.MORE)
    # sizes
    report(5, "size is not the node's", real & (draw.size != size_of(np.where(real, node, 0))))
    wrong = [
        i
        for i in np.flatnonzero(agg).tolist()
        if int(draw.key[i]) in blocks and int(draw.size[i]) != int(size_of(blocks[int(draw.key[i])]).sum())
    ]
    report(5, "aggregate size is not its members' bytes", np.array(wrong, dtype=np.int64))
    report(5, "free space other than the root's", (kind == RectKind.FREE) & (draw.size != (root.free or 0)))
    report(
        5,
        "unknown space other than the root's",
        (kind == RectKind.UNKNOWN) & (draw.size != (root.unknown or 0)),
    )
    # coverage, container by container: the zoom root, then every expanded folder
    extra = (root.free or 0) + max(0, root.unknown or 0)
    is_dir = root.members is not None or store.cols["kind"][root.node] == Kind.DIR
    containers: list[tuple[int, np.ndarray, int]]
    if draw.meta is not None and (not is_dir or draw.levels == 0):
        # partitions draw a file root, and a root with no band or ring to fill, from FrameRoot alone
        containers = []
    elif root.members is not None:
        containers = [(-1, np.asarray(root.members, dtype=np.int64), 0)]
    elif is_dir:
        containers = [(-1, children(root.node), int(size_of(np.array([root.node]))[0]))]
    else:
        containers = [(-1, np.array([root.node], dtype=np.int64), 0)]
        extra = 0
    containers += [
        (r, children(int(node[r])), int(draw.size[r])) for r in np.flatnonzero(kind == FOLDER).tolist()
    ]
    slack = SLACK_PX if draw.meta is None else 2 * draw.meta.gap + 2
    for row, ids, own in containers:
        under = draw.parent == row
        drawn = node[under & real]
        report(
            5, f"row outside its container (row {row})", np.flatnonzero(under & real)[~np.isin(drawn, ids)]
        )
        outside = [
            i
            for i in np.flatnonzero(under & agg).tolist()
            if not np.isin(blocks.get(int(draw.key[i]), ids), ids).all()
        ]
        report(5, f"aggregate members outside its container (row {row})", np.array(outside, dtype=np.int64))
        s = size_of(ids)
        missing = (s > 0) & ~np.isin(ids, drawn) & ~np.isin(ids, every)
        if not missing.any():
            continue
        # engines share the container out by max(its children's bytes, its own size): span per byte
        basis = max(int(s[s > 0].sum()) + (extra if row < 0 else 0), own)
        span = s[missing] / basis * _extent(draw, row, under)
        if (span > slack).any():
            where = "the zoom root" if row < 0 else f"row {row}"
            problems.append(
                f"rule 5: nodes of {where} are neither rows nor members though they span up to "
                f"{float(span.max()):.0f} px: {ids[missing][span > slack][:SHOWN].tolist()}"
            )


def _extent(draw: DrawList, row: int, under: np.ndarray) -> float:
    """Pixels along which ``row`` (-1: the zoom root) shares out its children; never more than that.

    Treemap: the short side of the drawn children's bounding box (the content box when unknown).
    Flame graph: the bar's width. Polar: the arc of the children's ring.
    """
    g = draw.geometry
    meta = draw.meta
    if isinstance(meta, pv.PolarGeometry):
        level = 0 if row < 0 else int(draw.depth[row])
        turn = 1.0 if row < 0 else (int(g["a1"][row]) - int(g["a0"][row])) / meta.unit
        return turn * 2 * np.pi * (meta.hole + (level + 0.5) * meta.ring_w)
    if isinstance(meta, pv.FlameGeometry):
        return float(draw.width if row < 0 else int(g["w"][row]))
    if row < 0:
        return float(min(draw.width, draw.height))
    if not under.any():
        return 0.0  # nothing drawn inside: the content box is unknown
    x, y, w, h = (g[k][under].astype(np.int64) for k in ("x", "y", "w", "h"))
    return float(min(int((x + w).max() - x.min()), int((y + h).max() - y.min())))


def _geometry(
    draw: DrawList,
    parent: np.ndarray,
    inner: np.ndarray,
    depth: np.ndarray,
    report: Callable[[int, str, np.ndarray], None],
) -> None:
    up = np.where(inner, parent, 0)
    g = {name: col.astype(np.int64) for name, col in draw.geometry.items()}
    if draw.layout in (pv.DRAWLIST_LAYOUT, pv.FLAME_LAYOUT):
        x, y, w, h = g["x"], g["y"], g["w"], g["h"]
        report(8, "empty block", (w <= 0) | (h <= 0))
        report(8, "block off the canvas", (x < 0) | (y < 0) | (x + w > draw.width) | (y + h > draw.height))
        report(8, "block sticks out of its parent", inner & ((x < x[up]) | (x + w > x[up] + w[up])))
        if draw.layout == pv.DRAWLIST_LAYOUT:
            report(8, "block sticks out of its parent", inner & ((y < y[up]) | (y + h > y[up] + h[up])))
        else:
            _levels(depth, x, x + w, (y, h), report)
    else:
        a0, a1, r0, r1 = g["a0"], g["a1"], g["r0"], g["r1"]
        unit = draw.meta.unit if isinstance(draw.meta, pv.PolarGeometry) else pv.ANGLE_UNIT
        report(8, "empty or out-of-turn sector", (a0 < 0) | (a1 <= a0) | (a1 > unit))
        report(8, "empty ring", (r0 < 0) | (r1 <= r0))
        report(8, "sector sticks out of its parent", inner & ((a0 < a0[up]) | (a1 > a1[up]) | (r0 < r1[up])))
        _levels(depth, a0, a1, (r0, r1), report)


def _levels(
    depth: np.ndarray,
    start: np.ndarray,
    end: np.ndarray,
    band: tuple[np.ndarray, np.ndarray],
    report: Callable[[int, str, np.ndarray], None],
) -> None:
    """Partition layouts: one band / ring per level, its rows in order along it without overlaps."""
    for d in np.unique(depth).tolist():
        rows = np.flatnonzero(depth == d)
        for col in band:
            report(8, f"level {d} is not one band", rows[col[rows] != col[rows[0]]])
        report(8, f"level {d} is out of order or overlaps", rows[1:][start[rows[1:]] < end[rows[:-1]]])

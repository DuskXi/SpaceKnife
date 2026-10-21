"""Which blocks a container shows: the small-item and "+N items" merges (G5) and aggregate zoom pages.

Shared by every layout engine; each one says, through the byte threshold, what "too small" means in
its own geometry (an area share in the treemap, a length share in bars and rings).
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass

import numpy as np

from ..store.nodes import Kind, NodeStore
from .basis import Basis
from .rows import KEY_FREE, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, PAGE_REST, RectKind


@dataclass(slots=True)
class Item:
    """One block of a container, in drawing order, before it is placed."""

    key: int
    node: int
    kind: RectKind
    size: int
    cls: int
    count: int
    weight: float = 0.0  # area basis when it is not ``size`` (a shrunk "+N items")


@dataclass(slots=True)
class MergeCtx:
    """One layout run's merge state: the member blocks found so far."""

    store: NodeStore
    basis: Basis
    sort_by_size: bool
    members: dict[int, np.ndarray]  # SMALL/MORE key -> node ids (DrawList.members)
    avoid: int = -1  # aggregate zoom: key of the block zoomed into


def merge_items(
    ctx: MergeCtx,
    container: int,
    ids: np.ndarray,
    s: np.ndarray,
    extra: list[tuple[int, int]],
    thr_of: Callable[[float], float],
) -> list[Item]:
    """Ordered items of one container after the small-item and "+N" merges.

    ``s`` holds the sizes of ``ids``; ``extra`` the free / unknown space blocks as (key, bytes);
    ``thr_of(total)`` the byte size below which an item is too small, for the container's total.
    Only blocks that will be drawn become Python objects; everything too small is counted and
    merged with vectorised operations, so a huge folder costs one numpy pass.
    """
    keep = s > 0
    ids, s = ids[keep], s[keep]
    total = float(s.sum()) + sum(v for _, v in extra)
    if total <= 0:
        return []
    thr = thr_of(total)  # bytes below → too small
    below = s < thr
    is_file = ctx.store.cols["kind"][ids] != Kind.DIR
    small = below & is_file
    tag = container & KEY_NODE_MASK
    basis = ctx.basis
    agg: Item | None = None
    n_small = int(np.count_nonzero(small))
    if n_small >= 2:
        sm_ids, sm_s = ids[small], s[small]
        by_cls = np.bincount(basis.cls_of(sm_ids), weights=sm_s, minlength=len(basis.class_ids))
        top = int(np.argmax(by_cls))
        sm_total = int(sm_s.sum())
        cls = top if by_cls[top] * 2 >= sm_total else basis.other
        agg = Item(KEY_SMALL | tag, container, RectKind.SMALL, sm_total, cls, n_small)
        ctx.members[KEY_SMALL | tag] = sm_ids
        rest = ~small
    else:
        rest = np.ones(len(ids), dtype=bool)
    # "+N items": everything still below the threshold (folders, a lone small file, the aggregate)
    tiny = rest & below
    agg_tiny = agg is not None and agg.size < thr
    n_tiny = int(np.count_nonzero(tiny)) + (1 if agg_tiny else 0)
    more: Item | None = None
    if n_tiny >= 2:
        t_ids, t_s = ids[tiny], s[tiny]
        members = [t_ids]
        size, count = int(t_s.sum()), len(t_ids)
        if agg is not None and agg_tiny:
            members.append(ctx.members.pop(KEY_SMALL | tag))
            size, count = size + agg.size, count + agg.count
            agg = None
        ctx.members[KEY_MORE | tag] = np.concatenate(members)
        more = Item(KEY_MORE | tag, container, RectKind.MORE, size, basis.other, count)
        rest &= ~tiny
    ids, s = ids[rest], s[rest]
    if ctx.sort_by_size:
        order = np.argsort(-s, kind="stable")
    else:
        names = [ctx.store.name(int(i)).casefold() for i in ids]
        order = np.array(sorted(range(len(ids)), key=names.__getitem__), dtype=np.int64)
    classes = basis.cls_of(ids)
    out = [Item(int(ids[i]), int(ids[i]), RectKind.FILE, int(s[i]), int(classes[i]), 1) for i in order]
    for key, size in extra:
        out.append(Item(key, -1, RectKind.FREE if key == KEY_FREE else RectKind.UNKNOWN, size, 0, 0))
    if agg is not None:
        out.append(agg)
    if ctx.sort_by_size and (extra or agg is not None):
        out.sort(key=lambda it: -it.size)  # stable: equal sizes keep their order
    if more is not None:
        out.append(more)
    return out


def page_items(ctx: MergeCtx, container: int, ids: np.ndarray, s: np.ndarray, need: float) -> list[Item]:
    """Items of an aggregate zoom: the largest members one by one, the rest as one "+N items".

    ``s`` holds the sizes of ``ids``; ``need`` is the share of the view one member needs to be
    drawn on its own (the minimum element size). Members are drawn on their own while each still
    gets it. When the leftover's share of the view is what keeps them merged, that block is shrunk
    to ``PAGE_REST`` of the view, so at least the largest member always gets through and zooming
    into the leftover again shows the next members.
    """
    keep = s > 0
    ids, s = ids[keep], s[keep]
    n = len(ids)
    if n == 0:
        return []
    order = np.lexsort((ids, -s))  # largest first, ties by id: a stable representative
    ids, s = ids[order], s[order]
    sf = s.astype(np.float64)
    total = float(sf.sum())
    fits = sf >= need * total  # proportional: a prefix, as sizes descend
    prefix = np.cumsum(sf)
    basis = np.minimum(total, prefix / (1 - PAGE_REST))  # area basis with the leftover shrunk
    shrunk = sf >= need * basis
    k_prop = n if fits.all() else int(np.argmin(fits))
    k = max(k_prop, n if shrunk.all() else int(np.argmin(shrunk)), 1)
    if n - k == 1:
        k = n  # merging needs at least two members
    page, page_s = ids[:k], s[:k]
    if not ctx.sort_by_size:
        names = [ctx.store.name(int(i)).casefold() for i in page]
        by_name = np.array(sorted(range(k), key=names.__getitem__), dtype=np.int64)
        page, page_s = page[by_name], page_s[by_name]
    out = [
        Item(i, i, RectKind.FILE, z, c, 1)
        for i, z, c in zip(page.tolist(), page_s.tolist(), ctx.basis.cls_of(page).tolist(), strict=True)
    ]
    if k < n:
        rest = ids[k:]
        key = KEY_MORE | (int(rest[0]) & KEY_NODE_MASK)
        if key == ctx.avoid:  # sizes changed since that zoom: the next member represents it
            key = KEY_MORE | (int(rest[1]) & KEY_NODE_MASK)
        weight = float((total if k == k_prop else basis[k - 1]) - prefix[k - 1])
        ctx.members[key] = rest
        out.append(Item(key, container, RectKind.MORE, int(s[k:].sum()), ctx.basis.other, len(rest), weight))
    return out

"""Changes to a scanned tree after the scan (M5 file change hooks): docs/monitor.md §4.

The store stays append-only: ids are never reused and a row's name is never rewritten (the name
index and the filter's level cache rely on it). A file whose size or times changed is updated in
place. A folder whose entries changed is listed again and gets a *new* child range at the end of
the store: entries that are still there are copied over (a folder with its own child range and
totals; its children are re-parented), new entries get new rows, and the old range — plus every
subtree that disappeared — is marked ``GONE``: depth 0, no parent, no size, outside every child
range, so tree walks never reach it and whole-store scans mask it out. :func:`compact` renumbers
the store once too much of it is ``GONE``.

Every function here must run on the event loop thread inside ``STRUCTURE.write()`` (see
:mod:`.lock`): readers on the compute pool never see a half-moved folder.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from .nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore
from .subtree import ranges_to_ids

TOTALS = ("tot_size", "tot_alloc", "tot_files", "tot_dirs")
# state a row carries over when it is copied to a new id (the attributes come from the new listing)
_KEEP = np.uint32(Flag.PENDING | Flag.ACCESS_DENIED | Flag.ERROR | Flag.HARDLINK_DUP)
COMPACT_RATIO = 0.25  # compact once GONE rows are this share of the store ...
COMPACT_MIN = 100_000  # ... and at least this many


@dataclass(frozen=True, slots=True)
class Relist:
    """What :func:`relist` did to one folder."""

    start: int  # first id of the folder's new child range
    moved_from: np.ndarray  # old ids of entries that are still there ...
    moved_to: np.ndarray  # ... and their new ids
    removed: np.ndarray  # old ids of entries that disappeared (their subtrees are GONE as well)
    removed_alloc: np.ndarray  # what each of them occupied (subtree total for folders)
    added: np.ndarray  # new ids of entries that are new
    pending: np.ndarray  # the new folders among them: still to be scanned
    grown: np.ndarray  # new ids of kept files whose allocation changed ...
    grown_by: np.ndarray  # ... and by how much (bytes, signed)
    gone: int  # rows marked GONE by this change


def is_gone(flags: np.ndarray) -> np.ndarray:
    """Bool per row: deleted, or the stale copy of a moved row."""
    result: np.ndarray = (flags & np.uint32(Flag.GONE)) != 0
    return result


def gone_count(store: NodeStore) -> int:
    return int(np.count_nonzero(is_gone(store.cols["flags"][: len(store)])))


def update_stats(
    store: NodeStore,
    nodes: np.ndarray,
    size: np.ndarray,
    alloc: np.ndarray,
    mtime: np.ndarray | None = None,
) -> np.ndarray:
    """Set new sizes (and times) of files in place; every ancestor's totals follow. Returns the
    change of allocation per node (0 for hard-link duplicates, which never count)."""
    c = store.cols
    nodes = np.asarray(nodes, dtype=np.int64)
    if not len(nodes):
        return np.zeros(0, dtype=np.int64)
    size = np.asarray(size, dtype=np.int64)
    alloc = np.asarray(alloc, dtype=np.int64)
    counted = (c["flags"][nodes] & np.uint32(Flag.HARDLINK_DUP)) == 0
    d_size = np.where(counted, size - c["size"][nodes], 0)
    d_alloc = np.where(counted, alloc - c["alloc"][nodes], 0)
    c["size"][nodes] = size
    c["alloc"][nodes] = alloc
    if mtime is not None:
        c["mtime"][nodes] = np.clip(np.asarray(mtime, dtype=np.int64), 0, 2**32 - 1)
    _climb(
        store,
        c["parent"][nodes].astype(np.int64),
        np.stack([d_size, d_alloc], axis=1),
        ("tot_size", "tot_alloc"),
    )
    store.version += 1
    return d_alloc


def relist(
    store: NodeStore,
    folder: int,
    name_blob: bytes,
    name_lens: np.ndarray,
    entries: dict[str, np.ndarray],
    ext_ids: np.ndarray | None = None,
) -> Relist:
    """Replace a listed folder's children by a fresh listing of it (same columns as a scan).

    Entries are matched by name; a name that changed between file and folder counts as removed
    and added. New folders are ``PENDING`` (to be scanned) unless they are mount points. New files
    take ``HARDLINK_DUP`` from ``entries`` (the caller decides), kept ones keep theirs.

    Kept entries change ids. While a scan runs, its queue names ``PENDING`` folders by id: do not
    relist a folder with a ``PENDING`` child before the scan is done (see :func:`can_relist`).
    """
    c = store.cols
    if c["kind"][folder] != Kind.DIR or c["flags"][folder] & np.uint32(Flag.GONE):
        raise ValueError(f"node {folder} is not a folder of the tree")
    lo, hi = store.child_range(folder)
    old_ids = np.arange(lo, hi, dtype=np.int64)
    lengths = np.asarray(name_lens, dtype=np.int64)
    count = len(lengths)
    ends = np.cumsum(lengths)
    names = [name_blob[int(e - n) : int(e)] for e, n in zip(ends.tolist(), lengths.tolist(), strict=True)]
    kind = np.asarray(entries["kind"]).astype(np.uint8)
    by_name = {store.name_bytes(int(i)): int(i) for i in old_ids}
    old_kind = c["kind"]
    kept_old = np.full(count, -1, dtype=np.int64)
    for k, name in enumerate(names):
        o = by_name.get(name)
        if o is not None and (old_kind[o] == Kind.DIR) == (kind[k] == Kind.DIR):
            kept_old[k] = o
    kept = kept_old >= 0
    removed = np.setdiff1d(old_ids, kept_old[kept], assume_unique=True)
    removed_alloc = np.where(
        old_kind[removed] == Kind.DIR,
        c["tot_alloc"][removed],
        np.where(_counted(c, removed), c["alloc"][removed], 0),
    ).astype(np.int64)
    dead_below = _descendants(store, removed)  # before anything moves

    n0 = len(store)
    if count:
        store._grow(count)
        c = store.cols
    sl = slice(n0, n0 + count)
    new_ids = np.arange(n0, n0 + count, dtype=np.int64)
    c["parent"][sl] = folder
    c["depth"][sl] = int(c["depth"][folder]) + 1
    for key in ENTRY_COLUMNS:
        c[key][sl] = entries[key]
    c["ext"][sl] = ext_ids if ext_ids is not None else 0
    for key in (*TOTALS, "child_start", "child_count"):
        c[key][sl] = 0
    if count:
        offsets = store._heap.append(name_blob, lengths)
        store._fit_offsets(int(offsets[-1] + lengths[-1]))
        c = store.cols
        c["name_off"][sl] = offsets
        c["name_len"][sl] = lengths
    to_new, from_old = new_ids[kept], kept_old[kept]
    grown_mask = (kind[kept] != Kind.DIR) & (c["alloc"][to_new] != c["alloc"][from_old])
    grown, grown_by = to_new[grown_mask], (c["alloc"][to_new] - c["alloc"][from_old])[grown_mask]
    grown_by = np.where(_counted(c, from_old[grown_mask]), grown_by, 0)
    c["flags"][to_new] |= c["flags"][from_old] & _KEEP
    folders = to_new[kind[kept] == Kind.DIR]
    folders_old = from_old[kind[kept] == Kind.DIR]
    for key in ("child_start", "child_count", *TOTALS):
        c[key][folders] = c[key][folders_old]
    starts = c["child_start"][folders_old].astype(np.int64)
    counts = c["child_count"][folders_old].astype(np.int64)
    c["parent"][ranges_to_ids(starts, counts)] = np.repeat(folders, counts)  # grandchildren
    added = new_ids[~kept]
    enter = (kind[~kept] == Kind.DIR) & ((c["flags"][added] & np.uint32(Flag.MOUNT)) == 0)
    pending = added[enter]
    c["flags"][pending] |= np.uint32(Flag.PENDING)

    before = np.array([int(c[k][folder]) for k in TOTALS], dtype=np.int64)
    store._n = n0 + count  # publish: rows, the store's count, then the folder's range
    c["child_start"][folder] = n0
    c["child_count"][folder] = count
    gone = np.concatenate([old_ids, dead_below])
    _mark_gone(store, gone)
    after = _own_totals(store, new_ids)
    for key, total in zip(TOTALS, after.tolist(), strict=True):
        c[key][folder] = total
    parent = int(c["parent"][folder])
    if parent >= 0:
        _climb(store, np.array([parent]), (after - before)[None, :], TOTALS)
    store.version += 1
    return Relist(
        start=n0,
        moved_from=from_old,
        moved_to=to_new,
        removed=removed,
        removed_alloc=removed_alloc,
        added=added,
        pending=pending,
        grown=grown,
        grown_by=grown_by.astype(np.int64),
        gone=len(gone),
    )


def can_relist(store: NodeStore, folder: int) -> bool:
    """A listed, live folder none of whose children still waits to be scanned (their ids are in a
    scan's queue; :func:`relist` would move them)."""
    c = store.cols
    if c["kind"][folder] != Kind.DIR or c["flags"][folder] & np.uint32(Flag.GONE | Flag.PENDING):
        return False
    lo, hi = store.child_range(folder)
    return not bool((c["flags"][lo:hi] & np.uint32(Flag.PENDING)).any())


def needs_compaction(store: NodeStore) -> bool:
    gone = gone_count(store)
    return gone >= COMPACT_MIN and gone >= COMPACT_RATIO * len(store)


def compact(store: NodeStore) -> tuple[NodeStore, np.ndarray]:
    """A copy of the live tree renumbered breadth first (children right after each other, every
    range after its folder again, no GONE rows) and ``remap``: new id per old id, -1 for GONE.

    Pure read of ``store`` (runs on the compute pool); the caller adopts the copy under
    ``STRUCTURE.write()`` once nothing changed in between.
    """
    c = store.cols
    n = len(store)
    order_parts = [np.array([0], dtype=np.int64)]
    level = order_parts[0]
    is_dir = c["kind"][:n] == Kind.DIR
    is_dir[0] = True
    while level.size:
        dirs = level[is_dir[level]]
        level = ranges_to_ids(
            c["child_start"][dirs].astype(np.int64), c["child_count"][dirs].astype(np.int64)
        )
        if level.size:
            order_parts.append(level)
    order = np.concatenate(order_parts)
    remap = np.full(n, -1, dtype=np.int64)
    remap[order] = np.arange(len(order), dtype=np.int64)
    cols = {key: col[:n][order] for key, col in c.items()}
    parent = cols["parent"].astype(np.int64)
    cols["parent"] = np.where(parent >= 0, remap[np.maximum(parent, 0)], -1).astype(c["parent"].dtype)
    listed = cols["child_count"] > 0
    start = cols["child_start"].astype(np.int64)
    cols["child_start"] = np.where(listed, remap[np.where(listed, start, 0)], 0).astype(
        c["child_start"].dtype
    )
    # names in the new order: one gather over the joined heap image
    image = np.frombuffer(store.names_blob, dtype=np.uint8)
    ln = cols["name_len"].astype(np.int64)
    off = store._heap.image_positions(cols["name_off"].astype(np.int64))
    names = image[ranges_to_ids(off, ln)].tobytes()
    cols["name_off"] = (np.cumsum(ln) - ln).astype(np.int64)
    out = NodeStore.from_columns(cols, names, store.ext_table)
    return out, remap


# --------------------------------------------------------------------------- helpers


def _counted(c: dict[str, np.ndarray], ids: np.ndarray) -> np.ndarray:
    result: np.ndarray = (c["flags"][ids] & np.uint32(Flag.HARDLINK_DUP)) == 0
    return result


def _descendants(store: NodeStore, roots: np.ndarray) -> np.ndarray:
    """Every node strictly below any of ``roots`` (folders' child ranges, level by level)."""
    c = store.cols
    n = len(store)
    found: list[np.ndarray] = []
    frontier = roots[c["kind"][roots] == Kind.DIR] if len(roots) else roots
    while frontier.size:
        count = c["child_count"][frontier].astype(np.int64)
        start = c["child_start"][frontier].astype(np.int64)
        ok = (count > 0) & (start >= 1) & (start + count <= n)
        ids = ranges_to_ids(start[ok], count[ok])
        if not ids.size:
            break
        found.append(ids)
        frontier = ids[c["kind"][ids] == Kind.DIR]
    return np.concatenate(found) if found else np.empty(0, dtype=np.int64)


def _mark_gone(store: NodeStore, ids: np.ndarray) -> None:
    c = store.cols
    c["flags"][ids] = np.uint32(Flag.GONE)
    c["kind"][ids] = Kind.OTHER
    c["parent"][ids] = -1
    c["depth"][ids] = 0
    for key in ("size", "alloc", "child_start", "child_count", *TOTALS):
        c[key][ids] = 0


def _own_totals(store: NodeStore, children: np.ndarray) -> np.ndarray:
    """A folder's four totals from its children (what NodeStore.reaggregate computes)."""
    c = store.cols
    kind = c["kind"][children]
    folder = kind == Kind.DIR
    counted = _counted(c, children)
    return np.array(
        [
            int(np.where(counted, c["size"][children], 0).sum() + c["tot_size"][children][folder].sum()),
            int(np.where(counted, c["alloc"][children], 0).sum() + c["tot_alloc"][children][folder].sum()),
            int(np.count_nonzero(kind == Kind.FILE) + c["tot_files"][children][folder].sum()),
            int(np.count_nonzero(folder) + c["tot_dirs"][children][folder].sum()),
        ],
        dtype=np.int64,
    )


def _climb(store: NodeStore, nodes: np.ndarray, values: np.ndarray, columns: tuple[str, ...]) -> None:
    """Add ``values[i]`` to ``columns`` of ``nodes[i]`` and of every ancestor of it."""
    c = store.cols
    parent = c["parent"]
    node, add = nodes.astype(np.int64), values.astype(np.int64)
    keep = node >= 0
    node, add = node[keep], add[keep]
    while len(node):
        for i, key in enumerate(columns):
            np.add.at(c[key], node, add[:, i])
        node = parent[node].astype(np.int64)
        keep = node >= 0
        node, add = node[keep], add[keep]


__all__ = [
    "COMPACT_MIN",
    "COMPACT_RATIO",
    "Relist",
    "can_relist",
    "compact",
    "gone_count",
    "is_gone",
    "needs_compaction",
    "relist",
    "update_stats",
]

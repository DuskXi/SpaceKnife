"""S0: candidates from the node store, without any IO (docs/dupes.md §2.1).

A candidate is a regular file of at least ``min_size`` bytes with no flag that rules it out (hard
link already counted, cloud placeholder, offline, error, gone, pending), outside the excluded trees
(protected and program folders unless ``include_system``, recycle bins, network volumes unless
``include_network``), whose size another candidate shares. Candidates of all scans in the scope
form one table (:data:`codes.CAND_DTYPE`, 40 bytes each) sorted by size, so each size group is a
contiguous run; :data:`GROUP_DTYPE` describes the runs.

Everything here reads the store under the structure lock of the caller (``server.compute``) in
slices of a million rows, so no temporary is as long as the store.
"""

from __future__ import annotations

import ntpath
import os
import posixpath
import sys
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Literal

import numpy as np

from ..ops.paths import current_rules
from ..scan.scanner import RootInfo
from ..store.nodes import Flag, Kind, NodeStore
from ..view.trash import find_bins
from . import codes as C
from .identity import is_network

Mode = Literal["all", "inside", "against", "file"]
SLICE = 1 << 20

_RULED_OUT = np.uint32(
    Flag.HARDLINK_DUP | Flag.CLOUD | Flag.OFFLINE | Flag.GONE | Flag.ERROR | Flag.PENDING | Flag.VIRTUAL
)
_FILE = np.uint8(Kind.FILE)

GROUP_DTYPE = np.dtype(
    {
        "names": ["size", "first", "count", "state"],
        "formats": ["<i8", "<i4", "<i4", "u1"],
        "offsets": [0, 8, 12, 16],
        "itemsize": 24,
    }
)
G_PENDING, G_ACTIVE, G_DONE = 0, 1, 2


@dataclass(slots=True)
class ScanInput:
    """One scan taking part: its id, store and roots (``ScanHandle`` provides all three)."""

    scan_id: str
    store: NodeStore
    roots: list[RootInfo]


@dataclass(slots=True)
class Scope:
    scans: list[ScanInput]
    mode: Mode = "all"
    scan: int = 0  # inside / against / file: which scan ``node`` belongs to
    node: int = 0
    min_size: int = C.DEFAULT_MIN_SIZE
    include_system: bool = False
    include_network: bool = False


@dataclass(slots=True)
class Excluded:
    small: int = 0
    cloud: int = 0
    system: int = 0
    network: int = 0
    bins: int = 0
    unique: int = 0  # eligible, but no other candidate has its size: read nothing, decided as unique

    def as_dict(self) -> dict[str, int]:
        return {
            "small": self.small,
            "cloud": self.cloud,
            "system": self.system,
            "network": self.network,
            "bins": self.bins,
            "unique": self.unique,
        }


@dataclass(slots=True)
class Selection:
    cand: np.ndarray  # CAND_DTYPE, sorted by (size, scan, node)
    groups: np.ndarray  # GROUP_DTYPE, one per size shared by two or more candidates
    root_of: np.ndarray  # u1 per candidate: index into ``roots[scan]``
    excluded: Excluded = field(default_factory=Excluded)
    unique_nodes: dict[int, np.ndarray] = field(default_factory=dict)  # per scan: eligible, size unique

    def __len__(self) -> int:
        return len(self.cand)


def _depth_one(store: NodeStore, nodes: np.ndarray) -> np.ndarray:
    """The depth-1 ancestor (the scan root) of each node."""
    parent = store.cols["parent"]
    depth = store.cols["depth"]
    cur = nodes.astype(np.int64)
    for _ in range(4096):
        up = depth[cur] > 1
        if not up.any():
            break
        cur = np.where(up, parent[cur], cur)
    return cur


def _below(store: NodeStore, nodes: np.ndarray, tops: np.ndarray) -> np.ndarray:
    """Whether each node is ``tops`` or below one of them (``tops`` sorted)."""
    out = np.zeros(len(nodes), dtype=bool)
    if not len(tops) or not len(nodes):
        return out
    parent = store.cols["parent"]
    cur = nodes.astype(np.int64)
    for _ in range(4096):
        live = cur > 0
        if not live.any():
            break
        hit = np.zeros(len(cur), dtype=bool)
        pos = np.searchsorted(tops, cur[live])
        pos = np.minimum(pos, len(tops) - 1)
        hit[live] = tops[pos] == cur[live]
        out |= hit
        cur = np.where(live & ~out, parent[np.maximum(cur, 0)], 0)
    return out


def excluded_trees(
    store: NodeStore, roots: Sequence[RootInfo], include_system: bool
) -> tuple[np.ndarray, np.ndarray]:
    """(protected and program trees, recycle bins) of one store as sorted node ids."""
    bins = np.asarray(find_bins(store), dtype=np.int64)
    if include_system:
        return np.zeros(0, dtype=np.int64), bins
    rules = current_rules()
    paths: set[str] = set()
    for key, reason in rules.tree.items():
        if reason not in ("trash",):
            paths.add(key)
    for key, reason in rules.exact.items():
        if reason in ("program_root", "data_root"):
            paths.add(key)  # Program Files, ProgramData: the whole tree, for duplicates
    for r in roots:
        base = r.path
        for name in rules.root_trees:
            if rules.windows and r.volume is not None and r.whole_volume:
                paths.add(ntpath.join(base, name))
    nodes: list[int] = []
    for p in paths:
        if not _inside_any(p, roots, rules.windows):
            continue
        n = store.find(p)
        if n is not None:
            nodes.append(n)
    # macOS: the folders the user chose not to enter (TCC) are not listed, so nothing to do here
    return np.array(sorted(set(nodes)), dtype=np.int64), bins


def _inside_any(path: str, roots: Sequence[RootInfo], windows: bool) -> bool:
    mod = ntpath if windows else posixpath
    p = mod.normcase(mod.normpath(path))
    for r in roots:
        base = mod.normcase(mod.normpath(r.path))
        if p == base or p.startswith(base.rstrip("\\/") + mod.sep):
            return True
    return False


def _network_roots(roots: Sequence[RootInfo]) -> np.ndarray:
    return np.array(sorted(r.node for r in roots if is_network(r.volume)), dtype=np.int64)


def select(scope: Scope) -> Selection:
    """S0 over every scan of the scope (call under STRUCTURE.read)."""
    min_size = max(1, int(scope.min_size))  # empty files are never duplicates worth listing
    ex = Excluded()
    parts: list[np.ndarray] = []
    sizes: list[np.ndarray] = []
    root_ix: list[np.ndarray] = []
    for si, sc in enumerate(scope.scans):
        store = sc.store
        n = len(store)
        cols = store.cols
        tops, bins = excluded_trees(store, sc.roots, scope.include_system)
        net = np.zeros(0, dtype=np.int64) if scope.include_network else _network_roots(sc.roots)
        root_nodes = np.array([r.node for r in sc.roots], dtype=np.int64)
        order = np.argsort(root_nodes)
        for a in range(0, n, SLICE):
            z = min(n, a + SLICE)
            kind = cols["kind"][a:z]
            flags = cols["flags"][a:z]
            size = cols["size"][a:z]
            files = kind == _FILE
            ok = files & ((flags & _RULED_OUT) == 0)
            ex.small += int((ok & (size < min_size) & (size > 0)).sum())
            ex.cloud += int(
                (files & ((flags & np.uint32(Flag.CLOUD | Flag.OFFLINE)) != 0) & (size >= min_size)).sum()
            )
            ok &= size >= min_size
            ids = np.flatnonzero(ok).astype(np.int64) + a
            if not len(ids):
                continue
            if len(tops):
                hit = _below(store, ids, tops)
                ex.system += int(hit.sum())
                ids = ids[~hit]
            if len(bins):
                hit = _below(store, ids, bins)
                ex.bins += int(hit.sum())
                ids = ids[~hit]
            if len(net):
                hit = _below(store, ids, net)
                ex.network += int(hit.sum())
                ids = ids[~hit]
            if not len(ids):
                continue
            top = _depth_one(store, ids)
            pos = np.searchsorted(root_nodes[order], top)
            pos = np.minimum(pos, len(order) - 1)
            known = root_nodes[order][pos] == top
            ids, pos = ids[known], pos[known]
            c = np.zeros(len(ids), dtype=C.CAND_DTYPE)
            c["node"] = ids
            c["scan"] = si
            c["group"] = -1
            parts.append(c)
            sizes.append(cols["size"][ids].astype(np.int64))
            root_ix.append(order[pos].astype(np.uint8))
    if not parts:
        return Selection(np.zeros(0, C.CAND_DTYPE), np.zeros(0, GROUP_DTYPE), np.zeros(0, np.uint8), ex)
    cand = np.concatenate(parts, dtype=C.CAND_DTYPE)  # (plain concatenation would drop the padding)
    size = np.concatenate(sizes)
    rix = np.concatenate(root_ix)
    order = np.lexsort((cand["node"], cand["scan"], size))
    cand, size, rix = cand[order], size[order], rix[order]
    # size groups with two or more members
    starts = np.flatnonzero(np.r_[True, size[1:] != size[:-1]])
    counts = np.diff(np.r_[starts, len(size)])
    multi = counts >= 2
    keep = np.repeat(multi, counts)
    ex.unique = int((~keep).sum())
    lone = cand[~keep]
    unique = {
        si: np.sort(lone["node"][lone["scan"] == si]).astype(np.int64) for si in range(len(scope.scans))
    }
    cand, size, rix = cand[keep], size[keep], rix[keep]
    # the modes narrow the groups
    if scope.mode != "all" and len(cand):
        st = scope.scans[scope.scan].store
        in_scan = cand["scan"] == scope.scan
        mine = np.zeros(len(cand), dtype=bool)
        if scope.mode == "file":
            want = int(st.cols["size"][scope.node])
            mine = size == want
        else:
            ids = cand["node"][in_scan].astype(np.int64)
            mine[in_scan] = _below(st, ids, np.array([scope.node], dtype=np.int64))
        if scope.mode == "inside":
            keep = mine
        else:  # against / file: every group with a member there, partners anywhere
            sizes_hit = np.unique(size[mine])
            keep = np.isin(size, sizes_hit)
        cand, size, rix = cand[keep], size[keep], rix[keep]
        if scope.mode == "inside" and len(cand):  # groups may have shrunk to one
            starts = np.flatnonzero(np.r_[True, size[1:] != size[:-1]])
            counts = np.diff(np.r_[starts, len(size)])
            keep = np.repeat(counts >= 2, counts)
            cand, size, rix = cand[keep], size[keep], rix[keep]
    starts = np.flatnonzero(np.r_[True, size[1:] != size[:-1]]) if len(size) else np.zeros(0, np.int64)
    counts = np.diff(np.r_[starts, len(size)]) if len(size) else np.zeros(0, np.int64)
    groups = np.zeros(len(starts), dtype=GROUP_DTYPE)
    groups["size"] = size[starts] if len(size) else 0
    groups["first"] = starts
    groups["count"] = counts
    cand["group"] = np.repeat(np.arange(len(starts), dtype=np.int32), counts)
    return Selection(cand, groups, rix, ex, unique)


class PathCache:
    """Paths of candidate nodes, built from cached folder paths (one ``store.path`` per folder,
    not per file). Cleared when the tree changes."""

    def __init__(self, store: NodeStore) -> None:
        self.store = store
        self._dirs: dict[int, str] = {}

    def clear(self) -> None:
        self._dirs.clear()

    def path(self, node: int) -> str:
        store = self.store
        parent = int(store.cols["parent"][node])
        d = self._dirs.get(parent)
        if d is None:
            d = store.path(parent)
            if len(self._dirs) > 1 << 18:
                self._dirs.clear()
            self._dirs[parent] = d
        return os.path.join(d, store.name(node))


def name_usable(path: str) -> bool:
    """Whether the path can be handed to the OS as listed (no lone surrogates, no separator inside
    a name on POSIX). Others are counted as unnameable and not read."""
    try:
        path.encode("utf-8")
    except UnicodeEncodeError:
        return False
    if sys.platform == "win32":
        from ..scan.worker import unnameable

        return not unnameable(ntpath.basename(path))
    return True


def plan(sel: Selection, *, n_scans: int) -> dict[str, object]:
    """What S2 and S3 would read at most, from the selection alone (no IO)."""
    g = sel.groups
    size = g["size"].astype(np.int64)
    count = g["count"].astype(np.int64)
    probe = np.array([C.probe_bytes(int(s)) for s in size], dtype=np.int64) if len(size) <= 200_000 else None
    probe_total = int((probe * count).sum()) if probe is not None else int((count * 2 * C.BLOCK).sum())
    return {
        "candidates": int(count.sum()),
        "groups": len(g),
        "bytes": int((size * count).sum()),
        "probe_bytes": probe_total,
        "full_bytes_max": int((size * count).sum()),
        "reclaim_max": int((size * (count - 1)).sum()),
        "excluded": sel.excluded.as_dict(),
        "scans": n_scans,
    }

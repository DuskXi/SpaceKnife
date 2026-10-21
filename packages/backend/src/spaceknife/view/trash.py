"""Which blocks are in a recycle bin / trash (row flag ``F_TRASH``: the views draw them apart, and the
bin's own block says how much emptying it frees).

The bins, by path (any platform's, so a snapshot from another system is read the same):

- Windows: ``<drive>:\\$Recycle.Bin`` (and ``RECYCLER`` / ``RECYCLED`` of old systems and FAT
  drives), at the top of a drive or share;
- macOS: ``/Users/<name>/.Trash`` and ``.Trashes`` at the top of a volume;
- Linux (freedesktop.org Trash specification): ``…/.local/share/Trash`` of a home folder (or
  ``$XDG_DATA_HOME/Trash``), ``.Trash-<uid>`` at the top of a mount, and the shared ``.Trash`` at the
  top of a scanned mount.

Only the rows of a frame and their ancestors are looked at, never the whole tree: a folder whose
name has the length of a bin's name is a candidate, and only candidates are named and matched. What
a store's nodes were found to be is kept per store version.
"""

from __future__ import annotations

import os
import re
import sys
import threading
import time
from weakref import WeakKeyDictionary

import numpy as np

from ..layout.api import SizeSource
from ..store.nodes import Kind, NodeStore

# byte lengths of the bins' folder names: Trash, .Trash, .Trashes / RECYCLER / RECYCLED,
# $Recycle.Bin, .Trash-<uid> (up to 10 digits)
_LENGTHS = np.array([5, 6, 8, 12, *range(8, 18)], dtype=np.int64)
_NAMES = frozenset({"trash", ".trash", ".trashes", "$recycle.bin", "recycler", "recycled", ".trash-"})
_UID = re.compile(r"\.Trash-\d{1,10}")

# either separator: a Windows snapshot read elsewhere joins its paths with "/"
_WIN_BIN = re.compile(
    r"(?:[a-z]:|[\\/]{2}[^\\/]+[\\/][^\\/]+)[\\/]+(?:\$recycle\.bin|recycler|recycled)", re.IGNORECASE
)
_MAC_HOME = re.compile(r"/Users/[^/]+/\.Trash")
_MAC_VOLUME = re.compile(r"(?:/Volumes/[^/]+)?/\.Trashes")
_XDG_HOME = re.compile(r".*/\.local/share/Trash")
_UID_TRASH = re.compile(r".*/\.Trash-\d{1,10}")


def _xdg_trash() -> str | None:
    """This user's home trash when ``XDG_DATA_HOME`` moves it (Linux)."""
    data = os.environ.get("XDG_DATA_HOME", "").strip()
    if not data or not sys.platform.startswith("linux"):
        return None
    return os.path.normpath(os.path.join(data, "Trash"))


def is_trash_path(path: str, *, top: bool = False) -> bool:
    """Whether ``path`` is a recycle bin / trash folder. ``top``: its parent is a scan root (the
    shared ``.Trash`` of a mount and a volume's ``.Trashes`` count there too)."""
    p = path.rstrip("\\/") if len(path) > 3 else path
    if _WIN_BIN.fullmatch(p):
        return True
    q = p.replace("\\", "/")
    if any(r.fullmatch(q) for r in (_MAC_HOME, _MAC_VOLUME, _XDG_HOME, _UID_TRASH)):
        return True
    name = q.rsplit("/", 1)[-1]
    if top and name in (".Trash", ".Trashes"):
        return True
    xdg = _xdg_trash()
    return xdg is not None and os.path.normpath(p) == xdg


def _candidate(name: str) -> bool:
    low = name.casefold()
    return low in _NAMES or _UID.fullmatch(name) is not None


class _Known:
    """What the nodes of one store version were found to be: a bin, or not."""

    __slots__ = ("bins", "checked", "version")

    def __init__(self, version: int) -> None:
        self.version = version
        self.checked = np.zeros(0, dtype=np.int64)  # sorted node ids already looked at
        self.bins: set[int] = set()


_cache: WeakKeyDictionary[NodeStore, _Known] = WeakKeyDictionary()
_lock = threading.Lock()


def _bins_among(store: NodeStore, nodes: np.ndarray) -> set[int]:
    """The bins among ``nodes`` (folder ids, unique), remembered per store version."""
    with _lock:
        known = _cache.get(store)
        if known is None or known.version != store.version:
            known = _cache[store] = _Known(store.version)
    fresh = np.setdiff1d(nodes, known.checked, assume_unique=True)
    if len(fresh):
        c = store.cols
        lengths = c["name_len"][fresh].astype(np.int64)
        parent = c["parent"]
        for node in fresh[np.isin(lengths, _LENGTHS)].tolist():
            name = store.name(node)
            if not _candidate(name):
                continue
            up = int(parent[node])
            if is_trash_path(store.path(node), top=up > 0 and int(parent[up]) == 0):
                known.bins.add(node)
        # a scan root that is itself a bin (the trash folder was scanned on its own)
        for node in fresh[c["parent"][fresh] == 0].tolist():
            if is_trash_path(store.name(node)):
                known.bins.add(node)
        with _lock:
            known.checked = np.union1d(known.checked, fresh)
    return known.bins


def trash_marks(store: NodeStore, nodes: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Per node of ``nodes`` (ids; negative: none): whether it is in a bin (the bin itself included),
    and whether it is a bin."""
    inside = np.zeros(len(nodes), dtype=bool)
    is_bin = np.zeros(len(nodes), dtype=bool)
    n = len(store)
    valid = (nodes > 0) & (nodes < n)
    if not valid.any():
        return inside, is_bin
    c = store.cols
    parent, kind = c["parent"][:n], c["kind"][:n]
    # the rows' nodes and every ancestor, one level per step (all chains end at node 0)
    chains = [nodes[valid].astype(np.int64)]
    cur = chains[0]
    while True:
        cur = parent[cur].astype(np.int64)
        cur = cur[cur > 0]
        if not len(cur):
            break
        cur = np.unique(cur)
        chains.append(cur)
    every = np.unique(np.concatenate(chains))
    bins = _bins_among(store, every[kind[every] == Kind.DIR])
    if not bins:
        return inside, is_bin
    marks = np.array(sorted(bins), dtype=np.int64)
    own = nodes[valid].astype(np.int64)
    is_bin[valid] = np.isin(own, marks)
    hit = is_bin[valid].copy()
    cur = own
    while True:
        cur = np.where(cur > 0, parent[np.maximum(cur, 0)], 0).astype(np.int64)
        if not (cur > 0).any():
            break
        hit |= np.isin(cur, marks)
    inside[valid] = hit
    return inside, is_bin


def in_trash(store: NodeStore, nodes: np.ndarray) -> np.ndarray:
    """Per node of ``nodes``: it is a bin or inside one (:func:`trash_marks`)."""
    return trash_marks(store, nodes)[0]


# ------------------------------------------------------------------ every bin of a store (「回收站」 chip)

BIN_DEPTH = 7  # the deepest a bin sits below its scan root (depth 1): /home/<user>/.local/share/Trash
FULL_SEARCH_S = 5.0  # a store that keeps changing is searched whole at most this often
_FIRST = np.array([ord(ch) for ch in "$.TtRr"], dtype=np.uint8)  # the bins' names start with these


class _Found:
    __slots__ = ("at", "ids", "paths", "version")

    def __init__(self, version: int, at: float, ids: np.ndarray, paths: list[str]) -> None:
        self.version, self.at, self.ids, self.paths = version, at, ids, paths


_found: WeakKeyDictionary[NodeStore, _Found] = WeakKeyDictionary()


def _first_bytes(store: NodeStore, n: int) -> np.ndarray:
    """The first byte of every node's name (0 for an empty one), from the name heap's chunks."""
    out = np.zeros(n, dtype=np.uint8)
    lengths = store.cols["name_len"][:n]
    a = 0
    while a < n:
        stop, run, sep = store.names_run(a, n)
        if stop <= a:
            break
        first = run[np.minimum(sep.astype(np.int64) + 1, len(run) - 1)]
        out[a:stop] = np.where(lengths[a:stop] > 0, first, 0)
        a = stop
    return out


def _search(store: NodeStore) -> tuple[np.ndarray, list[str]]:
    """Every bin in the store: folders near the top of a root whose name and path say so."""
    n = len(store)
    if n <= 1:
        return np.zeros(0, dtype=np.int64), []
    c = store.cols
    kind, depth, parent = c["kind"][:n], c["depth"][:n], c["parent"][:n]
    near = (kind == Kind.DIR) & (depth <= BIN_DEPTH) & (depth >= 1)
    near &= np.isin(c["name_len"][:n].astype(np.int64), _LENGTHS)
    near[0] = False
    ids = np.flatnonzero(near)
    if len(ids):
        ids = ids[np.isin(_first_bytes(store, n)[ids], _FIRST)]
    bins: list[int] = []
    for node in ids.tolist():
        if not _candidate(store.name(node)):
            continue
        up = int(parent[node])
        if is_trash_path(store.path(node), top=up > 0 and int(parent[up]) == 0):
            bins.append(node)
    for root in store.roots():  # a scan of the bin itself
        if is_trash_path(store.name(root)):
            bins.append(int(root))
    found = np.array(sorted(set(bins)), dtype=np.int64)
    return found, [store.path(int(b)) for b in found]


def find_bins(store: NodeStore) -> np.ndarray:
    """The ids of every recycle bin / trash folder in the store, sorted. A store that changes all the
    time (monitoring) is searched whole at most every FULL_SEARCH_S seconds; in between, the bins
    found last are looked up again by path."""
    now = time.monotonic()
    with _lock:
        hit = _found.get(store)
    if hit is not None and hit.version == store.version:
        return hit.ids
    if hit is not None and now - hit.at < FULL_SEARCH_S:
        again = [store.find(p) for p in hit.paths]
        ids = np.array(sorted({i for i in again if i is not None}), dtype=np.int64)
        fresh = _Found(store.version, hit.at, ids, [store.path(int(i)) for i in ids])
    else:
        ids, paths = _search(store)
        fresh = _Found(store.version, now, ids, paths)
    with _lock:
        _found[store] = fresh
    return fresh.ids


def subtree(store: NodeStore, tops: np.ndarray) -> np.ndarray:
    """``tops`` and everything below them, sorted (one level per step, ranges joined with numpy)."""
    c = store.cols
    n = len(store)
    parts = [tops.astype(np.int64)]
    frontier = parts[0]
    while len(frontier):
        frontier = frontier[c["kind"][frontier] == Kind.DIR]
        start = c["child_start"][frontier].astype(np.int64)
        count = c["child_count"][frontier].astype(np.int64)
        ok = (count > 0) & (start >= 1) & (start + count <= n)
        start, count = start[ok], count[ok]
        if not len(count):
            break
        # each range start … start + count - 1, all at once
        steps = np.ones(int(count.sum()), dtype=np.int64)
        heads = np.concatenate(([0], np.cumsum(count)[:-1]))
        steps[heads] = start - np.concatenate(([0], start[:-1] + count[:-1] - 1))
        frontier = np.cumsum(steps)
        parts.append(frontier)
    return np.unique(np.concatenate(parts))


class WithoutBins:
    """A size source with the recycle bins taken out (the 「回收站」 chip off): a bin and everything
    in it weigh nothing, and each of its ancestors loses the bin's bytes."""

    __slots__ = ("_cut_ids", "_cut_val", "_inside", "base", "n")

    def __init__(self, base: SizeSource, store: NodeStore, bins: np.ndarray) -> None:
        self.base = base
        self.n = base.n
        bins = bins[bins < base.n]
        self._inside = subtree(store, bins)
        self._inside = self._inside[self._inside < base.n]
        less: dict[int, int] = {}
        for b, size in zip(bins.tolist(), base.of(bins).tolist(), strict=True):
            for a in store.ancestors(int(b)):
                less[a] = less.get(a, 0) + int(size)
        self._cut_ids = np.array(sorted(less), dtype=np.int64)
        self._cut_val = np.array([less[i] for i in self._cut_ids.tolist()], dtype=np.int64)

    def of(self, ids: np.ndarray) -> np.ndarray:
        out = np.array(self.base.of(ids), dtype=np.int64, copy=True)
        if len(self._inside):
            out[np.isin(ids, self._inside)] = 0
        if len(self._cut_ids):
            at = np.searchsorted(self._cut_ids, ids)
            hit = (at < len(self._cut_ids)) & (self._cut_ids[np.minimum(at, len(self._cut_ids) - 1)] == ids)
            out[hit] -= self._cut_val[at[hit]]
        return np.maximum(out, 0)

    def full(self) -> np.ndarray:
        out = np.array(self.base.full(), dtype=np.int64, copy=True)
        out[self._inside] = 0
        out[self._cut_ids] -= self._cut_val
        return np.maximum(out, 0)


__all__ = ["WithoutBins", "find_bins", "in_trash", "is_trash_path", "subtree", "trash_marks"]

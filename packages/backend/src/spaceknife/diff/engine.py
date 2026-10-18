"""Snapshot compare (docs/manage.md §3): two trees merged by path into one union tree.

Both trees are walked from their roots together, level by level. The children of a folder that
exists on both sides are matched by name (the Windows rule folds case: ``ntpath.normcase`` of the
decoded name, an exact spelling wins over a folded one); a file and a folder of the same name are
two different items (removed + added), a renamed item is removed + added. Everything below a
folder that exists on one side only is taken over whole. ``GONE`` rows (store.mutate) lie outside
every child range and never take part.

One level is matched without a Python object per node:

1. Aligned passes. The children of a folder pair are compared position by position from the front
   and, when the counts differ, from the back. Two scans of the same disk list almost every folder
   in the same order, so this matches nearly everything with one word-wise comparison of the names
   (8 bytes at a time, read through an unaligned ``uint64`` view of the name image).
2. Leftovers by key. A 64-bit hash of each name, mixed with its folder and its folder-or-not bit,
   gets the row number packed into its low bits; one ``np.sort`` (no argsort) puts equal keys next
   to each other. Pairs are verified byte for byte; the rare groups of three or more (duplicate
   names, collisions of the shortened key) are paired in Python.
3. Windows rule only: the leftovers once more with case folded (ASCII in bulk with a SWAR fold,
   names with other characters through ``ntpath.normcase``).

The union store is laid out breadth first: every level's rows are grouped by parent in parent
order (a matched folder lists the target's children in the target's order, then the removed ones),
so child ranges are contiguous, every id is greater than its parent's and the totals are those
:meth:`NodeStore.reaggregate` computes. Its size columns hold the target's values, the base's for
removed rows. Per row :class:`DiffTree` adds both ends (folders: subtree totals), the change, a
status, and the growth and shrinkage summed over the subtree.

``compare`` only reads the two stores; the caller holds ``STRUCTURE.read()`` while it runs (the
compute pool does).
"""

from __future__ import annotations

import heapq
import ntpath
import os
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass
from enum import IntEnum
from typing import Literal

import numpy as np
from loguru import logger

from ..store.nodes import COLUMNS, NAME_ERRORS, OFF_NARROW_LIMIT, Flag, Kind, NodeStore, decode_name
from ..store.subtree import ranges_to_ids

ProgressFn = Callable[[float], None]  # fraction done, 0..1
CancelFn = Callable[[], bool]  # True: stop (e.g. ``threading.Event().is_set``)
Direction = Literal["grow", "shrink"]

DOMINANT = 0.5  # changes(): a folder is broken down when significant children hold this share ...
MINOR = 0.05  # ... a child is significant from this share of its folder's change

# User-facing text, kept in one place for the i18n extraction (M7.5).
TEXT = {
    "cancelled": "对比已取消",
    "direction": "方向只能是 grow 或 shrink",
    "status.unchanged": "不变",
    "status.added": "新增",
    "status.removed": "已删除",
    "status.changed": "变化",
}


class Status(IntEnum):
    UNCHANGED = 0
    ADDED = 1
    REMOVED = 2
    CHANGED = 3


STATUS_LABELS = {s: TEXT[f"status.{s.name.lower()}"] for s in Status}


class CompareCancelled(Exception):
    """``cancel()`` returned True while :func:`compare` ran."""

    def __init__(self) -> None:
        super().__init__(TEXT["cancelled"])


@dataclass(frozen=True, slots=True)
class Change:
    """One change unit of :meth:`DiffTree.changes`."""

    node: int  # row of DiffTree.store
    path: str
    kind: Kind
    status: Status  # a rest unit: CHANGED
    amount: int  # growth (or shrinkage) inside the unit: what the ranking uses, > 0
    delta: int  # net change, target_alloc - base_alloc
    base_alloc: int
    target_alloc: int
    base_size: int
    target_size: int
    rest: bool = False  # the folder ``node`` without its significant children (listed on their own)
    members: int = 0  # rest unit: its changed entries (children of ``node``)


@dataclass(eq=False, slots=True)
class DiffTree:
    """The union of two trees; one entry per row of :attr:`store` in every array."""

    store: NodeStore  # union tree; size columns hold the target's values (removed rows: the base's)
    base_id: np.ndarray  # i4 row of the base store, -1 where the item is not in the base
    target_id: np.ndarray  # i4 row of the target store, -1 where the item is not in the target
    base_alloc: np.ndarray  # i8 allocation in the base (folders: subtree total; hard-link dups 0)
    target_alloc: np.ndarray
    base_size: np.ndarray  # i8 logical size, same rules
    target_size: np.ndarray
    delta: np.ndarray  # i8 target_alloc - base_alloc
    status: np.ndarray  # u1 Status
    grow: np.ndarray  # i8 growth summed over the subtree (a file: its own), ≥ 0
    shrink: np.ndarray  # i8 shrinkage summed over the subtree, ≥ 0
    fold_case: bool  # names were matched with the Windows rule

    def __len__(self) -> int:
        return len(self.store)

    def area(self) -> np.ndarray:
        """Treemap area basis in compare mode: ``|change|`` per file, a folder the sum below it
        (grow + shrink), so nested blocks add up like the normal sizes do."""
        result: np.ndarray = self.grow + self.shrink
        return result

    def memory_bytes(self) -> int:
        arrays = (
            self.base_id,
            self.target_id,
            self.base_alloc,
            self.target_alloc,
            self.base_size,
            self.target_size,
            self.delta,
            self.status,
            self.grow,
            self.shrink,
        )
        return self.store.memory_bytes() + sum(a.nbytes for a in arrays)

    def changes(
        self,
        direction: Direction,
        limit: int,
        *,
        scope: int = 0,
        amount: np.ndarray | None = None,
        dominant: float = DOMINANT,
        minor: float = MINOR,
    ) -> list[Change]:
        """The ``limit`` largest change units in ``direction`` (growth or shrinkage), largest first.

        The units partition the change exactly: none lies inside another and together they add up
        to the growth (shrinkage) of the whole scope, so nothing is counted twice. Rule, from the
        scope down: a child holding at least ``minor`` of its folder's change is *significant*. A
        folder whose significant children together hold at least ``dominant`` of its change (its
        change mostly comes from one or a few children) is broken down: each significant child
        competes on its own (and is broken down by the same rule), the rest of the folder becomes
        one ``rest`` unit. Any other folder — its change spread thinly over many entries — is one
        unit, and so is every file. A folder that grew by one big new file shows that file; a
        folder that grew by thousands of small ones shows the folder. Ties: lower row first.

        ``scope``: rank inside this row's subtree only (default: everything). A folder scope is
        always broken down: its children are the first candidates. ``amount``: per-row amounts
        to rank by instead of :attr:`grow` / :attr:`shrink` (counted over a filter's matches, say;
        a folder's amount must be the sum below it). Other values of a unit come from the arrays.
        """
        if direction not in ("grow", "shrink"):
            raise ValueError(TEXT["direction"])
        if amount is None:
            amount = self.grow if direction == "grow" else self.shrink
        store = self.store
        kind = store.cols["kind"]
        out: list[Change] = []
        lists: list[np.ndarray] = []  # significant children of broken-down folders, largest first
        heap: list[tuple[int, int, int, int, int]] = []  # (-amount, node, is rest, list, rank)
        rests: dict[int, tuple[int, np.ndarray]] = {}  # folder -> (entries in its rest, its significant)

        def significant(node: int, everything: bool = False) -> tuple[np.ndarray, int]:
            lo, hi = store.child_range(node)
            a = amount[lo:hi]
            keep = a > 0 if everything else (a > 0) & (a >= minor * int(amount[node]))
            ids = np.flatnonzero(keep)
            ids = ids[np.argsort(-a[ids], kind="stable")] + lo
            return ids, int(np.count_nonzero(a > 0)) - len(ids)

        def expand(node: int, ids: np.ndarray, others: int) -> None:
            if ids.size:
                lists.append(ids)
                first = int(ids[0])
                heapq.heappush(heap, (-int(amount[first]), first, 0, len(lists) - 1, 0))
            rest = int(amount[node]) - int(amount[ids].sum())
            if rest > 0 and node != scope:
                rests[node] = (others, ids)
                heapq.heappush(heap, (-rest, node, 1, -1, -1))

        if limit <= 0:
            return out
        if kind[scope] == Kind.DIR:
            expand(scope, *significant(scope, everything=True))  # the scope itself is never a unit
        elif int(amount[scope]) > 0:
            return [self._change(scope, int(amount[scope]))]
        while heap and len(out) < limit:
            neg, node, is_rest, li, rank = heapq.heappop(heap)
            if is_rest:
                others, sig = rests.pop(node)
                out.append(self._change(node, -neg, rest=sig, members=others))
                continue
            sibs = lists[li]
            if rank + 1 < len(sibs):
                nxt = int(sibs[rank + 1])
                heapq.heappush(heap, (-int(amount[nxt]), nxt, 0, li, rank + 1))
            if kind[node] == Kind.DIR:
                ids, others = significant(node)
                if ids.size and int(amount[ids].sum()) >= dominant * int(amount[node]):
                    expand(node, ids, others)
                    continue
            out.append(self._change(node, -neg))
        return out

    def _change(self, node: int, amount: int, rest: np.ndarray | None = None, members: int = 0) -> Change:
        """A unit: a whole row, or (``rest``: the significant children taken out) its rest."""

        def value(arr: np.ndarray) -> int:
            return int(arr[node]) - (int(arr[rest].sum()) if rest is not None else 0)

        return Change(
            node=node,
            path=self.store.path(node),
            kind=Kind(int(self.store.cols["kind"][node])),
            status=Status.CHANGED if rest is not None else Status(int(self.status[node])),
            amount=amount,
            delta=value(self.delta),
            base_alloc=value(self.base_alloc),
            target_alloc=value(self.target_alloc),
            base_size=value(self.base_size),
            target_size=value(self.target_size),
            rest=rest is not None,
            members=members,
        )


def compare(
    base: NodeStore,
    target: NodeStore,
    *,
    progress: ProgressFn | None = None,
    cancel: CancelFn | None = None,
    fold_case: bool | None = None,
) -> DiffTree:
    """Merge ``base`` and ``target`` by path into a :class:`DiffTree`.

    ``fold_case``: match names case-insensitively (the Windows rule); default: on Windows.
    ``progress`` gets the fraction done now and then; ``cancel`` is polled between steps and
    makes this raise :class:`CompareCancelled`.
    """
    t0 = time.perf_counter()
    fold = sys.platform == "win32" if fold_case is None else bool(fold_case)
    tree = _Merge(_Side(base), _Side(target), fold, progress, cancel).run()
    logger.debug(
        "compare: {} + {} rows -> {} in {:.2f} s",
        len(base),
        len(target),
        len(tree.store),
        time.perf_counter() - t0,
    )
    return tree


# --------------------------------------------------------------------------- names as words

_DIR = np.uint8(Kind.DIR)
_GONE = np.uint32(Flag.GONE)
_DUP = np.uint32(Flag.HARDLINK_DUP)
_ONES = np.uint64(0xFFFF_FFFF_FFFF_FFFF)
_LO7 = np.uint64(0x7F7F_7F7F_7F7F_7F7F)
_HIGH = np.uint64(0x8080_8080_8080_8080)
_PLUS_A = np.uint64(0x3F3F_3F3F_3F3F_3F3F)  # byte + 0x3F sets bit 7 from "A" (0x41) up
_PLUS_Z = np.uint64(0x2525_2525_2525_2525)  # byte + 0x25 sets bit 7 from "[" (0x5B) up
_SEED = np.uint64(0x243F_6A88_85A3_08D3)
_K_LEN = np.uint64(0x9E37_79B9_7F4A_7C15)
_K_ROUND = np.uint64(0xBF58_476D_1CE4_E5B9)
_K_OWNER = np.uint64(0x94D0_49BB_1331_11EB)
_K_DIR = np.uint64(0xD6E8_FEB8_6659_FD93)
_FMIX1 = np.uint64(0xFF51_AFD7_ED55_8CCD)
_FMIX2 = np.uint64(0xC4CE_B9FE_1A85_EC53)
_U8 = np.uint64(8)
_U2 = np.uint64(2)


class _Image:
    """A ``\\0name`` byte image with a ``uint64`` view starting at every byte (unaligned)."""

    def __init__(self, image: np.ndarray) -> None:
        if len(image) < 8:
            padded = np.zeros(8, dtype=np.uint8)
            padded[: len(image)] = image
            image = padded
        self.image = image
        self.last = len(image) - 8  # the last position a whole word can be read from
        self.words = np.ndarray(shape=(len(image) - 7,), dtype="<u8", buffer=image, strides=(1,))

    def read(self, pos: np.ndarray) -> np.ndarray:
        """The 8 bytes at each position as a little-endian word; bytes past the end read as 0."""
        if pos.size and int(pos.max()) > self.last:
            over = np.maximum(pos - self.last, 0).astype(np.uint64)
            tail: np.ndarray = self.words[np.minimum(pos, self.last)] >> (over * _U8)
            return tail
        words: np.ndarray = self.words[pos]
        return words

    @classmethod
    def of(cls, names: list[bytes]) -> tuple[_Image, np.ndarray, np.ndarray]:
        """An image of some names: the image, each name's position and length."""
        lens = np.fromiter((len(n) for n in names), dtype=np.int64, count=len(names))
        pos = np.cumsum(lens + 1) - lens
        raw = b"".join(b"\0" + n for n in names)
        return cls(np.frombuffer(raw, dtype=np.uint8) if raw else np.zeros(1, dtype=np.uint8)), pos, lens


def _mask(rem: np.ndarray) -> np.ndarray:
    """The low ``min(rem, 8)`` bytes of a word (``rem`` ≥ 1)."""
    nbytes = np.minimum(rem, 8).astype(np.uint64)
    mask: np.ndarray = _ONES >> ((_U8 - nbytes) * _U8)
    return mask


def _fold(w: np.ndarray) -> np.ndarray:
    """ASCII ``A``–``Z`` to lower case in every byte of the words (other bytes unchanged)."""
    x = w & _LO7
    upper = (x + _PLUS_A) & ~(x + _PLUS_Z) & ~w & _HIGH
    folded: np.ndarray = w | (upper >> _U2)
    return folded


def _fmix(h: np.ndarray) -> np.ndarray:
    h = h ^ (h >> np.uint64(33))
    h *= _FMIX1
    h ^= h >> np.uint64(33)
    h *= _FMIX2
    h ^= h >> np.uint64(33)
    return h


def _hash(img: _Image, pos: np.ndarray, lens: np.ndarray, *, fold: bool) -> tuple[np.ndarray, np.ndarray]:
    """64-bit hash of each name (ASCII-folded with ``fold``) and whether it has a non-ASCII byte."""
    m = len(pos)
    h = lens.astype(np.uint64) * _K_LEN ^ _SEED
    wide = np.zeros(m, dtype=bool)
    idx = np.flatnonzero(lens > 0)
    p, rem, x = pos[idx].astype(np.int64, copy=False), lens[idx].astype(np.int64, copy=False), h[idx]
    while idx.size:
        w = img.read(p) & _mask(rem)
        if fold:
            wide[idx[(w & _HIGH) != 0]] = True
            w = _fold(w)
        x = (x ^ w) * _K_ROUND
        x ^= x >> np.uint64(29)
        more = rem > 8
        if not bool(more.all()):
            done = ~more
            h[idx[done]] = x[done]
            idx, p, rem, x = idx[more], p[more], rem[more], x[more]
        p += 8
        rem -= 8
    return _fmix(h), wide


def _equal(
    ia: _Image, pa: np.ndarray, ib: _Image, pb: np.ndarray, lens: np.ndarray, *, fold: bool
) -> np.ndarray:
    """Per pair: the names of length ``lens`` at ``pa`` in ``ia`` and ``pb`` in ``ib`` are equal."""
    ok = np.ones(len(lens), dtype=bool)
    idx = np.flatnonzero(lens > 0)
    pa, pb = pa[idx].astype(np.int64, copy=False), pb[idx].astype(np.int64, copy=False)
    rem = lens[idx].astype(np.int64, copy=False)
    while idx.size:
        mask = _mask(rem)
        wa, wb = ia.read(pa) & mask, ib.read(pb) & mask
        if fold:
            wa, wb = _fold(wa), _fold(wb)
        same = wa == wb
        ok[idx[~same]] = False
        more = same & (rem > 8)
        idx, pa, pb, rem = idx[more], pa[more] + 8, pb[more] + 8, rem[more] - 8
    return ok


def _key(h: np.ndarray, owner: np.ndarray, is_dir: np.ndarray) -> np.ndarray:
    """A name hash bound to its folder (index within the level) and its folder-or-not bit."""
    return _fmix(h + owner.astype(np.uint64) * _K_OWNER + is_dir.astype(np.uint64) * _K_DIR)


def _pair_sorted(kb: np.ndarray, kt: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    """Equal keys of the two sides: ``(pairs_b, pairs_t, crowd_b, crowd_t)``.

    Pairs are groups of exactly one key of each side (to verify); a crowd is every member of a
    group of three or more (to pair in Python). The key loses its low bits to the row number, so
    a group may hold keys that only look alike: callers compare full keys and names.
    """
    nb, m = len(kb), len(kb) + len(kt)
    bits = max(1, (m - 1).bit_length())
    low = np.uint64((1 << bits) - 1)
    packed = (np.concatenate([kb, kt]) & ~low) | np.arange(m, dtype=np.uint64)
    packed.sort()
    idx = (packed & low).astype(np.int64)
    group = packed >> np.uint64(bits)
    fresh = np.ones(m, dtype=bool)
    fresh[1:] = group[1:] != group[:-1]
    starts = np.flatnonzero(fresh)
    sizes = np.diff(np.append(starts, m))
    two = starts[sizes == 2]
    a, b = idx[two], idx[two + 1]  # a < b: equal keys sort by row number
    cross = (a < nb) & (b >= nb)
    big = sizes >= 3
    crowd = idx[ranges_to_ids(starts[big], sizes[big])] if bool(big.any()) else np.empty(0, np.int64)
    return a[cross], b[cross] - nb, crowd[crowd < nb], crowd[crowd >= nb] - nb


# --------------------------------------------------------------------------- the merge


class _Side:
    """One input tree: its live columns and its names as one image."""

    def __init__(self, store: NodeStore) -> None:
        n = len(store)
        c = store.cols
        self.store = store
        self.n = n
        self.kind = c["kind"][:n]
        self.flags = c["flags"][:n]
        self.name_off = c["name_off"][:n]
        self.name_len = c["name_len"][:n]
        self.child_start = c["child_start"][:n]
        self.child_count = c["child_count"][:n]
        parts = store.names_parts()
        self.multi = len(parts) > 1  # a live scan's heap: several chunks, joined here once
        image = np.concatenate(parts) if self.multi else (parts[0] if parts else np.zeros(1, np.uint8))
        self.img = _Image(image)
        self.visited = 0

    def pos(self, ids: np.ndarray) -> np.ndarray:
        """Where the names of rows ``ids`` start in :attr:`img`."""
        off: np.ndarray = self.name_off[ids].astype(np.int64)
        if self.multi:
            moved: np.ndarray = self.store._heap.image_positions(off)
            return moved
        return off

    def name(self, row: int) -> bytes:
        p = int(self.pos(np.array([row]))[0])
        return self.img.image[p : p + int(self.name_len[row])].tobytes()

    def children(self, folders: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
        """Live children of ``folders`` (-1: none on this side): ``(rows, owner, first, count)``.

        ``owner`` is the index into ``folders`` of each child's folder; ``first`` / ``count`` give
        every folder's slice of ``rows``. Ranges are read as :meth:`NodeStore.child_range` does.
        """
        k = len(folders)
        have = folders >= 0
        fid = np.where(have, folders, 0)
        count = self.child_count[fid].astype(np.int64)
        start = self.child_start[fid].astype(np.int64)
        ok = have & (self.kind[fid] == _DIR) & (count > 0) & (start >= 1) & (start + count <= self.n)
        count = np.where(ok, count, 0)
        rows = ranges_to_ids(start[ok], count[ok])
        owner = np.repeat(np.arange(k, dtype=np.int64), count)
        live = (self.flags[rows] & _GONE) == 0
        if not bool(live.all()):
            rows, owner = rows[live], owner[live]
            count = np.bincount(owner, minlength=k).astype(np.int64, copy=False)
        self.visited += len(rows)
        if self.visited > self.n:  # only overlapping child ranges can do this: never run away
            raise RuntimeError("compare: the child ranges of a tree overlap")
        return rows, owner, np.cumsum(count) - count, count

    def ends(self, ids: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """Per union row (``ids``: this side's row, -1): allocation and size at this end — subtree
        totals for folders, 0 for hard-link duplicates — and the row's own allocation as it counts
        toward its folder (a folder's own blocks too)."""
        n = len(ids)
        have = np.flatnonzero(ids >= 0)
        rows = ids[have]
        c = self.store.cols
        is_dir = self.kind[rows] == _DIR
        counted = (self.flags[rows] & _DUP) == 0
        out: list[np.ndarray] = []
        own_alloc = np.zeros(n, dtype=np.int64)
        for key in ("alloc", "size"):
            own = np.where(counted, c[key][rows], 0)
            if key == "alloc":
                own_alloc[have] = own
            full = np.zeros(n, dtype=np.int64)
            full[have] = np.where(is_dir, c["tot_" + key][rows], own)
            out.append(full)
        return out[0], out[1], own_alloc


def _normcase(side: _Side, row: int, known: str | None) -> str:
    """A row's name folded by the Windows rule: ``ntpath.normcase`` of the decoded name."""
    return known if known is not None else ntpath.normcase(decode_name(side.name(row)))


@dataclass(slots=True)
class _Level:
    """Candidates of one level: the children of the previous level's folders on one side."""

    rows: np.ndarray  # rows of the side's store
    owner: np.ndarray  # index of the folder (within the level) each belongs to
    first: np.ndarray  # per folder: its first candidate
    count: np.ndarray  # per folder: how many
    match: np.ndarray  # index of the matched candidate of the other side, -1


class _Merge:
    def __init__(
        self, base: _Side, target: _Side, fold: bool, progress: ProgressFn | None, cancel: CancelFn | None
    ) -> None:
        self.B, self.T = base, target
        self.fold = fold
        self.progress = progress
        self.cancel = cancel
        self.done = 0
        self.total = max(1, base.n + target.n)

    def _tick(self, fraction: float) -> None:
        if self.cancel is not None and self.cancel():
            raise CompareCancelled
        if self.progress is not None:
            self.progress(min(1.0, max(0.0, fraction)))

    # ------------------------------------------------------------------ walking

    def run(self) -> DiffTree:
        B, T = self.B, self.T
        rows_b = [np.zeros(1, dtype=np.int64)]  # per level: base row of each union row (-1)
        rows_t = [np.zeros(1, dtype=np.int64)]
        parents = [np.full(1, -1, dtype=np.int64)]
        start = 0  # union id of the previous level's first row
        while True:
            self._tick(0.75 * self.done / self.total)
            pb, pt = rows_b[-1], rows_t[-1]
            is_dir = np.where(pt >= 0, T.kind[np.maximum(pt, 0)] == _DIR, B.kind[np.maximum(pb, 0)] == _DIR)
            folders = np.flatnonzero(is_dir)
            if not folders.size:
                break
            nb, nt, par = self._level(pb[folders], pt[folders], start + folders, roots=len(rows_b) == 1)
            if not len(par):
                break
            start += len(pb)
            rows_b.append(nb)
            rows_t.append(nt)
            parents.append(par)
        sizes = np.array([len(x) for x in rows_b], dtype=np.int64)
        base_id = np.concatenate(rows_b)
        target_id = np.concatenate(rows_t)
        del rows_b, rows_t
        parent = np.concatenate(parents).astype(COLUMNS["parent"])
        del parents
        self._tick(0.75)
        return self._build(base_id, target_id, parent, sizes)

    def _level(
        self, fb: np.ndarray, ft: np.ndarray, fu: np.ndarray, *, roots: bool
    ) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """The next level below folders ``fu`` (union ids; ``fb`` / ``ft`` their rows, -1):
        base row, target row and parent of every new union row, grouped by parent in order."""
        B, T = self.B, self.T
        cb = B.children(fb)
        ct = T.children(ft)
        lb = _Level(*cb, np.full(len(cb[0]), -1, dtype=np.int64))
        lt = _Level(*ct, np.full(len(ct[0]), -1, dtype=np.int64))
        self.done += len(lb.rows) + len(lt.rows)
        both = (fb >= 0) & (ft >= 0)
        if roots:
            self._match_roots(lb, lt)
        elif len(lb.rows) and len(lt.rows):
            self._match(lb, lt, both)
        # union rows: per folder the target's children in order, then the base's unmatched ones
        k = len(fu)
        loose = np.flatnonzero(lb.match < 0)
        loose_count = np.bincount(lb.owner[loose], minlength=k).astype(np.int64, copy=False)
        count = lt.count + loose_count
        offset = np.cumsum(count) - count
        size = int(count.sum())
        new_b = np.full(size, -1, dtype=np.int64)
        new_t = np.full(size, -1, dtype=np.int64)
        at_t = offset[lt.owner] + (np.arange(len(lt.rows)) - lt.first[lt.owner])
        new_t[at_t] = lt.rows
        hit = lt.match >= 0
        new_b[at_t[hit]] = lb.rows[lt.match[hit]]
        owner = lb.owner[loose]
        rank = np.arange(len(loose)) - (np.cumsum(loose_count) - loose_count)[owner]
        new_b[offset[owner] + lt.count[owner] + rank] = lb.rows[loose]
        return new_b, new_t, np.repeat(fu, count)

    # ------------------------------------------------------------------ matching

    def _match_roots(self, lb: _Level, lt: _Level) -> None:
        """Scan roots: equal normalised paths (folded with the Windows rule), first come first."""
        B, T = self.B, self.T

        def key(side: _Side, row: int) -> tuple[str, bool]:
            path = os.path.normpath(decode_name(side.name(row)))
            return (ntpath.normcase(path) if self.fold else path), bool(side.kind[row] == _DIR)

        waiting: dict[tuple[str, bool], list[int]] = {}
        for i, row in enumerate(lb.rows.tolist()):
            waiting.setdefault(key(B, row), []).append(i)
        for j, row in enumerate(lt.rows.tolist()):
            free = waiting.get(key(T, row))
            if free:
                i = free.pop(0)
                lb.match[i], lt.match[j] = j, i

    def _match(self, lb: _Level, lt: _Level, both: np.ndarray) -> None:
        # 1. aligned from the front
        k = np.flatnonzero(both & (lb.count > 0) & (lt.count > 0))
        n = np.minimum(lb.count[k], lt.count[k])
        self._try(lb, lt, ranges_to_ids(lb.first[k], n), ranges_to_ids(lt.first[k], n))
        # 2. aligned from the back where the counts differ (an entry added or removed)
        k = k[lb.count[k] != lt.count[k]]
        if k.size:
            n = np.minimum(lb.count[k], lt.count[k])
            step = ranges_to_ids(np.zeros(len(k), dtype=np.int64), n)
            folder = np.repeat(k, n)
            ib = (lb.first + lb.count - 1)[folder] - step
            it = (lt.first + lt.count - 1)[folder] - step
            free = (lb.match[ib] < 0) & (lt.match[it] < 0)
            self._try(lb, lt, ib[free], it[free])
        # 3. the rest by key, 4. by folded key (Windows)
        self._by_key(lb, lt, both, fold=False)
        if self.fold:
            self._by_key(lb, lt, both, fold=True)

    def _try(self, lb: _Level, lt: _Level, ib: np.ndarray, it: np.ndarray) -> None:
        """Match candidate pairs whose names are byte for byte equal (and both folders or not)."""
        B, T = self.B, self.T
        xb, xt = lb.rows[ib], lt.rows[it]
        lens = B.name_len[xb]
        alike = (lens == T.name_len[xt]) & ((B.kind[xb] == _DIR) == (T.kind[xt] == _DIR))
        sel = np.flatnonzero(alike)
        if not sel.size:
            return
        eq = _equal(B.img, B.pos(xb[sel]), T.img, T.pos(xt[sel]), lens[sel], fold=False)
        sel = sel[eq]
        lb.match[ib[sel]] = it[sel]
        lt.match[it[sel]] = ib[sel]

    def _by_key(self, lb: _Level, lt: _Level, both: np.ndarray, *, fold: bool) -> None:
        B, T = self.B, self.T
        k = len(both)
        ib = np.flatnonzero((lb.match < 0) & both[lb.owner])
        it = np.flatnonzero((lt.match < 0) & both[lt.owner])
        if not ib.size or not it.size:
            return
        # only folders with leftovers on both sides
        has_b = np.bincount(lb.owner[ib], minlength=k) > 0
        has_t = np.bincount(lt.owner[it], minlength=k) > 0
        ib, it = ib[has_t[lb.owner[ib]]], it[has_b[lt.owner[it]]]
        if not ib.size or not it.size:
            return
        xb, xt = lb.rows[ib], lt.rows[it]
        ob, ot = lb.owner[ib], lt.owner[it]
        db, dt = B.kind[xb] == _DIR, T.kind[xt] == _DIR
        hb, wide_b, folded_b = self._keys(B, xb, fold)
        ht, wide_t, folded_t = self._keys(T, xt, fold)
        pb, pt, crowd_b, crowd_t = _pair_sorted(_key(hb, ob, db), _key(ht, ot, dt))
        ok = (hb[pb] == ht[pt]) & (ob[pb] == ot[pt]) & (db[pb] == dt[pt])
        wide = wide_b[pb] | wide_t[pt]
        good = np.zeros(len(pb), dtype=bool)
        lens = B.name_len[xb[pb]]
        sel = np.flatnonzero(ok & ~wide & (lens == T.name_len[xt[pt]]))  # word by word
        if sel.size:
            eq = _equal(B.img, B.pos(xb[pb[sel]]), T.img, T.pos(xt[pt[sel]]), lens[sel], fold=fold)
            good[sel[eq]] = True
        for j in np.flatnonzero(ok & wide).tolist():  # a name with other characters: normcase
            a, b = int(pb[j]), int(pt[j])
            good[j] = _normcase(B, int(xb[a]), folded_b.get(a)) == _normcase(T, int(xt[b]), folded_t.get(b))
        sel = np.flatnonzero(good)
        lb.match[ib[pb[sel]]] = it[pt[sel]]
        lt.match[it[pt[sel]]] = ib[pb[sel]]
        if crowd_b.size and crowd_t.size:
            self._crowd(lb, lt, ib[crowd_b], it[crowd_t], fold)

    def _keys(
        self, side: _Side, rows: np.ndarray, fold: bool
    ) -> tuple[np.ndarray, np.ndarray, dict[int, str]]:
        """Hash per row (ASCII-folded with ``fold``), which rows have non-ASCII names (with
        ``fold`` their hash is that of ``normcase`` of the name instead), and those folded names."""
        h, wide = _hash(side.img, side.pos(rows), side.name_len[rows], fold=fold)
        folded: dict[int, str] = {}
        if fold and bool(wide.any()):
            at = np.flatnonzero(wide)
            for j in at.tolist():
                folded[j] = _normcase(side, int(rows[j]), None)
            img, pos, lens = _Image.of([folded[j].encode("utf-8", NAME_ERRORS) for j in at.tolist()])
            h[at] = _hash(img, pos, lens, fold=True)[0]
        return h, wide, folded

    def _crowd(self, lb: _Level, lt: _Level, ib: np.ndarray, it: np.ndarray, fold: bool) -> None:
        """Pair the members of crowded key groups by their real names, in candidate order."""
        B, T = self.B, self.T

        def key(side: _Side, row: int, owner: int) -> tuple[int, bool, bytes | str]:
            name: bytes | str = _normcase(side, row, None) if fold else side.name(row)
            return owner, bool(side.kind[row] == _DIR), name

        waiting: dict[tuple[int, bool, bytes | str], list[int]] = {}
        for i in sorted(ib[lb.match[ib] < 0].tolist()):
            waiting.setdefault(key(B, int(lb.rows[i]), int(lb.owner[i])), []).append(i)
        for j in sorted(it[lt.match[it] < 0].tolist()):
            free = waiting.get(key(T, int(lt.rows[j]), int(lt.owner[j])))
            if free:
                i = free.pop(0)
                lb.match[i], lt.match[j] = j, i

    # ------------------------------------------------------------------ the union store

    def _build(
        self, base_id: np.ndarray, target_id: np.ndarray, parent: np.ndarray, sizes: np.ndarray
    ) -> DiffTree:
        B, T = self.B, self.T
        n = len(base_id)
        has_t = target_id >= 0
        at_t, at_b = np.flatnonzero(has_t), np.flatnonzero(~has_t)
        rt, rb = target_id[at_t], base_id[at_b]
        cols: dict[str, np.ndarray] = {}
        for key in (
            "kind",
            "flags",
            "name_len",
            "size",
            "alloc",
            "mtime",
            "ctime",
            "atime",
            "ino",
            "nlink",
            "ext",
        ):
            col = cols[key] = np.empty(n, dtype=COLUMNS[key])  # the target's row, else the base's
            col[at_t] = T.store.cols[key][rt]
            col[at_b] = B.store.cols[key][rb]
        ext_table, ext_map = _ext_tables(B.store.ext_table, T.store.ext_table)
        cols["ext"][at_b] = ext_map[cols["ext"][at_b]]
        cols["kind"][0], cols["flags"][0] = Kind.DIR, Flag.VIRTUAL
        self._tick(0.8)
        image, cols["name_off"] = self._names((T, rt, at_t), (B, rb, at_b), cols["name_len"])
        del at_t, at_b, rt, rb
        self._tick(0.85)

        cols["parent"] = parent
        cols["depth"] = np.repeat(np.arange(len(sizes)), sizes).astype(COLUMNS["depth"])
        count = np.bincount(parent[1:], minlength=n)
        cols["child_start"] = np.where(count > 0, 1 + np.cumsum(count) - count, 0).astype(
            COLUMNS["child_start"]
        )
        cols["child_count"] = count.astype(COLUMNS["child_count"])
        del count

        # both ends of every row
        base_alloc, base_size, own_delta = B.ends(base_id)
        target_alloc, target_size, own_t = T.ends(target_id)
        both = (base_id >= 0) & has_t
        status = np.where(both, Status.UNCHANGED, np.where(has_t, Status.ADDED, Status.REMOVED)).astype(
            np.uint8
        )
        status[both & ((base_alloc != target_alloc) | (base_size != target_size))] = Status.CHANGED
        # also changed: a file's own size (a hard-link duplicate counts 0 at both ends), a folder's
        # own blocks, the duplicate flag, file <-> link
        m = np.flatnonzero(both)
        mb, mt = base_id[m], target_id[m]
        bc, tc = B.store.cols, T.store.cols
        raw = (bc["size"][mb] != tc["size"][mt]) | (bc["alloc"][mb] != tc["alloc"][mt])
        raw |= (B.kind[mb] != T.kind[mt]) | (((B.flags[mb] ^ T.flags[mt]) & _DUP) != 0)
        status[m[raw]] = Status.CHANGED
        del m, mb, mt, raw, has_t
        np.subtract(own_t, own_delta, out=own_delta)
        del own_t
        self._tick(0.9)
        grow, shrink = self._totals(cols, sizes, own_delta, status, both)
        del own_delta, both

        store = NodeStore.from_columns(cols, image, ext_table, separated=True)
        del cols, image
        self._tick(1.0)
        return DiffTree(
            store=store,
            base_id=base_id.astype(np.int32),
            target_id=target_id.astype(np.int32),
            base_alloc=base_alloc,
            target_alloc=target_alloc,
            base_size=base_size,
            target_size=target_size,
            delta=target_alloc - base_alloc,
            status=status,
            grow=grow,
            shrink=shrink,
            fold_case=self.fold,
        )

    def _totals(
        self,
        cols: dict[str, np.ndarray],
        sizes: np.ndarray,
        own_delta: np.ndarray,
        status: np.ndarray,
        both: np.ndarray,
    ) -> tuple[np.ndarray, np.ndarray]:
        """Subtree totals of the union store (``cols``), growth and shrinkage, and the matched
        folders' status, folded up level by level (deepest first) with segment sums over the
        contiguous child ranges. Returns ``(grow, shrink)``."""
        n = len(own_delta)
        kind, flags = cols["kind"], cols["flags"]
        tot = {
            key: np.zeros(n, dtype=COLUMNS[key]) for key in ("tot_size", "tot_alloc", "tot_files", "tot_dirs")
        }
        grow = np.zeros(n, dtype=np.int64)
        shrink = np.zeros(n, dtype=np.int64)
        changed_below = np.zeros(n, dtype=bool)
        bounds = np.concatenate([[0], np.cumsum(sizes)]).tolist()
        start, count = cols["child_start"], cols["child_count"]
        for level in range(len(sizes) - 1, 0, -1):
            lo, hi = bounds[level], bounds[level + 1]
            sl = slice(lo, hi)
            folder = kind[sl] == _DIR
            counted = (flags[sl] & _DUP) == 0
            # a matched folder's status needs its children's (one level down: folded already)
            status[sl][folder & both[sl] & changed_below[sl]] = Status.CHANGED
            up = own_delta[sl]
            g_own, s_own = np.maximum(up, 0), np.maximum(-up, 0)
            files = ~folder
            grow[sl][files] = g_own[files]
            shrink[sl][files] = s_own[files]
            contrib = {
                "tot_size": np.where(counted, cols["size"][sl], 0) + tot["tot_size"][sl],
                "tot_alloc": np.where(counted, cols["alloc"][sl], 0) + tot["tot_alloc"][sl],
                "tot_files": (kind[sl] == Kind.FILE) + tot["tot_files"][sl].astype(np.int64),
                "tot_dirs": folder + tot["tot_dirs"][sl].astype(np.int64),
                "grow": np.where(folder, grow[sl] + g_own, g_own),
                "shrink": np.where(folder, shrink[sl] + s_own, s_own),
                "changed": (status[sl] != Status.UNCHANGED).astype(np.int64),
            }
            p_lo = bounds[level - 1]
            parents = np.flatnonzero(count[p_lo:lo] > 0) + p_lo
            if parents.size:
                at = start[parents].astype(np.int64) - lo
                for key in ("tot_size", "tot_alloc", "tot_files", "tot_dirs"):
                    tot[key][parents] = np.add.reduceat(contrib[key], at)
                grow[parents] = np.add.reduceat(contrib["grow"], at)
                shrink[parents] = np.add.reduceat(contrib["shrink"], at)
                changed_below[parents] = np.add.reduceat(contrib["changed"], at) > 0
            self._tick(0.9 + 0.1 * (len(sizes) - level) / len(sizes))
        status[0] = Status.CHANGED if bool(changed_below[0]) else Status.UNCHANGED
        cols.update(tot)
        return grow, shrink

    @staticmethod
    def _names(
        target: tuple[_Side, np.ndarray, np.ndarray],
        base: tuple[_Side, np.ndarray, np.ndarray],
        lens: np.ndarray,
    ) -> tuple[np.ndarray, np.ndarray]:
        """The union's ``\0name`` image in id order and each row's offset in it. ``target`` and
        ``base``: a side, its rows, and the union rows they are.

        Union rows whose source names follow each other too are copied as one run: long runs by
        slicing, the many short ones with a vectorised gather in bounded blocks.
        """
        ln = lens.astype(np.int64)
        off = np.cumsum(ln + 1) - ln
        image = np.zeros(int(off[-1] + ln[-1]), dtype=np.uint8)
        for side, rows, at in (target, base):
            if not at.size:
                continue
            src = side.pos(rows)
            ln_at = ln[at]
            cont = np.zeros(len(at), dtype=bool)  # continues the previous row's run
            cont[1:] = (at[1:] == at[:-1] + 1) & (src[1:] == src[:-1] + ln_at[:-1] + 1)
            first = np.flatnonzero(~cont)
            last = np.append(first[1:], len(at)) - 1
            r_dst = off[at[first]]
            _copy_runs(image, side.img.image, r_dst, src[first], off[at[last]] + ln_at[last] - r_dst)
        dtype = COLUMNS["name_off"] if len(image) < OFF_NARROW_LIMIT else np.dtype("<i8")
        return image, off.astype(dtype)


_LONG_RUN = 4096  # bytes: copied by slicing; shorter runs by a gather
_BLOCK = 1 << 22  # bytes per gather block (bounds the index temporaries)


def _copy_runs(dst: np.ndarray, src: np.ndarray, d: np.ndarray, s: np.ndarray, ln: np.ndarray) -> None:
    long = ln >= _LONG_RUN
    for a, b, k in zip(d[long].tolist(), s[long].tolist(), ln[long].tolist(), strict=True):
        dst[a : a + k] = src[b : b + k]
    d, s, ln = d[~long], s[~long], ln[~long]
    if not ln.size:
        return
    ends = np.cumsum(ln)
    cuts = np.searchsorted(ends, np.arange(_BLOCK, int(ends[-1]), _BLOCK))
    for lo, hi in zip([0, *cuts.tolist()], [*cuts.tolist(), len(ln)], strict=True):
        if hi > lo:
            k = ln[lo:hi]
            dst[ranges_to_ids(d[lo:hi], k)] = src[ranges_to_ids(s[lo:hi], k)]


def _ext_tables(base: list[str], target: list[str]) -> tuple[list[str], np.ndarray]:
    """The union's extension table (the target's, then the base's others) and base id -> union id."""
    table = list(target) or [""]
    index = {e: i for i, e in enumerate(table)}
    out = np.zeros(max(1, len(base)), dtype=np.uint16)
    for i, e in enumerate(base):
        j = index.get(e)
        if j is None:
            if len(table) >= 65535:
                j = 0
            else:
                j = index[e] = len(table)
                table.append(e)
        out[i] = j
    return table, out


__all__ = [
    "DOMINANT",
    "MINOR",
    "STATUS_LABELS",
    "TEXT",
    "Change",
    "CompareCancelled",
    "DiffTree",
    "Direction",
    "Status",
    "compare",
]

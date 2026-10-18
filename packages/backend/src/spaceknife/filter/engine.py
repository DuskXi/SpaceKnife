"""Vectorised evaluation of a filter's normal form over a :class:`NodeStore`.

Semantics (docs/filter-syntax.md, section 5): candidates are the non-directory entries. A file
matches when every facet holds — name/class (any), folder (any ancestor below the scan root),
tag (any of the colours, own or inherited), every size and age range, the attribute set — and no
negated name/class/folder/tag/attribute condition holds. Directories never match themselves;
their per-subtree match counts and bytes are aggregated bottom-up.

``evaluate`` only reads rows below a snapshot of ``len(store)``, so it can run in a worker thread
while a scan keeps appending to the store.
"""

from __future__ import annotations

import time
import weakref
from dataclasses import dataclass
from pathlib import PurePath

import numpy as np

from spaceknife_protocol.filter import AttrName, AttrSet, FilterNormal, TagColor
from spaceknife_protocol.filter_syntax import glob_parts

from ..classes import DEFAULT_CLASSES, ClassTable
from ..store.nodes import Flag, Kind, NodeStore
from .names import glob_str_regex, name_index

ATTR_FLAGS: dict[AttrName, Flag] = {
    "archive": Flag.ARCHIVE,
    "system": Flag.SYSTEM,
    "readonly": Flag.READONLY,
    "hidden": Flag.HIDDEN,
    "compressed": Flag.COMPRESSED,
    "encrypted": Flag.ENCRYPTED,
    "offline": Flag.OFFLINE,
    "temporary": Flag.TEMPORARY,
    "notindexed": Flag.NOT_INDEXED,
    "sparse": Flag.SPARSE,
    "ads": Flag.ADS,
}
TAG_BITS: dict[TagColor, int] = {"red": 1, "yellow": 2, "green": 4, "blue": 8}
AGE_COLUMNS = {"modify": "mtime", "create": "ctime", "access": "atime"}


@dataclass(frozen=True, slots=True)
class FilterContext:
    now: int
    classes: ClassTable = DEFAULT_CLASSES
    tags: np.ndarray | None = None  # uint8 per node: own tag bits (TAG_BITS); inherited downwards

    @classmethod
    def current(cls, **kw: object) -> FilterContext:
        return cls(now=int(time.time()), **kw)  # type: ignore[arg-type]


@dataclass(frozen=True, slots=True)
class FilterResult:
    n: int  # nodes evaluated (ids < n)
    match: np.ndarray  # bool per node: this file matches
    count: np.ndarray  # int64 per node: matching files in the subtree (1/0 for a file)
    alloc: np.ndarray  # int64 per node: allocated bytes of the matching files in the subtree
    size: np.ndarray  # int64 per node: logical bytes of the matching files in the subtree
    folder_hit: np.ndarray  # bool per node: a folder that matches a positive folder mask (bold)
    elapsed_s: float

    def totals(self, node: int = 0) -> tuple[int, int, int]:
        """(matching files, allocated bytes, logical bytes) below ``node``."""
        return int(self.count[node]), int(self.alloc[node]), int(self.size[node])


class _Levels:
    """Tree structure for level-by-level propagation; depends only on the node count.

    Nodes are append-only and a node's parent and depth never change, so one instance serves
    every evaluation until the store grows.
    """

    def __init__(self, store: NodeStore, n: int) -> None:
        c = store.cols
        depth = c["depth"][:n]
        self.n = n
        self.parent = c["parent"][:n].astype(np.int64)
        self.order = np.argsort(depth, kind="stable")  # by depth, ids ascending within a level
        self.max_depth = int(depth.max()) if n else 0
        self.bounds = np.searchsorted(depth[self.order], np.arange(self.max_depth + 2))

    def level(self, d: int) -> np.ndarray:
        return self.order[self.bounds[d] : self.bounds[d + 1]]


_LEVELS: weakref.WeakKeyDictionary[NodeStore, _Levels] = weakref.WeakKeyDictionary()


def _levels_of(store: NodeStore, n: int) -> _Levels:
    hit = _LEVELS.get(store)
    if hit is None or hit.n != n:
        hit = _LEVELS[store] = _Levels(store, n)
    return hit


class _Eval:
    def __init__(self, store: NodeStore, ctx: FilterContext) -> None:
        self.store = store
        self.ctx = ctx
        self.n = n = len(store)
        c = store.cols
        self.kind = c["kind"][:n]
        self.flags = c["flags"][:n]
        self.depth = c["depth"][:n]
        self.ext = c["ext"][:n]
        self.is_dir = self.kind == Kind.DIR
        self.leaf = ~self.is_dir
        self.levels = _levels_of(store, n)
        self.parent = self.levels.parent

    # ------------------------------------------------------------- names

    def name_mask(self, pattern: str) -> np.ndarray:
        """Bool per node: the name matches (files and directories alike)."""
        out = np.zeros(self.n, dtype=bool)
        parts = glob_parts(pattern)
        if parts == [("any", "")]:
            out[:] = True
            return out
        if len(parts) == 2 and parts[0] == ("any", "") and parts[1][0] == "lit":
            lit = parts[1][1]
            ext = lit[1:]
            if lit.startswith(".") and ext and "." not in ext and len(ext) <= 16:
                return self._ext_fast(pattern, ext)
        out[name_index(self.store).match(pattern, self.n)] = True
        return out

    def _ext_fast(self, pattern: str, ext: str) -> np.ndarray:
        """``*.ext``: files by extension id; other leaves and the dotfile ``.ext`` checked by name."""
        table = self.store.ext_table
        ids = np.array([i for i, e in enumerate(table) if i and e == ext], dtype=np.int64)
        is_file = self.kind == Kind.FILE
        out: np.ndarray = is_file & np.isin(self.ext, ids)
        name_len = self.store.cols["name_len"][: self.n]
        dotfile_len = len(("." + ext).encode("utf-8"))
        odd = (self.leaf & ~is_file) | (is_file & (self.ext == 0) & (name_len == dotfile_len))
        regex = glob_str_regex(pattern)
        for i in np.nonzero(odd)[0]:
            if regex.fullmatch(self.store.name(int(i))):
                out[i] = True
        return out

    # ------------------------------------------------------------- propagation

    def down(self, values: np.ndarray) -> np.ndarray:
        """OR ``values`` into every descendant (folder hits, inherited tags)."""
        out = values.copy()
        parent = self.parent
        for d in range(2, self.levels.max_depth + 1):
            idx = self.levels.level(d)
            out[idx] |= out[parent[idx]]
        return out

    def up(self, count: np.ndarray, *columns: np.ndarray) -> None:
        """Add every node's values into its ancestors, in place (``count`` and ``columns`` alike).

        Only nodes with a non-zero ``count`` carry anything, so each level touches just those.
        Within one depth level (ids ascending) a folder's children form one contiguous run: a
        segmented sum (``reduceat``) and a scatter without duplicate indices do the work.
        """
        parent = self.parent
        for d in range(self.levels.max_depth, 0, -1):
            idx = self.levels.level(d)
            if not len(idx):
                continue
            sel = idx[count[idx] != 0]
            if not len(sel):
                continue
            p = parent[sel]
            starts = np.concatenate(([0], np.flatnonzero(p[1:] != p[:-1]) + 1))
            owners = p[starts]
            for col in (count, *columns):
                col[owners] += np.add.reduceat(col[sel], starts)

    def folders(self, patterns: list[str]) -> np.ndarray:
        """Folders matching any mask: every folder below a scan root, and the root itself.

        A root is stored under its full path; it takes part under its own name (``D:\\Photos`` →
        ``Photos``), a volume root (``C:\\``, ``/``) under the empty name. Folders above the root
        never take part, so the result does not depend on where the scan started.
        """
        hit = np.zeros(self.n, dtype=bool)
        for pattern in patterns:
            hit |= self.name_mask(pattern)
        hit &= self.is_dir & (self.depth >= 2)
        regexes = [glob_str_regex(p) for p in patterns]
        for root in self.store.roots():
            if root < self.n and self.is_dir[root]:
                base = PurePath(self.store.name(root)).name
                hit[root] = any(r.fullmatch(base) for r in regexes)
        return hit

    def classes(self, class_id: str) -> np.ndarray:
        by_ext = self.ctx.classes.ext_mask(class_id, self.store.ext_table)
        result: np.ndarray = self.leaf & by_ext[self.ext]
        return result

    def attrs(self, s: AttrSet) -> np.ndarray:
        req = int(sum(ATTR_FLAGS[a] for a in s.required))
        forb = int(sum(ATTR_FLAGS[a] for a in s.forbidden))
        f = self.flags
        result: np.ndarray = ((f & np.uint32(req)) == req) & ((f & np.uint32(forb)) == 0)
        return result

    def tags(self) -> np.ndarray:
        own = self.ctx.tags
        if own is None:
            return np.zeros(self.n, dtype=np.uint8)
        bits = np.zeros(self.n, dtype=np.uint8)
        m = min(self.n, len(own))
        bits[:m] = own[:m]
        return self.down(bits)

    # ------------------------------------------------------------- facets

    def run(self, nf: FilterNormal) -> tuple[np.ndarray, np.ndarray]:
        ok = self.leaf.copy()
        if nf.masks or nf.classes:
            what = np.zeros(self.n, dtype=bool)
            for m in nf.masks:
                what |= self.name_mask(m)
            for k in nf.classes:
                what |= self.classes(k)
            ok &= what
        for m in nf.not_masks:
            ok &= ~self.name_mask(m)
        for k in nf.not_classes:
            ok &= ~self.classes(k)
        folder_hit = self.folders(nf.folders) if nf.folders else np.zeros(self.n, dtype=bool)
        if nf.folders:
            ok &= self.down(folder_hit)
        if nf.not_folders:
            ok &= ~self.down(self.folders(nf.not_folders))
        if nf.tags is not None or nf.not_tags:
            eff = self.tags()
            if nf.tags is not None:
                want = sum(TAG_BITS[t] for t in nf.tags)
                ok &= (eff & np.uint8(want)) != 0
            if nf.not_tags:
                drop = sum(TAG_BITS[t] for t in nf.not_tags)
                ok &= (eff & np.uint8(drop)) == 0
        c = self.store.cols
        for r in nf.sizes:
            v = c["alloc" if r.field == "disk" else "size"][: self.n]
            if r.lo is not None:
                ok &= v >= r.lo
            if r.hi is not None:
                ok &= v <= r.hi
        for r in nf.ages:
            t = c[AGE_COLUMNS[r.field]][: self.n].astype(np.int64)
            age = self.ctx.now - t
            ok &= t != 0  # unknown timestamp: no age condition holds
            if r.lo is not None:
                ok &= age >= r.lo
            if r.hi is not None:
                ok &= age <= r.hi
        if nf.attrs is not None:
            ok &= self.attrs(nf.attrs)
        for s in nf.not_attrs:
            ok &= ~self.attrs(s)
        return ok, folder_hit


def evaluate(store: NodeStore, nf: FilterNormal, ctx: FilterContext | None = None) -> FilterResult:
    """Evaluate a normal form; an empty one matches every file."""
    t0 = time.perf_counter()
    ev = _Eval(store, ctx or FilterContext.current())
    match, folder_hit = ev.run(nf)
    c = store.cols
    count = match.astype(np.int64)
    hit = np.flatnonzero(match)
    hit = hit[(ev.flags[hit] & np.uint32(Flag.HARDLINK_DUP)) == 0]  # duplicates add no bytes
    alloc = np.zeros(ev.n, dtype=np.int64)
    size = np.zeros(ev.n, dtype=np.int64)
    alloc[hit] = c["alloc"][hit]
    size[hit] = c["size"][hit]
    ev.up(count, alloc, size)
    return FilterResult(
        n=ev.n,
        match=match,
        count=count,
        alloc=alloc,
        size=size,
        folder_hit=folder_hit,
        elapsed_s=time.perf_counter() - t0,
    )

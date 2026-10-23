"""What a job found, and what it means for the views (docs/dupes.md §8, §10).

:class:`Results` is the engine's sink. It keeps, per verified group, its members as rows of
:data:`codes.MEMBER_DTYPE` (64 bytes) plus their paths, applies the keep rules (``keep.py``) to
give each member a role, and derives:

- the ledger: what deleting the extra copies frees, per volume;
- per scan, a sorted index of member nodes (hover cards, frame annotation, ``locate``);
- per scan, the rollup of extra bytes onto every folder above them (sparse: node i4, bytes i8);
- the frame columns ``dup`` (u1) and ``dupg`` (u4) of the 「重复」 colouring;
- the coverage of a folder: what in it exists elsewhere.

Node ids move when the tree changes (monitor relists): :meth:`remap` maps them, :meth:`invalidate`
takes changed members out (their group goes back to 「待核对」 when fewer than two remain).
"""

from __future__ import annotations

import os
import threading
import time
from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

import numpy as np

from ..store.nodes import Kind, NodeStore
from . import codes as C
from .keep import KeepOptions, UnitView, choose, folder_sides
from .select import ScanInput

if TYPE_CHECKING:
    from .engine import Job

_FOLDERISH = (1, 2, 3)  # RectKind FOLDER, COLLAPSED, PENDING
_AGG = (4, 5)  # SMALL, MORE
_RECT_FILE = 0


@dataclass(slots=True)
class Group:
    id: int
    size: int
    digest: bytes | None
    shared: bool  # every copy is the same physical data (names of one file, block clones)
    start: int
    count: int
    units: int
    volumes: tuple[int, ...] = ()
    reclaim: int = 0
    shared_bytes: int = 0
    keep_why: int = 0
    keep_row: int = -1
    valid: bool = True
    names: int = 1  # distinct file names


@dataclass(slots=True)
class Ledger:
    extra: int = 0  # files that would go
    reclaim: int = 0  # bytes they free
    shared: int = 0  # bytes of names / clones that free nothing
    needed: int = 0  # bytes of copies that are always kept beyond the first
    groups: int = 0


@dataclass(slots=True)
class _ScanIndex:
    version: int
    nodes: np.ndarray  # sorted member nodes of the scan
    rows: np.ndarray  # member row of each
    folders: np.ndarray  # sorted folder nodes with extra bytes below them
    folder_bytes: np.ndarray
    pending: np.ndarray  # sorted candidate nodes still undecided
    skipped: np.ndarray  # sorted candidate nodes skipped


class Results:
    def __init__(self, scans: Sequence[ScanInput], keep: KeepOptions | None = None) -> None:
        self.scans = list(scans)
        self.keep = keep or KeepOptions()
        self.m = np.zeros(1024, C.MEMBER_DTYPE)
        self.n = 0
        self.paths: list[str] = []
        self.btime: list[int] = []
        self.groups: list[Group] = []
        self._by_cand_group: dict[int, list[int]] = {}
        self.version = 0
        self.lock = threading.RLock()
        self._index: dict[int, _ScanIndex] = {}
        self.job: Job | None = None
        self.vol_names: list[str] = []
        self.vol_notes: dict[int, list[str]] = {}
        self.finished_at = 0.0
        self.started_at = time.time()

    # ------------------------------------------------------------------ sink
    def start(self, job: Job) -> None:
        self.job = job

    def finish(self, job: Job) -> None:
        with self.lock:
            self.vol_names = [v.name for v in job.vols]
            self.vol_notes = {v.index: list(v.notes) for v in job.vols if v.notes}
            self.finished_at = time.time()
            self.version += 1

    def add_group(
        self,
        job: Job,
        g: int,
        size: int,
        digest: bytes | None,
        members: list[dict[str, Any]],
        *,
        shared: bool,
    ) -> None:
        with self.lock:
            self.vol_names = [v.name for v in job.vols]
            k = len(members)
            if self.n + k > len(self.m):
                grown = np.zeros(max(len(self.m) * 2, self.n + k), C.MEMBER_DTYPE)
                grown[: self.n] = self.m[: self.n]
                self.m = grown
            start = self.n
            reps: dict[int, int] = {}
            for j, mem in enumerate(members):
                row = start + j
                r = self.m[row]
                r["fid_lo"], r["fid_hi"] = mem["fid_lo"], mem["fid_hi"]
                r["mtime"], r["change"], r["usn"] = mem["mtime"], mem["change"], mem["usn"]
                sc = self.scans[mem["scan"]]
                node = mem["node"]
                r["alloc"] = int(sc.store.cols["alloc"][node])
                r["node"] = node
                r["group"] = len(self.groups)
                r["scan"] = mem["scan"]
                r["vol"] = mem["vol"]
                fl = int(mem["flags"]) & 0xFF
                if mem.get("external"):
                    fl |= C.F_PINNED
                r["flags"] = fl
                if mem["kind"] == "rep":
                    reps[mem["unit"]] = row
                r["unit"] = reps.get(mem["unit"], row)
                r["role"] = (
                    C.ROLE_ALIAS
                    if mem["kind"] == "alias"
                    else (C.ROLE_CLONE if mem["kind"] == "clone" else 0)
                )
                self.paths.append(mem["path"])
                self.btime.append(int(mem.get("btime", 0)))
            self.n += k
            vols = tuple(sorted({int(m["vol"]) for m in members}))
            names = len({os.path.basename(m["path"]).lower() for m in members})
            grp = Group(
                id=len(self.groups),
                size=size,
                digest=digest,
                shared=shared,
                start=start,
                count=k,
                units=len(reps),
                volumes=vols,
                names=names,
            )
            self.groups.append(grp)
            self._by_cand_group.setdefault(g, []).append(grp.id)
            self._apply_one(grp)
            self.version += 1
            self._index.clear()

    def invalidate_rows(self, job: Job, cand_rows: Sequence[int]) -> None:
        sel = job.sel
        if sel is None:
            return
        keys = {(int(sel.cand["scan"][r]), int(sel.cand["node"][r])) for r in cand_rows}
        with self.lock:
            rows = [i for i in range(self.n) if (int(self.m["scan"][i]), int(self.m["node"][i])) in keys]
            self.invalidate(rows)

    # ------------------------------------------------------------------ keep rules
    def set_keep(self, keep: KeepOptions) -> None:
        with self.lock:
            self.keep = keep
            self.apply_keep()

    def apply_keep(self) -> None:
        with self.lock:
            side = self._folder_sides()
            for g in self.groups:
                self._apply_one(g, side)
            self.version += 1
            self._index.clear()

    def _units(self, g: Group) -> list[UnitView]:
        m = self.m
        units: dict[int, UnitView] = {}
        for row in range(g.start, g.start + g.count):
            rep = int(m["unit"][row])
            u = units.get(rep)
            fl = int(m["flags"][row])
            if u is None:
                u = units[rep] = UnitView(
                    rows=[],
                    path=self.paths[rep],
                    vol=int(m["vol"][rep]),
                    btime=self.btime[rep],
                    mtime=int(m["mtime"][rep]),
                    forced=False,
                )
            u.rows.append(row)
            if fl & C.F_NEEDED:
                u.forced, u.forced_why = True, C.WHY_NEEDED
            elif fl & C.F_PINNED and not u.forced:
                u.forced, u.forced_why = True, C.WHY_PINNED
            elif fl & C.F_NETWORK and not u.forced:
                u.forced, u.forced_why = True, C.WHY_NEEDED
        for u in units.values():
            if len(u.rows) > 1 and not u.forced:
                u.forced, u.forced_why = True, C.WHY_PINNED  # several names: only ever kept (phase 1)
        return list(units.values())

    def _apply_one(self, g: Group, side: dict[str, int] | None = None) -> None:
        m = self.m
        units = self._units(g)
        if g.shared or not g.valid:
            for u in units:
                for row in u.rows:
                    base = int(m["role"][row]) & 0x0F
                    if base not in (C.ROLE_ALIAS, C.ROLE_CLONE):
                        base = C.ROLE_CLONE if g.shared and row != u.rows[0] else C.ROLE_KEEP
                    m["role"][row] = base
            g.reclaim = 0
            g.shared_bytes = sum(int(m["alloc"][r]) for u in units for r in u.rows[1:])
            return
        roles = choose(units, self.keep, digest=g.digest, side=side)
        reclaim = 0
        shared = 0
        g.keep_row = -1
        for u, (role, why) in zip(units, roles, strict=True):
            for k, row in enumerate(u.rows):
                base = int(m["role"][row]) & 0x0F
                if k > 0 and base in (C.ROLE_ALIAS, C.ROLE_CLONE, 0):
                    m["role"][row] = base or C.ROLE_ALIAS
                    shared += int(m["alloc"][row])
                    continue
                r = role
                if u.forced and int(m["flags"][row]) & C.F_NEEDED:
                    r = C.ROLE_NEEDED if role == C.ROLE_KEEP and why == C.WHY_NEEDED else role
                m["role"][row] = (r & 0x0F) | ((why & 0x0F) << 4)
                if role == C.ROLE_EXTRA:
                    reclaim += int(m["alloc"][row])
                elif g.keep_row < 0:
                    g.keep_row, g.keep_why = row, why
        g.reclaim = reclaim
        g.shared_bytes = shared

    def _folder_sides(self) -> dict[str, int] | None:
        if not self.keep.folder_consistent:
            return None
        pairs: dict[tuple[str, str], int] = {}
        info: dict[str, tuple[int, int]] = {}
        for g in self.groups:
            if g.shared or not g.valid or g.units != 2:
                continue
            units = self._units(g)
            if any(u.forced for u in units):
                continue
            a, b = os.path.dirname(units[0].path), os.path.dirname(units[1].path)
            if a == b:
                continue
            key = (a, b) if a < b else (b, a)
            pairs[key] = pairs.get(key, 0) + 1
            for u in units:
                d = os.path.dirname(u.path)
                bt = u.btime if u.btime > 0 else u.mtime
                old = info.get(d)
                info[d] = (min(old[0], bt), max(old[1], u.mtime)) if old else (bt, u.mtime)
        winners = folder_sides(pairs, info, self.keep)
        if not winners:
            return None
        side: dict[str, int] = {}
        for (a, b), w in winners.items():
            side[w] = 0
            loser = b if w == a else a
            side.setdefault(loser, 1)
        return side

    # ------------------------------------------------------------------ queries
    def role(self, row: int) -> int:
        return int(self.m["role"][row]) & 0x0F

    def why(self, row: int) -> int:
        return (int(self.m["role"][row]) >> 4) & 0x0F

    def valid_groups(self) -> list[Group]:
        return [g for g in self.groups if g.valid]

    def ledger(self) -> dict[int, Ledger]:
        out: dict[int, Ledger] = {}
        with self.lock:
            m = self.m
            for g in self.groups:
                if not g.valid:
                    continue
                for row in range(g.start, g.start + g.count):
                    v = int(m["vol"][row])
                    led = out.setdefault(v, Ledger())
                    r = self.role(row)
                    if r == C.ROLE_EXTRA:
                        led.extra += 1
                        led.reclaim += int(m["alloc"][row])
                    elif r in (C.ROLE_ALIAS, C.ROLE_CLONE):
                        led.shared += int(m["alloc"][row])
                for v in set(g.volumes):
                    out.setdefault(v, Ledger()).groups += 1
        return out

    def totals(self) -> tuple[int, int, int]:
        """(groups with something to free, extra files, bytes freed)."""
        with self.lock:
            gs = [g for g in self.groups if g.valid and not g.shared and g.reclaim > 0]
            extra = int(
                sum(
                    1
                    for i in range(self.n)
                    if self.role(i) == C.ROLE_EXTRA and self.groups[int(self.m["group"][i])].valid
                )
            )
            return len(gs), extra, int(sum(g.reclaim for g in gs))

    # ------------------------------------------------------------------ per scan index
    def scan_index(self, si: int) -> _ScanIndex:
        with self.lock:
            hit = self._index.get(si)
            job_ver = self.job.done_groups if self.job is not None else 0
            if hit is not None and hit.version == self.version * 1_000_003 + job_ver:
                return hit
            m = self.m[: self.n]
            valid = (
                np.array([self.groups[int(g)].valid for g in m["group"]], dtype=bool)
                if self.n
                else np.zeros(0, bool)
            )
            mine = np.flatnonzero((m["scan"] == si) & valid)
            order = np.argsort(m["node"][mine], kind="stable")
            rows = mine[order]
            nodes = m["node"][rows].astype(np.int64)
            folders, fbytes = self._rollup(si, m, valid)
            pending, skipped = self._pending(si)
            idx = _ScanIndex(
                self.version * 1_000_003 + job_ver,
                nodes,
                rows.astype(np.int64),
                folders,
                fbytes,
                pending,
                skipped,
            )
            self._index[si] = idx
            return idx

    def _rollup(self, si: int, m: np.ndarray, valid: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
        roles = m["role"] & 0x0F
        sel = (m["scan"] == si) & valid & (roles == C.ROLE_EXTRA)
        if not sel.any():
            return np.zeros(0, np.int64), np.zeros(0, np.int64)
        store = self.scans[si].store
        parent = store.cols["parent"]
        cur = m["node"][sel].astype(np.int64)
        vals = m["alloc"][sel].astype(np.int64)
        keys: list[np.ndarray] = []
        weights: list[np.ndarray] = []
        for _ in range(4096):
            cur = parent[cur].astype(np.int64)
            live = cur > 0
            if not live.any():
                break
            cur, vals = cur[live], vals[live]
            keys.append(cur)
            weights.append(vals)
        if not keys:
            return np.zeros(0, np.int64), np.zeros(0, np.int64)
        k = np.concatenate(keys)
        w = np.concatenate(weights)
        u, inv = np.unique(k, return_inverse=True)
        sums = np.zeros(len(u), np.int64)
        np.add.at(sums, inv, w)
        return u, sums

    def _pending(self, si: int) -> tuple[np.ndarray, np.ndarray]:
        job = self.job
        if job is None or job.sel is None or not len(job.sel.cand):
            return np.zeros(0, np.int64), np.zeros(0, np.int64)
        cand = job.sel.cand
        mine = cand["scan"] == si
        st = cand["state"]
        if job.state in ("done", "cancelled", "failed"):
            pend = np.zeros(len(cand), bool)
        else:
            pend = mine & np.isin(st, (C.ST_SELECTED, C.ST_PROBED, C.ST_VERIFYING))
        skip = mine & (st == C.ST_SKIPPED)
        return np.sort(cand["node"][pend].astype(np.int64)), np.sort(cand["node"][skip].astype(np.int64))

    def member_of(self, si: int, node: int) -> int:
        """The member row of a node of scan ``si``, or -1."""
        idx = self.scan_index(si)
        p = int(np.searchsorted(idx.nodes, node))
        if p < len(idx.nodes) and int(idx.nodes[p]) == node:
            return int(idx.rows[p])
        return -1

    def redundant(self, si: int, node: int) -> int:
        idx = self.scan_index(si)
        p = int(np.searchsorted(idx.folders, node))
        if p < len(idx.folders) and int(idx.folders[p]) == node:
            return int(idx.folder_bytes[p])
        return 0

    def is_pending(self, si: int, node: int) -> bool:
        idx = self.scan_index(si)
        p = int(np.searchsorted(idx.pending, node))
        return p < len(idx.pending) and int(idx.pending[p]) == node

    # ------------------------------------------------------------------ frames
    def annotate(self, si: int, store: NodeStore, draw: Any) -> tuple[np.ndarray, np.ndarray]:
        """``dup`` (u1) and ``dupg`` (u4) for every row of a frame of scan ``si``."""
        n = len(draw)
        dup = np.zeros(n, np.uint8)
        dupg = np.zeros(n, np.uint32)
        if not n:
            return dup, dupg
        with self.lock:
            idx = self.scan_index(si)
            node = draw.node.astype(np.int64)
            kind = draw.kind
            files = (kind == _RECT_FILE) & (node >= 0)
            if files.any() and len(idx.nodes):
                fn = node[files]
                p = np.minimum(np.searchsorted(idx.nodes, fn), len(idx.nodes) - 1)
                hit = idx.nodes[p] == fn
                rows = np.where(hit, idx.rows[p], -1)
                roles = np.where(hit, self.m["role"][np.maximum(rows, 0)] & 0x0F, 0)
                state = np.zeros(len(fn), np.uint8)
                state[(roles == C.ROLE_KEEP) | (roles == C.ROLE_NEEDED)] = C.DUP_KEEP
                state[roles == C.ROLE_EXTRA] = C.DUP_EXTRA
                state[(roles == C.ROLE_ALIAS) | (roles == C.ROLE_CLONE)] = C.DUP_SHARED
                gid = np.where(hit, self.m["group"][np.maximum(rows, 0)].astype(np.int64) + 1, 0)
                shared_g = np.array([self.groups[int(x) - 1].shared if x else False for x in gid], dtype=bool)
                state[shared_g & hit] = C.DUP_SHARED
                rest = ~hit
                if rest.any():
                    for arr, code in ((idx.pending, C.DUP_PENDING), (idx.skipped, C.DUP_SKIPPED)):
                        if len(arr):
                            q = np.minimum(np.searchsorted(arr, fn[rest]), len(arr) - 1)
                            mark = arr[q] == fn[rest]
                            sub = state[rest]
                            sub[mark & (sub == 0)] = code
                            state[rest] = sub
                dup[files] = state
                dupg[files] = gid.astype(np.uint32)
            elif files.any():
                fn = node[files]
                state = np.zeros(len(fn), np.uint8)
                for arr, code in ((idx.pending, C.DUP_PENDING), (idx.skipped, C.DUP_SKIPPED)):
                    if len(arr):
                        q = np.minimum(np.searchsorted(arr, fn), len(arr) - 1)
                        mark = (arr[q] == fn) & (state == 0)
                        state[mark] = code
                dup[files] = state
            # folders: the share of their bytes that are extra copies
            fold = np.isin(kind, _FOLDERISH) & (node >= 0)
            if fold.any() and len(idx.folders):
                fn = node[fold]
                p = np.minimum(np.searchsorted(idx.folders, fn), len(idx.folders) - 1)
                red = np.where(idx.folders[p] == fn, idx.folder_bytes[p], 0)
                dup[fold] = _tiers(red, draw.size[fold])
            for row in np.flatnonzero(np.isin(kind, _AGG)):
                members = draw.members.get(int(draw.key[row]))
                if members is None or not len(members):
                    continue
                red_bytes = self._redundant_of(si, store, np.asarray(members, np.int64))
                dup[row] = _tiers(np.array([red_bytes]), np.array([int(draw.size[row])]))[0]
        return dup, dupg

    def _redundant_of(self, si: int, store: NodeStore, nodes: np.ndarray) -> int:
        idx = self.scan_index(si)
        total = 0
        kind = store.cols["kind"][nodes]
        files = nodes[kind != Kind.DIR]
        dirs = nodes[kind == Kind.DIR]
        if len(files) and len(idx.nodes):
            p = np.minimum(np.searchsorted(idx.nodes, files), len(idx.nodes) - 1)
            hit = idx.nodes[p] == files
            rows = idx.rows[p][hit]
            roles = self.m["role"][rows] & 0x0F
            total += int(self.m["alloc"][rows][roles == C.ROLE_EXTRA].sum())
        if len(dirs) and len(idx.folders):
            p = np.minimum(np.searchsorted(idx.folders, dirs), len(idx.folders) - 1)
            total += int(np.where(idx.folders[p] == dirs, idx.folder_bytes[p], 0).sum())
        return total

    def locate(
        self, si: int, group: int, draw: Any, limit: int = 2000
    ) -> tuple[list[tuple[int, bool, int]], bool]:
        """Rows of a frame that show (exact) or hold (inside an aggregate or folder) the copies of
        ``group``: (row, exact, role)."""
        with self.lock:
            if not 0 <= group < len(self.groups):
                return [], False
            g = self.groups[group]
            rows = [r for r in range(g.start, g.start + g.count) if int(self.m["scan"][r]) == si]
            nodes = {int(self.m["node"][r]): self.role(r) for r in rows}
        out: list[tuple[int, bool, int]] = []
        if not nodes:
            return out, False
        store = self.scans[si].store
        parent = store.cols["parent"]
        node_rows: dict[int, int] = {}
        for i, nd in enumerate(draw.node.tolist()):
            if nd >= 0 and int(draw.kind[i]) in (0, 1, 2, 3):
                node_rows.setdefault(int(nd), i)
        agg: dict[int, int] = {}
        for i in np.flatnonzero(np.isin(draw.kind, _AGG)).tolist():
            for nd in draw.members.get(int(draw.key[i]), ()):
                agg.setdefault(int(nd), i)
        truncated = False
        for nd, role in sorted(nodes.items()):
            if len(out) >= limit:
                truncated = True
                break
            if nd in node_rows and int(draw.kind[node_rows[nd]]) == 0:
                out.append((node_rows[nd], True, role))
                continue
            cur = nd
            found = -1
            for _ in range(4096):
                if cur in agg:
                    found = agg[cur]
                    break
                if cur in node_rows and cur != nd:
                    found = node_rows[cur]
                    break
                cur = int(parent[cur])
                if cur <= 0:
                    break
            if found >= 0:
                out.append((found, False, role))
        return out, truncated

    # ------------------------------------------------------------------ tree changes
    def remap(self, si: int, mapping: Any) -> None:
        """Node ids of scan ``si`` moved (``mapping(old ids) -> new ids``, -1 = gone)."""
        with self.lock:
            sel = self.m["scan"][: self.n] == si
            if not sel.any():
                return
            old = self.m["node"][: self.n][sel].astype(np.int64)
            new = np.asarray(mapping(old), dtype=np.int64)
            col = self.m["node"][: self.n]
            col[sel] = np.maximum(new, 0).astype(np.int32)
            gone = np.flatnonzero(sel)[new < 0]
            self.invalidate(gone.tolist())
            self._index.clear()
            self.version += 1

    def changed_nodes(self, si: int, nodes: Iterable[int]) -> list[int]:
        """Member rows of scan ``si`` among ``nodes`` (files that changed)."""
        idx = self.scan_index(si)
        out: list[int] = []
        arr = np.fromiter((int(x) for x in nodes), dtype=np.int64)
        if not len(arr) or not len(idx.nodes):
            return out
        p = np.minimum(np.searchsorted(idx.nodes, arr), len(idx.nodes) - 1)
        hit = idx.nodes[p] == arr
        found: list[int] = idx.rows[p][hit].astype(np.int64).tolist()
        return found

    def invalidate(self, rows: Sequence[int]) -> int:
        """Take changed members out; groups left with fewer than two physical files go away.
        Returns how many groups were affected."""
        if not len(rows):
            return 0
        with self.lock:
            touched = {int(self.m["group"][r]) for r in rows}
            for r in rows:
                self.m["role"][r] = 0
                self.m["flags"][r] = int(self.m["flags"][r]) | 0x80  # changed: out of the group
            for gid in touched:
                g = self.groups[gid]
                g.valid = False  # back to 「待核对」: the next run (or the monitor's recheck) decides
                self._apply_one(g)
            self._index.clear()
            self.version += 1
            return len(touched)

    # ------------------------------------------------------------------ coverage
    def coverage(self, si: int, folder: int, *, limit: int = 50) -> Coverage:
        """What of ``folder``'s files (at least the job's minimum size) exists elsewhere."""
        from .select import _below

        out = Coverage()
        job = self.job
        store = self.scans[si].store
        cols = store.cols
        tops = np.array([folder], dtype=np.int64)
        only: list[int] = []
        counted = 0
        with self.lock:
            if job is not None and job.sel is not None:
                sel = job.sel
                uniq = sel.unique_nodes.get(si)
                if uniq is not None and len(uniq):
                    inside = uniq[_below(store, uniq, tops)]
                    counted += len(inside)
                    only.extend(inside.tolist())
                cand = sel.cand
                mine = np.flatnonzero(cand["scan"] == si)
                nodes = cand["node"][mine].astype(np.int64)
                hit = _below(store, nodes, tops)
                rows = mine[hit]
                counted += len(rows)
                for r in rows.tolist():
                    node = int(cand["node"][r])
                    st = int(cand["state"][r])
                    b = int(cols["alloc"][node])
                    mrow = self.member_of(si, node)
                    g = self.groups[int(self.m["group"][mrow])] if mrow >= 0 else None
                    if g is not None and g.valid:
                        unit = int(self.m["unit"][mrow])
                        other = same = False
                        for o in range(g.start, g.start + g.count):
                            osi = int(self.m["scan"][o])
                            onode = int(self.m["node"][o])
                            if osi == si and bool(_below(store, np.array([onode], np.int64), tops)[0]):
                                continue
                            if int(self.m["unit"][o]) == unit or g.shared:
                                same = True
                            else:
                                other = True
                        if other:
                            out.elsewhere += 1
                            out.elsewhere_bytes += b
                        elif same:
                            out.same_data += 1
                            out.same_data_bytes += b
                        else:
                            only.append(node)
                    elif st == C.ST_UNIQUE:
                        only.append(node)
                    else:
                        out.unchecked += 1
                        out.unchecked_bytes += b
            for node in only:
                out.only_here += 1
                out.only_here_bytes += int(cols["alloc"][node])
            biggest = sorted(only, key=lambda nd: -int(cols["alloc"][nd]))[:limit]
            out.only_here_list = [(nd, store.path(nd), int(cols["alloc"][nd])) for nd in biggest]
            out.small = max(0, int(cols["tot_files"][folder]) - counted)
            seen = out.only_here_bytes + out.elsewhere_bytes + out.same_data_bytes + out.unchecked_bytes
            out.small_bytes = max(0, int(cols["tot_alloc"][folder]) - seen)
        return out

    # ------------------------------------------------------------------ pages
    def sorted_groups(
        self,
        sort: str = "reclaim",
        *,
        cross_only: bool = False,
        include_shared: bool = False,
        scan: int | None = None,
        under: tuple[int, int] | None = None,
    ) -> list[Group]:
        with self.lock:
            gs = [g for g in self.groups if g.valid and (include_shared or not g.shared)]
            if cross_only:
                gs = [g for g in gs if len(g.volumes) > 1]
            if scan is not None:
                gs = [
                    g
                    for g in gs
                    if any(int(self.m["scan"][r]) == scan for r in range(g.start, g.start + g.count))
                ]
            if under is not None:
                si, node = under
                parent = self.scans[si].store.cols["parent"]

                def inside(r: int) -> bool:
                    if int(self.m["scan"][r]) != si:
                        return False
                    cur = int(self.m["node"][r])
                    while cur > 0:
                        if cur == node:
                            return True
                        cur = int(parent[cur])
                    return False

                gs = [g for g in gs if any(inside(r) for r in range(g.start, g.start + g.count))]
            key = {
                "reclaim": lambda g: (-g.reclaim, -g.size, g.id),
                "size": lambda g: (-g.size, g.id),
                "copies": lambda g: (-g.count, -g.size, g.id),
                "path": lambda g: (self.paths[g.start].lower(), g.id),
            }.get(sort)
            if key is None:
                key = lambda g: (-g.reclaim, g.id)  # noqa: E731
            return sorted(gs, key=key)

    def member_dict(self, row: int, scan_ids: Sequence[str]) -> dict[str, Any]:
        m = self.m[row]
        role = self.role(row)
        v = int(m["vol"])
        return {
            "scan_id": scan_ids[int(m["scan"])],
            "node": int(m["node"]),
            "path": self.paths[row],
            "volume": self.vol_names[v] if v < len(self.vol_names) else "",
            "mtime": int(m["mtime"]) // 1_000_000_000,
            "ctime": self.btime[row] // 1_000_000_000,
            "alloc": int(m["alloc"]),
            "role": C.ROLE_NAMES.get(role, "keep"),
            "why": C.WHY_NAMES.get(self.why(row)) if role in (C.ROLE_KEEP, C.ROLE_NEEDED) else None,
        }


def _tiers(red: np.ndarray, size: np.ndarray) -> np.ndarray:
    """Folder tiers: 0 none, 1 some, 2 ≥10 %, 3 ≥40 %, 4 ≥80 % of the bytes are extra copies."""
    red = red.astype(np.float64)
    tot = np.maximum(size.astype(np.float64), 1.0)
    share = red / tot
    out = np.zeros(len(red), np.uint8)
    out[red > 0] = 1
    out[(red > 0) & (share >= C.TIER_EDGES[0])] = 2
    out[(red > 0) & (share >= C.TIER_EDGES[1])] = 3
    out[(red > 0) & (share >= C.TIER_EDGES[2])] = 4
    return out


@dataclass(slots=True)
class Coverage:
    """What of a folder's eligible files exists elsewhere (docs/dupes.md §8.4)."""

    only_here: int = 0
    only_here_bytes: int = 0
    elsewhere: int = 0
    elsewhere_bytes: int = 0
    unchecked: int = 0
    unchecked_bytes: int = 0
    same_data: int = 0
    same_data_bytes: int = 0
    small: int = 0
    small_bytes: int = 0
    only_here_list: list[tuple[int, str, int]] = field(default_factory=list)  # (node, path, bytes)

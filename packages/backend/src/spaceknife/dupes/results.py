"""What a job found, and what it means for the views (docs/dupes.md §8, §10).

:class:`Results` is the engine's sink. It keeps, per verified group, its members as rows of
:data:`codes.MEMBER_DTYPE` (64 bytes) plus their paths and birth times, applies the keep rules
(``keep.py``) to give each member a role, and derives:

- the ledger: what deleting the extra copies frees, per volume;
- per scan, a sorted index of member nodes (hover cards, frame annotation, ``locate``);
- per scan, the rollup of extra bytes onto every folder above them (sparse: node i4, bytes i8);
- the frame columns ``dup`` (u1) and ``dupg`` (u4) of the 「重复」 colouring;
- the coverage of a folder: what in it exists elsewhere.

Node ids move when the tree changes (monitor relists): :meth:`remap` maps them, :meth:`invalidate`
takes changed members out (their group goes back to 「待核对」 when fewer than two remain).

Everything the event loop asks often (status four times a second, frames, hover cards) is numpy
over the member table or kept per group as it changes; the keep rules over every group (end of a
job, a rule changed) run in a thread on a snapshot and are committed if nothing changed meanwhile.
"""

from __future__ import annotations

import asyncio
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
    unique: np.ndarray  # sorted nodes compared and found to have no twin (size or contents)


class _Live:
    """Per scan: candidate nodes still undecided and skipped (changes as groups finish)."""

    __slots__ = ("key", "pending", "skipped", "t", "unique")

    def __init__(
        self, key: tuple[int, bool], pending: np.ndarray, skipped: np.ndarray, unique: np.ndarray
    ) -> None:
        self.key = key
        self.pending = pending
        self.skipped = skipped
        self.unique = unique
        self.t = time.monotonic()


LIVE_EVERY_S = 0.5  # while a job runs, the pending / skipped nodes are rebuilt at most this often


class Results:
    def __init__(self, scans: Sequence[ScanInput], keep: KeepOptions | None = None) -> None:
        self.scans = list(scans)
        self.keep = keep or KeepOptions()
        self.m = np.zeros(1024, C.MEMBER_DTYPE)
        self.bt = np.zeros(1024, np.int64)  # birth time of each member (ns, 0 unknown)
        self.n = 0
        self.paths: list[str] = []
        self.groups: list[Group] = []
        # per group, kept in step with the Group objects (vectorised status, ledger, frames)
        self._gvalid = np.zeros(256, bool)
        self._gshared = np.zeros(256, bool)
        self._greclaim = np.zeros(256, np.int64)
        self._vol_groups = np.zeros(256, np.int64)  # valid groups with a copy on each volume
        self._by_cand_group: dict[int, list[int]] = {}
        self.version = 0
        self.lock = threading.RLock()
        self._index: dict[int, _ScanIndex] = {}
        self._live: dict[int, _Live] = {}
        self._sorted: dict[tuple[Any, ...], tuple[int, list[Group]]] = {}
        self.job: Job | None = None
        self.vol_names: list[str] = []
        self.vol_notes: dict[int, list[str]] = {}
        self.finished_at = 0.0
        self.started_at = time.time()

    # ------------------------------------------------------------------ sink
    def start(self, job: Job) -> None:
        self.job = job

    def _finish_info(self, job: Job) -> None:
        with self.lock:
            self.vol_names = [v.name for v in job.vols]
            self.vol_notes = {v.index: list(v.notes) for v in job.vols if v.notes}
            self.finished_at = time.time()

    def finish(self, job: Job) -> None:
        self._finish_info(job)
        self.apply_keep()  # rules across groups (folder-consistent) need them all

    async def finish_async(self, job: Job) -> None:
        """:meth:`finish` with the keep rules in a thread (the engine calls this one)."""
        self._finish_info(job)
        await self.set_keep_async(self.keep)

    def _bump(self) -> None:
        self.version += 1
        self._index.clear()
        self._sorted.clear()

    def _sync_group(self, g: Group) -> None:
        i = g.id
        if i >= len(self._gvalid):
            cap = max(len(self._gvalid) * 2, i + 1)
            for name in ("_gvalid", "_gshared", "_greclaim"):
                old = getattr(self, name)
                new = np.zeros(cap, old.dtype)
                new[: len(old)] = old
                setattr(self, name, new)
        self._gvalid[i] = g.valid
        self._gshared[i] = g.shared
        self._greclaim[i] = g.reclaim

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
                cap = max(len(self.m) * 2, self.n + k)
                grown = np.zeros(cap, C.MEMBER_DTYPE)
                grown[: self.n] = self.m[: self.n]
                self.m = grown
                bt = np.zeros(cap, np.int64)
                bt[: self.n] = self.bt[: self.n]
                self.bt = bt
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
                self.bt[row] = int(mem.get("btime", 0))
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
            self._vol_groups[list(vols)] += 1
            self._by_cand_group.setdefault(g, []).append(grp.id)
            self._apply_one(grp)
            self._bump()

    def invalidate_rows(self, job: Job, cand_rows: Sequence[int]) -> None:
        sel = job.sel
        if sel is None or not len(cand_rows):
            return
        cr = np.asarray(cand_rows, dtype=np.int64)
        want = (sel.cand["scan"][cr].astype(np.int64) << 32) | sel.cand["node"][cr].astype(np.int64)
        with self.lock:
            m = self.m[: self.n]
            have = (m["scan"].astype(np.int64) << 32) | m["node"].astype(np.int64)
            self.invalidate(np.flatnonzero(np.isin(have, want)).tolist())

    # ------------------------------------------------------------------ keep rules
    def set_keep(self, keep: KeepOptions) -> None:
        with self.lock:
            self.keep = keep
            self.apply_keep()

    def apply_keep(self) -> None:
        with self.lock:
            side = self._folder_sides(self.keep, self.groups, self.m)
            for g in self.groups:
                self._apply_one(g, side)
            self._bump()

    async def set_keep_async(self, keep: KeepOptions) -> None:
        """The keep rules over every group in a thread, on a snapshot of the roles; committed when
        no group came or changed meanwhile (else computed again)."""
        for _ in range(3):
            with self.lock:
                v0, n, m = self.version, self.n, self.m
                groups = list(self.groups)
                roles = m["role"][:n].copy()
            if len(groups) < 64:  # not worth a thread
                break
            done = await asyncio.to_thread(self._keep_pass, keep, groups, m, roles)
            with self.lock:
                if self.version != v0 or self.n != n or self.m is not m:
                    continue
                self.m["role"][:n] = roles
                for g, (reclaim, shared, keep_row, keep_why) in zip(groups, done, strict=True):
                    g.reclaim, g.shared_bytes, g.keep_row, g.keep_why = reclaim, shared, keep_row, keep_why
                    self._sync_group(g)
                self.keep = keep
                self._bump()
                return
        self.set_keep(keep)

    def _keep_pass(
        self, keep: KeepOptions, groups: list[Group], m: np.ndarray, roles: np.ndarray
    ) -> list[tuple[int, int, int, int]]:
        side = self._folder_sides(keep, groups, m)
        return [self._roles_of(g, keep, side, m, roles) for g in groups]

    def _units(self, g: Group, m: np.ndarray | None = None) -> list[UnitView]:
        m = self.m if m is None else m
        units: dict[int, UnitView] = {}
        named: dict[int, int] = {}  # rows of the unit that are names of their own (not views)
        for row in range(g.start, g.start + g.count):
            rep = int(m["unit"][row])
            u = units.get(rep)
            fl = int(m["flags"][row])
            if u is None:
                u = units[rep] = UnitView(
                    rows=[],
                    path=self.paths[rep],
                    vol=int(m["vol"][rep]),
                    btime=int(self.bt[rep]),
                    mtime=int(m["mtime"][rep]),
                    forced=False,
                )
            u.rows.append(row)
            if fl & C.F_VIEW:
                u.alt.append(self.paths[row])
            else:
                named[rep] = named.get(rep, 0) + 1
            if fl & C.F_NEEDED:
                u.forced, u.forced_why = True, C.WHY_NEEDED
            elif fl & C.F_PINNED and not u.forced:
                u.forced, u.forced_why = True, C.WHY_PINNED
            elif fl & C.F_NETWORK and not u.forced:
                u.forced, u.forced_why = True, C.WHY_NEEDED
        for rep, u in units.items():
            if named.get(rep, 0) > 1 and not u.forced:
                # several names (hard links, clones): only ever kept (phase 1)
                u.forced, u.forced_why = True, C.WHY_PINNED
        return list(units.values())

    def _apply_one(self, g: Group, side: dict[str, int] | None = None) -> None:
        g.reclaim, g.shared_bytes, g.keep_row, g.keep_why = self._roles_of(
            g, self.keep, side, self.m, self.m["role"]
        )
        self._sync_group(g)

    def _roles_of(
        self,
        g: Group,
        keep: KeepOptions,
        side: dict[str, int] | None,
        m: np.ndarray,
        roles: np.ndarray,
    ) -> tuple[int, int, int, int]:
        """Writes the role of every row of ``g`` into ``roles``; (reclaim, shared bytes, keeper
        row, why) of the group."""
        units = self._units(g, m)
        if g.shared or not g.valid:
            shared = 0
            for u in units:
                for k, row in enumerate(u.rows):
                    base = int(roles[row]) & 0x0F
                    if int(m["flags"][row]) & C.F_VIEW:
                        roles[row] = C.ROLE_KEEP if not g.shared else base or C.ROLE_ALIAS
                        continue
                    if base not in (C.ROLE_ALIAS, C.ROLE_CLONE):
                        base = C.ROLE_CLONE if g.shared and row != u.rows[0] else C.ROLE_KEEP
                    roles[row] = base
                    if k > 0:
                        shared += int(m["alloc"][row])
            return 0, shared, -1, 0
        chosen = choose(units, keep, digest=g.digest, side=side)
        reclaim = 0
        shared = 0
        keep_row, keep_why = -1, 0
        for u, (role, why) in zip(units, chosen, strict=True):
            for k, row in enumerate(u.rows):
                fl = int(m["flags"][row])
                if k > 0 and fl & C.F_VIEW:
                    roles[row] = (role & 0x0F) | ((why & 0x0F) << 4)  # the same file: its role
                    continue
                base = int(roles[row]) & 0x0F
                if k > 0 and base in (C.ROLE_ALIAS, C.ROLE_CLONE, 0):
                    roles[row] = base or C.ROLE_ALIAS
                    shared += int(m["alloc"][row])
                    continue
                r = role
                if u.forced and fl & C.F_NEEDED:
                    r = C.ROLE_NEEDED if role == C.ROLE_KEEP and why == C.WHY_NEEDED else role
                roles[row] = (r & 0x0F) | ((why & 0x0F) << 4)
                if role == C.ROLE_EXTRA:
                    reclaim += int(m["alloc"][row])
                elif keep_row < 0:
                    keep_row, keep_why = row, why
        return reclaim, shared, keep_row, keep_why

    def _folder_sides(self, keep: KeepOptions, groups: list[Group], m: np.ndarray) -> dict[str, int] | None:
        if not keep.folder_consistent:
            return None
        pairs: dict[tuple[str, str], int] = {}
        info: dict[str, tuple[int, int]] = {}
        for g in groups:
            if g.shared or not g.valid or g.units != 2:
                continue
            units = self._units(g, m)
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
        winners = folder_sides(pairs, info, keep)
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

    def is_view(self, row: int) -> bool:
        return bool(int(self.m["flags"][row]) & C.F_VIEW)

    def valid_groups(self) -> list[Group]:
        return [g for g in self.groups if g.valid]

    def _valid_rows(self) -> np.ndarray:
        """Bool per member row: its group is still valid."""
        m = self.m[: self.n]
        return self._gvalid[: len(self.groups)][m["group"]] if self.n else np.zeros(0, bool)

    def ledger(self) -> dict[int, Ledger]:
        out: dict[int, Ledger] = {}
        with self.lock:
            if not self.n:
                return out
            m = self.m[: self.n]
            valid = self._valid_rows()
            role = m["role"] & 0x0F
            vol = m["vol"].astype(np.int64)
            own = (m["flags"] & C.F_VIEW) == 0
            nv = int(vol.max()) + 1
            extra = valid & own & (role == C.ROLE_EXTRA)
            shared = valid & own & ((role == C.ROLE_ALIAS) | (role == C.ROLE_CLONE))
            alloc = m["alloc"].astype(np.float64)
            ex_n = np.bincount(vol[extra], minlength=nv)
            ex_b = np.rint(np.bincount(vol[extra], weights=alloc[extra], minlength=nv)).astype(np.int64)
            sh_b = np.rint(np.bincount(vol[shared], weights=alloc[shared], minlength=nv)).astype(np.int64)
            g_n = self._vol_groups
            for v in np.flatnonzero(g_n[:nv] > 0).tolist():
                out[v] = Ledger(
                    extra=int(ex_n[v]), reclaim=int(ex_b[v]), shared=int(sh_b[v]), groups=int(g_n[v])
                )
        return out

    def totals(self) -> tuple[int, int, int]:
        """(groups with something to free, extra files, bytes freed)."""
        with self.lock:
            ng = len(self.groups)
            mask = self._gvalid[:ng] & ~self._gshared[:ng] & (self._greclaim[:ng] > 0)
            extra = 0
            if self.n:
                m = self.m[: self.n]
                extra = int(
                    np.count_nonzero(
                        self._valid_rows()
                        & ((m["role"] & 0x0F) == C.ROLE_EXTRA)
                        & ((m["flags"] & C.F_VIEW) == 0)
                    )
                )
            return int(mask.sum()), extra, int(self._greclaim[:ng][mask].sum())

    def stale(self) -> int:
        """Verified groups a change took apart (back to 「待核对」 until the next search)."""
        with self.lock:
            return int(np.count_nonzero(~self._gvalid[: len(self.groups)]))

    # ------------------------------------------------------------------ per scan index
    def scan_index(self, si: int) -> _ScanIndex:
        with self.lock:
            live = self._live_of(si)
            hit = self._index.get(si)
            if hit is not None and hit.version == self.version:
                hit.pending, hit.skipped, hit.unique = live.pending, live.skipped, live.unique
                return hit
            m = self.m[: self.n]
            valid = self._valid_rows()
            mine = np.flatnonzero((m["scan"] == si) & valid)
            order = np.argsort(m["node"][mine], kind="stable")
            rows = mine[order]
            nodes = m["node"][rows].astype(np.int64)
            folders, fbytes = self._rollup(si, m, valid)
            idx = _ScanIndex(
                self.version,
                nodes,
                rows.astype(np.int64),
                folders,
                fbytes,
                live.pending,
                live.skipped,
                live.unique,
            )
            self._index[si] = idx
            return idx

    def _live_of(self, si: int) -> _Live:
        job = self.job
        final = job is None or job.state in ("done", "cancelled", "failed")
        key = (job.done_groups if job is not None else 0, final)
        hit = self._live.get(si)
        if hit is not None and (
            hit.key == key or (not final and not hit.key[1] and time.monotonic() - hit.t < LIVE_EVERY_S)
        ):
            return hit
        pending, skipped, unique = self._pending(si)
        live = self._live[si] = _Live(key, pending, skipped, unique)
        return live

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

    def _pending(self, si: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        job = self.job
        empty = np.zeros(0, np.int64)
        if job is None or job.sel is None:
            return empty, empty, empty
        lone = job.sel.unique_nodes.get(si)
        lone = lone.astype(np.int64) if lone is not None else empty
        if not len(job.sel.cand):
            return empty, empty, lone
        cand = job.sel.cand
        mine = cand["scan"] == si
        st = cand["state"]
        if job.state in ("done", "cancelled", "failed"):
            pend = np.zeros(len(cand), bool)
        else:
            pend = mine & np.isin(st, (C.ST_SELECTED, C.ST_PROBED, C.ST_VERIFYING))
        skip = mine & (st == C.ST_SKIPPED)
        uniq = np.concatenate([cand["node"][mine & (st == C.ST_UNIQUE)].astype(np.int64), lone])
        return (
            np.sort(cand["node"][pend].astype(np.int64)),
            np.sort(cand["node"][skip].astype(np.int64)),
            np.sort(uniq),
        )

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

    def is_skipped(self, si: int, node: int) -> bool:
        idx = self.scan_index(si)
        p = int(np.searchsorted(idx.skipped, node))
        return p < len(idx.skipped) and int(idx.skipped[p]) == node

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
            gshared = self._gshared[: len(self.groups)]
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
                shared_g = (
                    np.where(hit, gshared[np.maximum(gid - 1, 0)], False) if len(gshared) else hit & False
                )
                state[shared_g & hit] = C.DUP_SHARED
                rest = ~hit
                if rest.any():
                    for arr, code in (
                        (idx.pending, C.DUP_PENDING),
                        (idx.skipped, C.DUP_SKIPPED),
                        (idx.unique, C.DUP_UNIQUE),
                    ):
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
                for arr, code in (
                    (idx.pending, C.DUP_PENDING),
                    (idx.skipped, C.DUP_SKIPPED),
                    (idx.unique, C.DUP_UNIQUE),
                ):
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
            self._bump()

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
            fresh = 0
            for gid in touched:
                g = self.groups[gid]
                if g.valid:
                    fresh += 1
                    self._vol_groups[list(g.volumes)] -= 1
                g.valid = False  # back to 「待核对」: the next run decides
                self._apply_one(g)
            self._bump()
            return fresh

    # ------------------------------------------------------------------ coverage
    def coverage(self, si: int, folder: int, *, limit: int = 50) -> Coverage:
        """What of ``folder``'s files (at least the job's minimum size) exists elsewhere. Runs in a
        thread: the tables are copied under the lock, the work is done outside it."""
        from .select import _below

        out = Coverage()
        store = self.scans[si].store
        cols = store.cols
        tops = np.array([folder], dtype=np.int64)
        with self.lock:
            job = self.job
            n = self.n
            m = self.m[:n].copy()
            ng = len(self.groups)
            gvalid = self._gvalid[:ng].copy()
            gshared = self._gshared[:ng].copy()
            sel = job.sel if job is not None else None
            cand = sel.cand[["scan", "node", "state"]].copy() if sel is not None else None
            uniq = sel.unique_nodes.get(si) if sel is not None else None
        only: list[np.ndarray] = []
        counted = 0
        if uniq is not None and len(uniq):
            inside_u = uniq[_below(store, uniq, tops)]
            counted += len(inside_u)
            only.append(inside_u.astype(np.int64))
        if cand is not None and len(cand):
            mine = np.flatnonzero(cand["scan"] == si)
            nodes = cand["node"][mine].astype(np.int64)
            hit = _below(store, nodes, tops)
            cn = nodes[hit]
            cst = cand["state"][mine[hit]]
            counted += len(cn)
            valid_row = gvalid[m["group"]] if n else np.zeros(0, bool)
            mrows = np.flatnonzero((m["scan"] == si) & valid_row) if n else np.zeros(0, np.int64)
            order = np.argsort(m["node"][mrows], kind="stable")
            mrows = mrows[order]
            mnodes = m["node"][mrows].astype(np.int64)
            rows_in = np.full(len(cn), -1, np.int64)
            if len(mnodes) and len(cn):
                p = np.minimum(np.searchsorted(mnodes, cn), len(mnodes) - 1)
                rows_in = np.where(mnodes[p] == cn, mrows[p], -1)
            mem = rows_in >= 0
            if mem.any():
                inside_all = np.zeros(n, bool)
                rs = np.flatnonzero(m["scan"] == si)
                inside_all[rs] = _below(store, m["node"][rs].astype(np.int64), tops)
                outside = valid_row & ~inside_all
                og = m["group"][outside].astype(np.int64)
                ou = m["unit"][outside].astype(np.int64)
                per_group = np.bincount(og, minlength=ng)
                upair, ucount = np.unique(og * (n + 1) + ou, return_counts=True)
                r = rows_in[mem]
                g = m["group"][r].astype(np.int64)
                key = g * (n + 1) + m["unit"][r].astype(np.int64)
                same_u = np.zeros(len(r), np.int64)
                if len(upair):
                    q = np.minimum(np.searchsorted(upair, key), len(upair) - 1)
                    same_u = np.where(upair[q] == key, ucount[q], 0)
                tot = per_group[g]
                sh = gshared[g]
                other = ~sh & (tot - same_u > 0)
                same = ~other & ((same_u > 0) | (sh & (tot > 0)))
                b = cols["alloc"][cn[mem]].astype(np.int64)
                out.elsewhere, out.elsewhere_bytes = int(other.sum()), int(b[other].sum())
                out.same_data, out.same_data_bytes = int(same.sum()), int(b[same].sum())
                only.append(cn[mem][~other & ~same])
                if other.any():
                    out.elsewhere_list = self._elsewhere(m, outside, r[other], b[other])
            rest = ~mem
            uniq_st = cst[rest] == C.ST_UNIQUE
            only.append(cn[rest][uniq_st])
            unchecked = cn[rest][~uniq_st]
            out.unchecked = len(unchecked)
            out.unchecked_bytes = int(cols["alloc"][unchecked].astype(np.int64).sum())
        only_nodes = np.concatenate(only) if only else np.zeros(0, np.int64)
        only_alloc = cols["alloc"][only_nodes].astype(np.int64)
        out.only_here = len(only_nodes)
        out.only_here_bytes = int(only_alloc.sum())
        biggest = only_nodes[np.argsort(-only_alloc, kind="stable")[:limit]]
        out.only_here_list = [(int(nd), store.path(int(nd)), int(cols["alloc"][nd])) for nd in biggest]
        out.small = max(0, int(cols["tot_files"][folder]) - counted)
        seen = out.only_here_bytes + out.elsewhere_bytes + out.same_data_bytes + out.unchecked_bytes
        out.small_bytes = max(0, int(cols["tot_alloc"][folder]) - seen)
        return out

    def _elsewhere(
        self, m: np.ndarray, outside: np.ndarray, rows: np.ndarray, sizes: np.ndarray, limit: int = 5
    ) -> list[tuple[int, str, int, int]]:
        """The folders holding the other copies of ``rows`` (a folder's files with a copy elsewhere),
        by the bytes of those files: (scan, folder path, bytes, node)."""
        out_rows = np.flatnonzero(outside)
        order = np.argsort(m["group"][out_rows], kind="stable")
        out_rows = out_rows[order]
        og = m["group"][out_rows]
        unit = m["unit"]
        paths = self.paths
        by: dict[tuple[int, str], int] = {}
        for r, size in zip(rows.tolist(), sizes.tolist(), strict=True):
            g = int(m["group"][r])
            lo, hi = int(np.searchsorted(og, g, "left")), int(np.searchsorted(og, g, "right"))
            seen: set[tuple[int, str]] = set()
            for o in out_rows[lo:hi].tolist():
                if int(unit[o]) == int(unit[r]):
                    continue
                key = (int(m["scan"][o]), os.path.dirname(paths[o]))
                if key not in seen:
                    seen.add(key)
                    by[key] = by.get(key, 0) + int(size)
        top = sorted(by.items(), key=lambda kv: (-kv[1], kv[0][1]))[:limit]
        found: list[tuple[int, str, int, int]] = []
        for (si, path), nbytes in top:
            node = self.scans[si].store.find(path)
            found.append((si, path, nbytes, int(node) if node is not None else 0))
        return found

    # ------------------------------------------------------------------ pages
    def sorted_groups(
        self,
        sort: str = "reclaim",
        *,
        cross_only: bool = False,
        include_shared: bool = False,
        scan: int | None = None,
        under: tuple[int, int] | None = None,
        query: str = "",
    ) -> list[Group]:
        from .select import _below

        q = query.strip().lower()
        key_c = (sort, cross_only, include_shared, scan, under, q)
        with self.lock:
            hit = self._sorted.get(key_c)
            if hit is not None and hit[0] == self.version:
                return hit[1]
            version = self.version
            gs = [g for g in self.groups if g.valid and (include_shared or not g.shared)]
            m = self.m[: self.n]
            if cross_only:
                gs = [g for g in gs if len(g.volumes) > 1]
            if scan is not None:
                ids = set(np.unique(m["group"][m["scan"] == scan]).tolist())
                gs = [g for g in gs if g.id in ids]
            if under is not None:
                si, node = under
                mine = np.flatnonzero(m["scan"] == si)
                inside = mine[
                    _below(self.scans[si].store, m["node"][mine].astype(np.int64), np.array([node]))
                ]
                ids = set(np.unique(m["group"][inside]).tolist())
                gs = [g for g in gs if g.id in ids]
            if q:
                paths = self.paths
                gs = [g for g in gs if any(q in paths[r].lower() for r in range(g.start, g.start + g.count))]
            key = {
                "reclaim": lambda g: (-g.reclaim, -g.size, g.id),
                "size": lambda g: (-g.size, g.id),
                "copies": lambda g: (-g.count, -g.size, g.id),
                "path": lambda g: (self.paths[g.start].lower(), g.id),
            }.get(sort)
            if key is None:
                key = lambda g: (-g.reclaim, g.id)  # noqa: E731
            out = sorted(gs, key=key)
            if len(self._sorted) >= 16:
                self._sorted.clear()
            self._sorted[key_c] = (version, out)
            return out

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
            "ctime": int(self.bt[row]) // 1_000_000_000,
            "alloc": int(m["alloc"]),
            "role": C.ROLE_NAMES.get(role, "keep"),
            "why": C.WHY_NAMES.get(self.why(row)) if role in (C.ROLE_KEEP, C.ROLE_NEEDED) else None,
            "view": self.is_view(row),
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
    elsewhere_list: list[tuple[int, str, int, int]] = field(default_factory=list)  # (scan, path, bytes, node)

"""The cleanup plan of one search (docs/dupes.md §16.3–16.7): decisions, edits, risks, approval.

Built when the user opens it, from the search's :class:`~spaceknife.dupes.results.Results`; one row
per member row there. Only verified groups (whole contents read, BLAKE3-256 equal) take part (I12):
changed, shared, excluded groups are listed as 「不在方案里」 with the reason.

Every edit is checked by the backend before it is kept (the last keeper of a group, the volume
floor, locked copies), bumps :attr:`version` and clears an approval (I5); 200 steps of undo. Risks
are evaluated over arrays after every edit; high ones must be acknowledged per kind, for the copies
then shown (a fingerprint of their keys), before an approval. An approval freezes the steps in the
order they run and a token over them, the acknowledgements and the options.

Threads: the plan is used under :attr:`lock`; the heavy calls run in the compute pool.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import threading
import time
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

import numpy as np

from .. import codes as C
from ..keep import KeepOptions
from . import codes as K
from . import risks as RK
from .caps import CopyFacts, VolCaps, availability
from .facts import Probe, SystemFacts, TagIndex, name_tags
from .keep import Decision, GroupInputs, Unit, decide, keep_floor
from .policy import RULE_TO_CRITERION, ManualCopy, ManualGroup, PairRule, Policy, Rule, Selector

if TYPE_CHECKING:
    from ..results import Group, Results

G_IN, G_STALE, G_SHARED, G_EXCLUDED, G_NO_EXTRA, G_CLEANED = 0, 1, 2, 3, 4, 5
G_NAMES = {
    G_IN: "in",
    G_STALE: "stale",
    G_SHARED: "shared",
    G_EXCLUDED: "excluded",
    G_NO_EXTRA: "no_extra",
    G_CLEANED: "cleaned",
}
F_CLEANED = C.F_CLEANED
UNDO_STEPS = 200
SEP = "\\" if sys.platform == "win32" else "/"


class Refused(Exception):
    """An edit the backend does not accept (the message is for the user)."""


def _hex(d: bytes | None) -> str:
    return (d or b"").hex()


def copy_key(digest: str, path: str) -> int:
    h = hashlib.blake2b(
        (digest + "\0" + os.path.normcase(path)).encode("utf-8", "surrogatepass"), digest_size=8
    )
    return int.from_bytes(h.digest(), "little")


@dataclass(slots=True)
class Step:
    """One frozen action of an approval (docs/dupes.md §16.7.6)."""

    seq: int
    group: int
    digest: str
    action: str
    path: str
    vol: int
    dev: int
    fid_lo: int
    fid_hi: int
    size: int
    mtime: int
    change: int
    usn: int
    parent: str
    keepers: list[dict[str, Any]]  # every keeper of the group that must stay intact (path, stamps)
    target: dict[str, Any] | None  # the keeper a link or clone points at
    text: str = ""  # a symbolic link's text
    risks: list[str] = field(default_factory=list)
    alloc: int = 0
    scan: int = 0
    node: int = 0
    verify_content: bool = False  # verify=auto decided this pair must be read again
    row: int = -1  # the copy's member row in the results
    parent_id: list[int] | None = None  # the folder's identity when approved (R1 checks it)

    def to_json(self) -> dict[str, Any]:
        return {k: getattr(self, k) for k in self.__slots__}


@dataclass(slots=True)
class Approval:
    version: int
    token: str
    steps: list[Step]
    acks: dict[str, int]  # risk id -> copies acknowledged
    options: dict[str, str]
    at: float = field(default_factory=time.time)


@dataclass(slots=True)
class Ack:
    keys: frozenset[int]
    count: int
    at: float


class CleanupPlan:
    def __init__(
        self,
        plan_id: str,
        results: Results,
        scan_ids: list[str],
        policy: Policy,
        *,
        vols: list[VolCaps],
        sys_facts: SystemFacts,
        tags: TagIndex,
        in_basket: Callable[[str], bool] | None = None,
        recovery_in_place: Callable[[int], bool] | None = None,
        exact: bool = False,
        no_trash_recovery: bool = True,
    ) -> None:
        self.id = plan_id
        self.res = results
        self.scan_ids = scan_ids
        self.policy = policy
        self.vols = vols
        self.sysf = sys_facts
        self.tagx = tags
        self.in_basket = in_basket
        self.recovery_in_place = recovery_in_place or (lambda v: False)
        self.exact = exact
        self.no_trash_recovery = no_trash_recovery
        self.lock = threading.RLock()
        self.version = 1
        self.undo_stack: list[Policy] = []
        self.redo_stack: list[Policy] = []
        self.acks: dict[str, Ack] = {}
        self.approval: Approval | None = None
        self.probes: dict[int, Probe] = {}
        self.results_version = -1
        self.closed = False
        self.created = time.time()
        self.review_marks: dict[int, int] = {}
        self._freed_cache: tuple[int, np.ndarray, np.ndarray] | None = None
        self._changed_cache: tuple[int, np.ndarray] | None = None
        self._build()

    # ------------------------------------------------------------------ building
    def _build(self) -> None:
        res = self.res
        with res.lock:
            self.n = n = res.n
            m = res.m[:n].copy()
            self.m = m
            self.paths = list(res.paths[:n])
            self.bt = res.bt[:n].copy()
            self.groups: list[Group] = list(res.groups)
            self.results_version = res.version
        ng = len(self.groups)
        self.gstate = np.zeros(ng, np.uint8)
        self.gunits: list[list[Unit]] = [[] for _ in range(ng)]
        self.dirs: list[str] = []
        dir_index: dict[str, int] = {}
        self.dir_of = np.zeros(n, np.int32)
        self.names: list[str] = [""] * n
        for r, p in enumerate(self.paths):
            d, nm = os.path.split(p)
            j = dir_index.get(d)
            if j is None:
                j = dir_index[d] = len(self.dirs)
                self.dirs.append(d)
            self.dir_of[r] = j
            self.names[r] = nm
        self.ext = [os.path.splitext(nm)[1].lower() for nm in self.names]
        self.tags = np.zeros(n, np.uint64)
        self._cheap_tags()
        self.vol = m["vol"].astype(np.int64)
        self.scan = m["scan"].astype(np.int64)
        self.alloc = m["alloc"].astype(np.int64)
        self.gsize = np.array([g.size for g in self.groups], np.int64) if ng else np.zeros(0, np.int64)
        self.gcross = np.array([len(g.volumes) > 1 for g in self.groups], bool) if ng else np.zeros(0, bool)
        self.group_of = m["group"].astype(np.int64)
        for g in self.groups:
            self.gunits[g.id] = self._units_of(g)
        self.keys = np.array(
            [copy_key(_hex(self.groups[int(self.group_of[r])].digest), self.paths[r]) for r in range(n)],
            np.uint64,
        )
        # decisions (per member row; the representative of each unit carries them)
        self.keep = np.zeros(n, np.uint8)
        self.ksrc = np.zeros(n, np.uint8)
        self.kref = np.zeros(n, np.int32)
        self.act = np.zeros(n, np.uint8)
        self.asrc = np.zeros(n, np.uint8)
        self.aref = np.zeros(n, np.int32)
        self.want = np.zeros(n, np.uint8)
        self.wna = np.zeros(n, np.uint8)
        self.target = np.full(n, -1, np.int64)
        self.refk = np.full(n, -1, np.int64)  # the keeper a copy is compared with
        self.na = np.zeros((n, 5), np.uint8)
        self.tgt = np.full((n, 5), -1, np.int64)
        self.sev = np.zeros(n, np.uint8)
        self.diff = np.zeros(n, np.uint32)
        self.is_rep = np.zeros(n, bool)
        self.gconflict = np.zeros(ng, bool)
        self.gonly_avoided = np.zeros(ng, bool)
        self.gsafety = np.zeros(ng, bool)
        self.gsev = np.zeros(ng, np.uint8)
        self.risk_rows = np.zeros(0, np.int64)
        self.risk_ids = np.zeros(0, np.int16)
        self.risk_sev = np.zeros(0, np.uint8)
        for units in self.gunits:
            for u in units:
                self.is_rep[u.rep] = True
        self._side: dict[str, int] | None = None
        self.recompute()

    def _cheap_tags(self) -> None:
        res = self.res
        n = self.n
        m = self.m
        now = time.time()
        try:
            from ...classes import active

            table = active()
        except Exception:
            table = None
        cls_of: dict[str, list[str]] = {}
        stores = [s.store for s in res.scans]
        for r in range(n):
            si = int(m["scan"][r])
            d = self.dirs[int(self.dir_of[r])]
            f = self.tagx.dir_facts(si, d)
            t = f.tags
            ext = self.ext[r]
            cls = cls_of.get(ext)
            if cls is None:
                cls = cls_of[ext] = list(table.classes_of(ext.lstrip("."))) if table is not None else []
            t |= name_tags(self.names[r], cls)
            node = int(m["node"][r])
            store = stores[si]
            fl = 0
            if 0 < node < len(store):
                fl = int(store.cols["flags"][node])
            from ...store.nodes import Flag

            if fl & Flag.HIDDEN:
                t |= K.T_HIDDEN
            if fl & Flag.SYSTEM:
                t |= K.T_SYSTEM
            if fl & Flag.READONLY:
                t |= K.T_READONLY
            if fl & Flag.ENCRYPTED:
                t |= K.T_ENCRYPTED
            if fl & Flag.SPARSE:
                t |= K.T_SPARSE
            if fl & Flag.COMPRESSED:
                t |= K.T_COMPRESSED
            mt = int(m["mtime"][r]) / 1e9
            ch = int(m["change"][r]) / 1e9
            recent = max(mt, ch)
            if now - recent < 86400:
                t |= K.T_RECENT1
            elif now - recent < 7 * 86400:
                t |= K.T_RECENT7
            if int(m["flags"][r]) & C.F_NETWORK:
                t |= K.T_NETWORK
            if int(m["flags"][r]) & C.F_CACHE_HIT and sys.platform == "win32" and not int(m["usn"][r]):
                t |= K.T_CACHE_ONLY
            if sys.platform == "win32" and not int(m["usn"][r]):
                t |= K.T_NO_USN
            if int(m["flags"][r]) & C.F_VIEW:
                t |= K.T_VIEW
            v = int(m["vol"][r])
            if v < len(self.vols):
                vc = self.vols[v]
                if vc.removable:
                    t |= K.T_REMOVABLE
                if os.path.normcase(d.rstrip("\\/")) == os.path.normcase(vc.mount.rstrip("\\/")) and (
                    t & (K.T_HIDDEN | K.T_SYSTEM) or fl & Flag.TEMPORARY or ext == ".tmp"
                ):
                    t |= K.T_HIDDEN_TEMP_ROOT
            key = os.path.normcase(self.paths[r])
            if key in self.tagx.link_targets:
                t |= K.T_LINK_TARGET
            if self.in_basket is not None and self.in_basket(self.paths[r]):
                t |= K.T_IN_BASKET
            self.tags[r] = np.uint64(t)

    def _units_of(self, g: Group) -> list[Unit]:
        res = self.res
        out: list[Unit] = []
        with res.lock:
            views = res._units(g, self.m)
        for uv in views:
            rows = [r for r in uv.rows]
            named = [
                r
                for r in rows
                if not int(self.m["flags"][r]) & C.F_VIEW and not int(self.m["flags"][r]) & F_CLEANED
            ]
            if not named:
                continue
            rep = rows[0]
            forced = 0
            fl = int(self.m["flags"][rep])
            if fl & C.F_NEEDED:
                forced = K.SRC_FORCED_NEEDED
            elif len(named) > 1:
                forced = K.SRC_FORCED_ALIAS
            elif fl & C.F_NETWORK:
                forced = K.SRC_FORCED_NETWORK
            elif fl & C.F_PINNED:
                forced = K.SRC_FORCED_PINNED
            elif int(self.tags[rep]) & K.T_VCS_INTERNAL:
                forced = K.SRC_FORCED_VCS
            d = os.path.dirname(uv.path)
            out.append(
                Unit(
                    rep=rep,
                    rows=rows,
                    path=uv.path,
                    vol=uv.vol,
                    btime=uv.btime,
                    mtime=uv.mtime,
                    forced=forced,
                    alt=list(uv.alt),
                    dir=d,
                    name=os.path.basename(uv.path),
                    depth=uv.path.count(SEP),
                )
            )
        return out

    # ------------------------------------------------------------------ selectors
    def _match(self, sel: Selector, rows: np.ndarray | None = None) -> np.ndarray:
        """Per member row: the selector holds (risk conditions left out)."""
        n = self.n
        out = np.ones(n, bool)
        if sel.under or sel.not_under:
            from ..keep import _prefer_rank

            per_dir = np.ones(len(self.dirs), bool)
            if sel.under:
                per_dir &= np.array(
                    [_prefer_rank(d + SEP + "x", sel.under) < len(sel.under) for d in self.dirs], bool
                )
            if sel.not_under:
                per_dir &= np.array(
                    [_prefer_rank(d + SEP + "x", sel.not_under) >= len(sel.not_under) for d in self.dirs],
                    bool,
                )
            out &= per_dir[self.dir_of] if len(self.dirs) else out
        if sel.volumes:
            want = {v.lower().rstrip("\\/") for v in sel.volumes}
            vm = np.array(
                [
                    vc.name.lower().rstrip("\\/") in want or vc.mount.lower().rstrip("\\/") in want
                    for vc in self.vols
                ],
                bool,
            )
            out &= vm[self.vol] if len(vm) else False
        if sel.fs:
            want_fs = {f.lower() for f in sel.fs}
            fm = np.array([vc.fs in want_fs for vc in self.vols], bool)
            out &= fm[self.vol] if len(fm) else False
        if sel.ext:
            want_e = {("." + e.lower().lstrip(".")) for e in sel.ext}
            out &= np.fromiter((e in want_e for e in self.ext), bool, n)
        if sel.classes:
            try:
                from ...classes import active

                table = active()
                wc = set(sel.classes)
                out &= np.fromiter(
                    (bool(set(table.classes_of(e.lstrip("."))) & wc) for e in self.ext), bool, n
                )
            except Exception:
                out &= False
        if sel.name_glob:
            import fnmatch

            pat = sel.name_glob.lower()
            out &= np.fromiter((fnmatch.fnmatchcase(nm.lower(), pat) for nm in self.names), bool, n)
        if sel.path_glob:
            import fnmatch

            pat = sel.path_glob.lower().replace("\\", "/")
            out &= np.fromiter(
                (fnmatch.fnmatchcase(p.lower().replace("\\", "/"), pat) for p in self.paths), bool, n
            )
        g = self.group_of
        if sel.size_min is not None:
            out &= self.gsize[g] >= sel.size_min
        if sel.size_max is not None:
            out &= self.gsize[g] <= sel.size_max
        if sel.copies_min is not None:
            units = np.array([len(u) for u in self.gunits], np.int64)
            out &= units[g] >= sel.copies_min
        if sel.cross is not None:
            out &= self.gcross[g] == sel.cross
        if sel.same_folder is not None:
            same = np.array([len({u.dir for u in us}) == 1 for us in self.gunits], bool)
            out &= same[g] == sel.same_folder
        if sel.mtime_before is not None:
            out &= self.m["mtime"] < sel.mtime_before * 1_000_000_000
        if sel.mtime_after is not None:
            out &= self.m["mtime"] >= sel.mtime_after * 1_000_000_000
        if sel.tags:
            bits = 0
            for tname in sel.tags:
                bits |= K.TAG_CODE.get(tname, 0)
            out &= (self.tags & np.uint64(bits)) != 0
        if sel.not_tags:
            bits = 0
            for tname in sel.not_tags:
                bits |= K.TAG_CODE.get(tname, 0)
            out &= (self.tags & np.uint64(bits)) == 0
        if sel.can:
            for a in sel.can:
                code = K.ACTION_CODE.get(a)
                if code:
                    out &= self.na[:, code - 1] == K.NA_OK
        if sel.same_volume is not None:
            out &= self._has_keeper_on_vol() == sel.same_volume
        return out

    def _has_keeper_on_vol(self) -> np.ndarray:
        out = np.zeros(self.n, bool)
        for us in self.gunits:
            kv = {u.vol for u in us if self.keep[u.rep]}
            for u in us:
                if u.vol in kv:
                    out[u.rep] = True
        return out

    # ------------------------------------------------------------------ decisions
    def _side_map(self) -> dict[str, int] | None:
        P = self.policy
        if not P.folder_consistent:
            return None
        crit = P.criteria[0] if P.criteria else "oldest_created"
        rule = {v: k for k, v in RULE_TO_CRITERION.items()}.get(crit, "oldest")
        prefer: list[str] = []
        for r in P.keep_rules:
            if r.enabled and r.effect == "prefer" and r.when.empty():
                prefer.extend(r.folders)
        opts = KeepOptions(rule=rule, prefer=prefer, folder_consistent=True)  # type: ignore[arg-type]
        with self.res.lock:
            return self.res._folder_sides(opts, self.groups, self.m)

    def _group_state(self, g: Group) -> int:
        if not g.valid:
            return G_STALE
        if g.shared:
            return G_SHARED
        units = self.gunits[g.id]
        if len(units) < 2:
            return (
                G_CLEANED
                if any(int(self.m["flags"][r]) & F_CLEANED for r in range(g.start, g.start + g.count))
                else G_NO_EXTRA
            )
        mg = self.policy.groups.get(_hex(g.digest))
        if mg is not None and mg.excluded:
            return G_EXCLUDED
        return G_IN

    def _inputs(self, g: Group, keep_masks: dict[int, np.ndarray]) -> GroupInputs:
        P = self.policy
        units = self.gunits[g.id]
        d = _hex(g.digest)
        manual: list[ManualCopy | None] = []
        for u in units:
            got = None
            for p in (u.path, *u.alt):
                got = P.manual.get((d, p))
                if got is not None:
                    break
            manual.append(got)
        mg = P.groups.get(d)
        masks = {rid: [bool(mk[u.rep]) for u in units] for rid, mk in keep_masks.items()}
        group_rules = {rid: any(v) for rid, v in masks.items()}
        for r in P.keep_rules:
            if r.when.empty():
                group_rules[r.id] = True
        pairs: list[tuple[int, PairRule, list[bool], list[bool]]] = []
        from ..keep import _prefer_rank

        for pi, pr in sorted(enumerate(P.pairs), key=lambda x: -(len(x[1].a) + len(x[1].b))):
            on_a = [_prefer_rank(u.path, [pr.a]) == 0 for u in units]
            on_b = [_prefer_rank(u.path, [pr.b]) == 0 for u in units]
            if any(on_a) and any(on_b):
                pairs.append((pi, pr, on_a, on_b))
        return GroupInputs(
            digest=d,
            manual=manual,
            keep_set=set(mg.keep_set) if mg is not None and mg.keep_set is not None else None,
            keep_masks=masks,
            group_rules=group_rules,
            pairs=pairs,
            side=self._side,
        )

    def recompute(self, groups: Iterable[int] | None = None) -> None:
        """Every decision again (or only those of ``groups``), then the risks over all rows."""
        with self.lock:
            P = self.policy
            if groups is None:
                self._side = self._side_map()
            keep_masks = {r.id: self._match(r.when) for r in P.keep_rules if r.enabled and not r.when.empty()}
            todo = list(range(len(self.groups))) if groups is None else sorted(set(groups))
            for gi in todo:
                g = self.groups[gi]
                self.gstate[gi] = self._group_state(g)
                self._decide_keep(g, keep_masks)
            self._decide_actions(todo)
            self._risks()
            self.version += 1
            self._freed_cache = None
            self._changed_cache = None

    def _decide_keep(self, g: Group, keep_masks: dict[int, np.ndarray]) -> None:
        units = self.gunits[g.id]
        for r in range(g.start, g.start + g.count):
            self.keep[r] = K.K_NO
            self.ksrc[r] = K.SRC_NONE
            self.act[r] = K.A_NONE
        st = int(self.gstate[g.id])
        if not units:
            return
        if st in (G_STALE, G_SHARED, G_NO_EXTRA, G_CLEANED):
            for u in units:
                for r in u.rows:
                    self.keep[r] = K.K_LOCKED
            return
        dec: Decision = decide(units, self.policy, self._inputs(g, keep_masks), exact=self.exact)
        self.gconflict[g.id] = dec.conflict
        self.gonly_avoided[g.id] = dec.only_avoided
        self.gsafety[g.id] = dec.safety
        for i, u in enumerate(units):
            for r in u.rows:
                self.keep[r] = dec.state[i]
                self.ksrc[r] = dec.src[i]
                self.kref[r] = dec.ref[i]

    def _facts(self, r: int) -> CopyFacts:
        p = self.probes.get(r)
        return CopyFacts(
            vol=int(self.vol[r]),
            tags=int(self.tags[r]),
            probe_ok=bool(p and p.ok),
            writable=bool(p.writable) if p and p.ok else True,
            uid=p.uid if p and p.ok else -1,
            immutable=bool(p and p.ok and p.bits & 4),
            nocow=bool(p and p.ok and p.bits & 8),
            nlink=p.nlink if p and p.ok else 0,
            dir_writable=p.dir_writable if p and p.ok else True,
        )

    def _decide_actions(self, todo: list[int]) -> None:
        P = self.policy
        planned: dict[int, int] = {}
        # availability first: action rules may ask what can be done (「能就地块克隆」)
        for gi in todo:
            g = self.groups[gi]
            st = int(self.gstate[gi])
            units = self.gunits[gi]
            kfacts = [(u.rep, self._facts(u.rep)) for u in units if self.keep[u.rep] != K.K_NO]
            for u in units:
                r = u.rep
                self.na[r, :] = K.NA_OK
                self.tgt[r, :] = -1
                if self.keep[r] != K.K_NO:
                    self.na[r, 1:] = K.NA_KEEPER if self.keep[r] == K.K_KEEP else K.NA_FORCED_KEEP
                    continue
                na, tgt = availability(
                    self._facts(r),
                    kfacts,
                    self.vols,
                    sys_facts=self.sysf,
                    stale=st != G_IN,
                    planned_links=planned,
                    no_trash_recovery=self.no_trash_recovery,
                )
                self.na[r, :] = na
                self.tgt[r, :] = tgt
        rule_masks: dict[int, np.ndarray] = {}
        for rule in P.action_rules:
            if rule.enabled:
                rule_masks[rule.id] = self._match(rule.when)
        for gi in todo:
            g = self.groups[gi]
            units = self.gunits[gi]
            d = _hex(g.digest)
            st = int(self.gstate[gi])
            keepers = [u for u in units if self.keep[u.rep] != K.K_NO]
            for u in units:
                r = u.rep
                if self.keep[r] != K.K_NO:
                    self.act[r] = K.A_NONE
                    self.target[r] = -1
                    self.refk[r] = -1
                    continue
                self.refk[r] = self._ref_keeper(u, keepers)
                if st != G_IN:
                    self._set_act(u, K.A_LEAVE, K.SRC_NONE, 0, K.A_LEAVE, 0)
                    continue
                man = None
                for p in (u.path, *u.alt):
                    man = P.manual.get((d, p))
                    if man is not None:
                        break
                want, src, ref, chain = 0, K.SRC_ACTION_PRESET, 0, []
                rule_obj: Rule | None = None
                if man is not None and man.action:
                    want, src = K.ACTION_CODE.get(man.action, K.A_LEAVE), K.SRC_ACTION_MANUAL
                else:
                    for pi, pr in enumerate(P.pairs):
                        if not pr.other_action:
                            continue
                        side_kept = pr.a if pr.keep == "a" else pr.b if pr.keep == "b" else ""
                        other = pr.b if pr.keep == "a" else pr.a if pr.keep == "b" else ""
                        if (
                            side_kept
                            and other
                            and _under(u.path, other)
                            and any(_under(k.path, side_kept) for k in keepers)
                        ):
                            want, src, ref = (
                                K.ACTION_CODE.get(pr.other_action, K.A_LEAVE),
                                K.SRC_ACTION_PAIR,
                                pi,
                            )
                            break
                    if not want:
                        for rule in P.action_rules:
                            if not rule.enabled or rule.when.risky:
                                continue
                            mk = rule_masks.get(rule.id)
                            if mk is not None and mk[r]:
                                want, src, ref, chain, rule_obj = (
                                    K.ACTION_CODE.get(rule.effect, K.A_LEAVE),
                                    K.SRC_ACTION_RULE,
                                    rule.id,
                                    list(rule.fallback),
                                    rule,
                                )
                                break
                    if not want:
                        want = K.ACTION_CODE.get(P.default_action, K.A_LEAVE)
                act = want
                if self.na[r, want - 1] != K.NA_OK:
                    act = K.A_LEAVE
                    for alt in chain:
                        c = K.ACTION_CODE.get(alt, K.A_LEAVE)
                        if self.na[r, c - 1] == K.NA_OK:
                            act = c
                            break
                wna = int(self.na[r, want - 1]) if act != want else 0
                self._set_act(u, act, src if act == want else K.SRC_ACTION_FALLBACK, ref, want, wna)
                self.target[r] = self._pick_target(u, act, keepers, man, rule_obj)
                if act == K.A_HARDLINK and self.target[r] >= 0:
                    planned[int(self.target[r])] = planned.get(int(self.target[r]), 0) + 1

    def _set_act(self, u: Unit, act: int, src: int, ref: int, want: int, wna: int) -> None:
        for r in u.rows:
            self.act[r] = act if r == u.rep else K.A_NONE
            self.asrc[r] = src
            self.aref[r] = ref
            self.want[r] = want
            self.wna[r] = wna

    def _ref_keeper(self, u: Unit, keepers: list[Unit]) -> int:
        if not keepers:
            return -1
        same = [k for k in keepers if k.vol == u.vol]
        pool = same or keepers
        from .fsops import common_parts

        parts = u.path.split(SEP)
        return max(pool, key=lambda k: (common_parts(parts, k.path.split(SEP)), -len(k.path))).rep

    def _eligible(self, u: Unit, act: int, keepers: list[Unit]) -> list[Unit]:
        if act == K.A_HARDLINK:
            return [k for k in keepers if k.vol == u.vol]
        if act == K.A_CLONE:
            fg = self.vols[u.vol].fs_group
            return [k for k in keepers if self.vols[k.vol].fs_group == fg and self.vols[k.vol].clone]
        if act == K.A_SYMLINK:
            return list(keepers)
        return []

    def _pick_target(
        self, u: Unit, act: int, keepers: list[Unit], man: ManualCopy | None, rule: Rule | None
    ) -> int:
        if act not in (K.A_HARDLINK, K.A_SYMLINK, K.A_CLONE):
            return -1
        elig = self._eligible(u, act, keepers)
        if not elig:
            return -1
        if man is not None and man.target:
            for k in elig:
                if os.path.normcase(k.path) == os.path.normcase(man.target):
                    return k.rep
        if rule is not None and rule.target_under:
            under = [
                k
                for k in elig
                if _under(k.path, rule.target_under)
                or self.vols[k.vol].name.lower() == rule.target_under.lower().rstrip("\\/")
            ]
            if under:
                elig = under
        from .fsops import common_parts

        parts = u.path.split(SEP)

        def score(k: Unit) -> tuple[int, int, int, int]:
            return (
                int(k.dir == u.dir),
                int(k.vol == u.vol),
                int(not self.vols[k.vol].removable),
                common_parts(parts, k.path.split(SEP)),
            )

        return max(elig, key=score).rep

    # ------------------------------------------------------------------ risks
    def _diff(self, r: int, k: int) -> int:
        d = 0
        if k < 0:
            return K.D_UNKNOWN
        if self.names[r].lower() != self.names[k].lower():
            d |= K.D_NAME
        if abs(int(self.m["mtime"][r]) - int(self.m["mtime"][k])) > 2_000_000_000:
            d |= K.D_MTIME
        if self.bt[r] and self.bt[k] and abs(int(self.bt[r]) - int(self.bt[k])) > 2_000_000_000:
            d |= K.D_BTIME
        if (int(self.tags[r]) ^ int(self.tags[k])) & (K.T_READONLY | K.T_HIDDEN):
            d |= K.D_ATTRS
        if (int(self.tags[r]) ^ int(self.tags[k])) & K.T_ENCRYPTED:
            d |= K.D_EFS
        a, b = self.probes.get(r), self.probes.get(k)
        if a is None or b is None or not a.ok or not b.ok:
            return d | K.D_UNKNOWN
        if a.owner_key != b.owner_key:
            d |= K.D_OWNER
        if a.acl_explicit != b.acl_explicit:
            d |= K.D_ACL_EXPLICIT
        if a.acl_inherited != b.acl_inherited:
            d |= K.D_ACL_INHERITED
        if a.mode != b.mode:
            d |= K.D_MODE
            if b.mode & 0o077 < a.mode & 0o077:
                d |= K.D_STRICTER
        if a.selinux != b.selinux:
            d |= K.D_SELINUX
        if a.bits & 1 and not b.bits & 1:  # F_MOTW
            d |= K.D_MOTW_LOST
        if (a.bits ^ b.bits) & 2:  # F_EFS
            d |= K.D_EFS
        theirs = {n for n, _ in b.streams}
        lost = sum(sz for nm, sz in a.streams if nm not in theirs)
        if lost > 4096:
            d |= K.D_STREAMS_BIG
        elif lost > 0 or any(nm not in theirs for nm, _ in a.streams):
            d |= K.D_STREAMS_SMALL
        return d

    def risk_ctx(self) -> RK.Ctx:
        n = self.n
        act = self.act.copy()
        ref = np.where(self.target >= 0, self.target, self.refk)
        refc = np.maximum(ref, 0)
        ktags = np.where(ref >= 0, self.tags[refc], np.uint64(0)).astype(np.uint64)
        active = np.flatnonzero(act > K.A_LEAVE)
        diff = np.zeros(n, np.uint32)
        knlink = np.zeros(n, np.uint32)
        streams = np.zeros(n, np.int64)
        probed = np.zeros(n, bool)
        links_to = np.zeros(n, np.int32)
        sym_targets = self.target[act == K.A_SYMLINK]
        if len(sym_targets):
            tv, tc = np.unique(sym_targets[sym_targets >= 0], return_counts=True)
            cnt = dict(zip(tv.tolist(), tc.tolist(), strict=True))
        else:
            cnt = {}
        last_vol = np.zeros(n, bool)
        sync_dup = np.zeros(n, bool)
        for r in active.tolist():
            k = int(ref[r])
            diff[r] = self._diff(r, k)
            pk = self.probes.get(k) if k >= 0 else None
            pr = self.probes.get(r)
            probed[r] = bool(pr and pr.ok and pk and pk.ok)
            knlink[r] = pk.nlink if pk and pk.ok else 1
            if pr and pr.ok:
                theirs = {nm for nm, _ in pk.streams} if pk and pk.ok else set()
                streams[r] = sum(sz for nm, sz in pr.streams if nm not in theirs)
            links_to[r] = cnt.get(int(self.target[r]), 0) if act[r] == K.A_SYMLINK else 0
            gr_ = int(self.group_of[r])
            kv = {u.vol for u in self.gunits[gr_] if self.keep[u.rep] != K.K_NO}
            last_vol[r] = int(self.vol[r]) not in kv
            if int(self.tags[r]) & K.T_SYNC and k >= 0:
                ra = self.tagx.dir_facts(int(self.scan[r]), self.dirs[int(self.dir_of[r])]).sync_root
                kb = self.tagx.dir_facts(int(self.scan[k]), self.dirs[int(self.dir_of[k])]).sync_root
                sync_dup[r] = bool(ra) and os.path.normcase(ra) == os.path.normcase(kb)
        tags = self.tags.copy()
        tags[sync_dup] |= np.uint64(K.T_SYNC_DUP)
        g = self.group_of
        shared_g = (
            np.array([gr.shared_bytes > 0 for gr in self.groups], bool) if self.groups else np.zeros(0, bool)
        )
        btrfs_v = np.array([vc.fs == "btrfs" for vc in self.vols], bool)
        hdd_v = np.array([vc.media in ("hdd", "usb_hdd") for vc in self.vols], bool)
        notrash_v = np.array([not vc.trash for vc in self.vols], bool)
        inplace_v = np.array([self.recovery_in_place(i) for i in range(len(self.vols))], bool)
        vol = self.vol
        return RK.Ctx(
            act=act,
            tags=tags,
            ktags=ktags,
            diff=diff,
            cross=self.gcross[g] if n else np.zeros(0, bool),
            last_vol=last_vol,
            other_vol=np.where(ref >= 0, self.vol[refc] != vol, False),
            knlink=knlink,
            links_to=links_to,
            probed=probed,
            streams=streams,
            shared=shared_g[g] if n and len(shared_g) else np.zeros(n, bool),
            btrfs=btrfs_v[vol] if n and len(btrfs_v) else np.zeros(n, bool),
            other_scan=np.where(ref >= 0, self.scan[refc] != self.scan, False),
            in_place_recovery=inplace_v[vol] if n and len(inplace_v) else np.zeros(n, bool),
            no_trash=notrash_v[vol] if n and len(notrash_v) else np.zeros(n, bool),
            size=self.gsize[g] if n else np.zeros(0, np.int64),
            hdd=hdd_v[vol] if n and len(hdd_v) else np.zeros(n, bool),
            win=sys.platform == "win32",
            verify=self.policy.options.verify,
        )

    def _risks(self) -> None:
        ctx = self.risk_ctx()
        rows, ids, sevs = RK.evaluate(ctx)
        # risk-condition rules: only ever to 「不动」, then once more (「不动」 has no risks)
        changed = False
        sev = np.zeros(self.n, np.uint8)
        if len(rows):
            np.maximum.at(sev, rows, sevs)
        for rule in self.policy.action_rules:
            if not rule.enabled or not rule.when.risky:
                continue
            mask = self._match(rule.when) & (self.act > K.A_LEAVE)
            hit = np.zeros(self.n, bool)
            if rule.when.risk_at_least:
                hit |= sev >= K.SEV_CODE.get(rule.when.risk_at_least, K.SEV_HIGH)
            if rule.when.risk_any and len(rows):
                want = {
                    RK.IDX[x]
                    for x in (RK.BY_CODE[c].id if c in RK.BY_CODE else c for c in rule.when.risk_any)
                    if x in RK.IDX
                }
                on = np.isin(ids, np.array(sorted(want), np.int16)) if want else np.zeros(len(ids), bool)
                hit[rows[on]] = True
            todo = np.flatnonzero(mask & hit)
            for r in todo.tolist():
                self.want[r] = self.act[r]
                self.act[r] = K.A_LEAVE
                self.asrc[r] = K.SRC_ACTION_RISK
                self.aref[r] = rule.id
                self.target[r] = -1
                changed = True
        if changed:
            ctx = self.risk_ctx()
            rows, ids, sevs = RK.evaluate(ctx)
        self.risk_rows, self.risk_ids, self.risk_sev = rows, ids, sevs
        self.sev[:] = 0
        if len(rows):
            np.maximum.at(self.sev, rows, sevs)
        ng = len(self.groups)
        self.gsev = np.zeros(ng, np.uint8)
        if self.n and ng:
            np.maximum.at(self.gsev, self.group_of, self.sev)

    # ------------------------------------------------------------------ queries
    def risks_of(self, r: int) -> list[tuple[str, int]]:
        lo = int(np.searchsorted(self.risk_rows, r, "left"))
        hi = int(np.searchsorted(self.risk_rows, r, "right"))
        return [(RK.CATALOG[int(self.risk_ids[i])].id, int(self.risk_sev[i])) for i in range(lo, hi)]

    def freed(self) -> tuple[np.ndarray, np.ndarray]:
        """(now, later) bytes per member row for its action (kept per version)."""
        hit = self._freed_cache
        if hit is not None and hit[0] == self.version:
            return hit[1], hit[2]
        now, later = self._freed()
        self._freed_cache = (self.version, now, later)
        return now, later

    def changed_marks(self) -> np.ndarray:
        """Groups whose decisions changed since they were marked reviewed (kept per version)."""
        hit = self._changed_cache
        if hit is not None and hit[0] == self.version:
            return hit[1]
        from .query import changed_since_review

        out = changed_since_review(self)
        self._changed_cache = (self.version, out)
        return out

    def _freed(self) -> tuple[np.ndarray, np.ndarray]:
        own = self.is_rep & (self.act > K.A_LEAVE)
        now = np.where(own & (self.act == K.A_CLONE), self.alloc, 0)
        later = np.where(own & np.isin(self.act, (K.A_TRASH, K.A_HARDLINK, K.A_SYMLINK)), self.alloc, 0)
        return now.astype(np.int64), later.astype(np.int64)

    def high_sets(self) -> dict[str, np.ndarray]:
        """Per high-risk kind: the keys of the copies in it now."""
        out: dict[str, np.ndarray] = {}
        hi = self.risk_sev >= K.SEV_HIGH
        for i in np.unique(self.risk_ids[hi]).tolist():
            rows = self.risk_rows[hi & (self.risk_ids == i)]
            out[RK.CATALOG[i].id] = np.unique(self.keys[rows])
        return out

    def unacked(self) -> dict[str, int]:
        """High-risk kinds not acknowledged for every copy now in them: kind -> copies not covered."""
        out: dict[str, int] = {}
        for rid, keys in self.high_sets().items():
            ack = self.acks.get(rid)
            if ack is None:
                out[rid] = len(keys)
                continue
            missing = sum(1 for k in keys.tolist() if k not in ack.keys)
            if missing:
                out[rid] = missing
        return out

    def facts_progress(self) -> tuple[int, int]:
        """(looked at, to look at) of the risk check: the copies with an action and their keepers."""
        need = set(np.flatnonzero(self.act > K.A_LEAVE).tolist())
        ref = np.where(self.target >= 0, self.target, self.refk)
        for r in list(need):
            k = int(ref[r])
            if k >= 0:
                need.add(k)
        done = sum(1 for r in need if r in self.probes)
        return done, len(need)

    def probe_order(self) -> list[int]:
        """Rows to look at, the ones the gate waits for first."""
        ref = np.where(self.target >= 0, self.target, self.refk)
        first: list[int] = []
        seen: set[int] = set()
        for r in np.flatnonzero(self.act > K.A_LEAVE).tolist():
            for x in (r, int(ref[r])):
                if x >= 0 and x not in seen and x not in self.probes:
                    seen.add(x)
                    first.append(x)
        rest = [r for r in np.flatnonzero(self.is_rep).tolist() if r not in seen and r not in self.probes]
        return first + rest

    def gates(
        self, *, busy: bool = False, job_running: bool = False, scanning: bool = False, attention: int = 0
    ) -> list[str]:
        out: list[str] = []
        done, total = self.facts_progress()
        if done < total:
            out.append(K.TEXT["gate_facts"].format(pct=int(100 * done / max(1, total))))
        un = self.unacked()
        if un:
            out.append(K.TEXT["gate_acks"].format(n=len(un)))
        if job_running:
            out.append(K.TEXT["gate_job"])
        if scanning:
            out.append(K.TEXT["gate_scan"])
        if busy:
            out.append(K.TEXT["gate_busy"])
        if attention:
            out.append(K.TEXT["gate_attention"].format(n=attention))
        if not int(np.count_nonzero(self.act > K.A_LEAVE)):
            out.append(K.TEXT["gate_nothing"])
        return out

    # ------------------------------------------------------------------ edits
    def snapshot(self) -> Policy:
        return self.policy.clone()

    def commit(self, before: Policy, groups: Iterable[int] | None = None) -> None:
        self.undo_stack.append(before)
        del self.undo_stack[:-UNDO_STEPS]
        self.redo_stack.clear()
        self.approval = None
        self.recompute(groups)

    def undo(self) -> bool:
        if not self.undo_stack:
            return False
        self.redo_stack.append(self.policy)
        self.policy = self.undo_stack.pop()
        self.approval = None
        self.recompute()
        return True

    def redo(self) -> bool:
        if not self.redo_stack:
            return False
        self.undo_stack.append(self.policy)
        self.policy = self.redo_stack.pop()
        self.approval = None
        self.recompute()
        return True

    def unit_of_row(self, r: int) -> tuple[int, Unit] | None:
        g = int(self.group_of[r])
        for u in self.gunits[g]:
            if r in u.rows:
                return g, u
        return None

    def find(self, group: int, path: str) -> tuple[int, Unit] | None:
        if not 0 <= group < len(self.groups):
            return None
        key = os.path.normcase(path)
        for u in self.gunits[group]:
            if os.path.normcase(u.path) == key or any(os.path.normcase(a) == key for a in u.alt):
                return group, u
        return None

    def check_floor(self, groups: Iterable[int]) -> None:
        """Refuse a policy that leaves a group below its floor (every group at least one keeper; one
        per volume when that is on) because of manual 「不保留」 choices."""
        P = self.policy
        for gi in groups:
            g = self.groups[gi]
            if self._group_state(g) != G_IN:
                continue
            units = self.gunits[gi]
            d = _hex(g.digest)
            gi_ = self._inputs(g, {})
            nots = [
                i
                for i, m in enumerate(gi_.manual)
                if m is not None and m.keep == "not" and not units[i].forced
            ]
            if gi_.keep_set is not None:
                nots = [
                    i
                    for i, u in enumerate(units)
                    if not u.forced
                    and u.path not in gi_.keep_set
                    and not any(a in gi_.keep_set for a in u.alt)
                ]
            free = [i for i in range(len(units)) if i not in nots]
            if not free:
                raise Refused(K.TEXT["refuse_last_keeper"])
            floor = keep_floor(P, gi_, len(units))
            if len(free) < floor:
                raise Refused(K.TEXT["refuse_last_keeper"])
            if P.per_volume and len(g.volumes) > 1:
                for v in sorted({u.vol for u in units}):
                    on_v = [i for i, u in enumerate(units) if u.vol == v]
                    if on_v and all(i in nots for i in on_v):
                        raise Refused(
                            K.TEXT["refuse_volume"].format(vol=self.vols[v].name if v < len(self.vols) else v)
                        )
            _ = d

    # ------------------------------------------------------------------ approval
    def _stamp(self, r: int) -> dict[str, Any]:
        m = self.m[r]
        v = int(m["vol"])
        return {
            "path": self.paths[r],
            "vol": v,
            "dev": self.vols[v].dev if v < len(self.vols) else 0,
            "fid_lo": int(m["fid_lo"]),
            "fid_hi": int(m["fid_hi"]),
            "size": int(self.gsize[int(m["group"])]),
            "mtime": int(m["mtime"]),
            "change": int(m["change"]),
            "usn": int(m["usn"]),
            "row": r,
        }

    def symlink_text(self, r: int, k: int, style: str = "") -> str:
        style = style or self.policy.options.symlink_style
        src, dst = self.paths[r], self.paths[k]
        same_vol = int(self.vol[r]) == int(self.vol[k])
        through = bool((int(self.tags[r]) | int(self.tags[k])) & K.T_THROUGH_LINK)
        rel = style == "relative" or (style == "auto" and same_vol and not through)
        if rel:
            try:
                return os.path.relpath(dst, os.path.dirname(src))
            except ValueError:
                pass
        return dst

    def approve(self, *, through_link: Callable[[str], bool] | None = None) -> Approval:
        """Freeze the plan into steps (refused while facts are being checked or high risks are not
        acknowledged: the caller checks :meth:`gates` first)."""
        with self.lock:
            steps: list[Step] = []
            rows = np.flatnonzero(self.act > K.A_LEAVE)
            # order: by volume, by folder; in a group replacements and clones before the recycle bin
            order = sorted(
                rows.tolist(),
                key=lambda r: (
                    int(self.vol[r]),
                    self.dirs[int(self.dir_of[r])],
                    0 if self.act[r] != K.A_TRASH else 1,
                    -int(self.gsize[int(self.group_of[r])]),
                    self.paths[r],
                ),
            )
            for r in order:
                gi = int(self.group_of[r])
                g = self.groups[gi]
                act = int(self.act[r])
                keepers = [self._stamp(u.rep) for u in self.gunits[gi] if self.keep[u.rep] != K.K_NO]
                if self.policy.per_volume and len(g.volumes) > 1:
                    mine = [k for k in keepers if k["vol"] == int(self.vol[r])]
                    keepers = mine or keepers
                t = int(self.target[r])
                text = ""
                if act == K.A_SYMLINK and t >= 0:
                    if through_link is not None and (
                        through_link(self.dirs[int(self.dir_of[r])])
                        or through_link(self.dirs[int(self.dir_of[t])])
                    ):
                        self.tags[r] |= np.uint64(K.T_THROUGH_LINK)
                    text = self.symlink_text(r, t)
                s = self._stamp(r)
                verify_content = self.policy.options.verify == "content" or (
                    self.policy.options.verify == "auto"
                    and bool(
                        (int(self.tags[r]) | (int(self.tags[t]) if t >= 0 else 0))
                        & (K.T_NO_USN | K.T_RECENT1 | K.T_CACHE_ONLY)
                    )
                )
                steps.append(
                    Step(
                        seq=len(steps) + 1,
                        group=gi,
                        digest=_hex(g.digest),
                        action=K.ACTION_NAME[act],
                        path=self.paths[r],
                        vol=s["vol"],
                        dev=s["dev"],
                        fid_lo=s["fid_lo"],
                        fid_hi=s["fid_hi"],
                        size=s["size"],
                        mtime=s["mtime"],
                        change=s["change"],
                        usn=s["usn"],
                        parent=os.path.dirname(self.paths[r]),
                        keepers=keepers,
                        target=self._stamp(t) if t >= 0 else None,
                        text=text,
                        risks=[rid for rid, _ in self.risks_of(r)],
                        alloc=int(self.alloc[r]),
                        scan=int(self.scan[r]),
                        node=int(self.m["node"][r]),
                        verify_content=verify_content and act != K.A_CLONE,
                        row=r,
                    )
                )
            acks = {rid: a.count for rid, a in self.acks.items()}
            opts: dict[str, str] = {
                "on_failure": self.policy.options.on_failure,
                "verify": self.policy.options.verify,
                "symlink_style": self.policy.options.symlink_style,
            }
            blob = json.dumps(
                {"steps": [s.to_json() for s in steps], "acks": acks, "options": opts}, sort_keys=True
            ).encode("utf-8", "surrogatepass")
            token = hashlib.blake2b(blob, digest_size=16).hexdigest()
            self.approval = Approval(self.version, token, steps, acks, opts)
            return self.approval

    def ack(self, rid: str) -> int:
        """Acknowledge a high-risk kind for the copies in it now (returns how many)."""
        keys = self.high_sets().get(rid)
        if keys is None:
            return 0
        self.acks[rid] = Ack(frozenset(keys.tolist()), len(keys), time.time())
        self.approval = None
        self.version += 1
        return len(keys)

    def unack(self, rid: str) -> None:
        if self.acks.pop(rid, None) is not None:
            self.approval = None
            self.version += 1

    # ------------------------------------------------------------------ results changed
    def sync_results(self) -> bool:
        """Groups the monitor took apart since the plan was built become 「失效」 (not editable, not
        run). Returns whether anything changed."""
        res = self.res
        with res.lock:
            if res.version == self.results_version:
                return False
            self.results_version = res.version
            if res.n != self.n or len(res.groups) != len(self.groups):
                stale = [g.id for g in res.groups[: len(self.groups)] if not g.valid]
            else:
                stale = [g.id for g in res.groups if not g.valid]
            flags = res.m["flags"][: self.n].copy()
        changed = [gi for gi in stale if self.gstate[gi] != G_STALE]
        cleaned = np.flatnonzero((flags & F_CLEANED) != 0)
        newly = [int(r) for r in cleaned.tolist() if not int(self.m["flags"][r]) & F_CLEANED]
        for r in newly:
            self.m["flags"][r] = int(self.m["flags"][r]) | F_CLEANED
        regroup = sorted({int(self.group_of[r]) for r in newly})
        for gi in regroup:
            self.gunits[gi] = self._units_of(self.groups[gi])
        for gi in changed:
            self.groups[gi].valid = False
        if changed or regroup:
            self.recompute(sorted(set(changed) | set(regroup)))
            return True
        return False


def _under(path: str, folder: str) -> bool:
    from ..keep import _prefer_rank

    return _prefer_rank(path, [folder]) == 0


def policy_from_keep(
    keep: KeepOptions, *, criteria: Sequence[str] | None = None, exact: bool = False
) -> Policy:
    """The plan's base from the search's keep options (the drawer's rule line, its manual choices)."""
    p = Policy()
    first = RULE_TO_CRITERION.get(keep.rule, "oldest_created")
    if exact:
        p.criteria = [first]
    else:
        rest = [c for c in (criteria or ["plain_name", "shortest_path"]) if c != first]
        p.criteria = [first, *rest]
    p.per_volume = keep.per_volume
    p.folder_consistent = keep.folder_consistent
    if keep.prefer:
        p.keep_rules.append(Rule(p.new_id(), "keep", "prefer", Selector(), folders=list(keep.prefer)))
    for (digest, path), role in keep.overrides.items():
        if role == "keep":
            p.manual[(digest.hex(), path)] = ManualCopy(keep="keep")
        elif role == "extra":
            p.manual[(digest.hex(), path)] = ManualCopy(keep="not")
    return p


def group_keys(groups: Sequence[Group]) -> dict[str, int]:
    return {_hex(g.digest): g.id for g in groups if g.digest}


__all__ = ["Approval", "CleanupPlan", "ManualGroup", "Refused", "Step", "policy_from_keep"]

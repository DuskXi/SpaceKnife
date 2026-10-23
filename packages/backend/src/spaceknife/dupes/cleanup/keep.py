"""The layered keep engine (docs/dupes.md §16.4): which copies of a group stay, and why.

The keep set only grows by constraints; rules only decide who fills a place:

    keep = locked ∪ manual keep ∪ group keep set ∪ pair 「both」 ∪ keep_all ∪ fills

Fills come from constraints in this order: one per volume (when on, across volumes), one per folder
(``per_folder`` rules), ``keep_n`` (the most any matching rule asks), at least one. A fill narrows
its candidates (not kept, not manually 「不保留」) step by step, skipping a step that would leave none:
the pair side, ``prefer`` rules (in order, their folders in order), ``avoid`` rules (while others
remain), folder-consistent sides, the criteria (an ``order`` rule's, else the base chain), then the
shorter path and the smaller one. The step that leaves one candidate is the reason recorded.

With the base chain reduced to the drawer's rule (``exact`` mode), every group gets exactly the
keepers ``dupes.keep.choose`` gives (tested group by group).
"""

from __future__ import annotations

import os
from collections.abc import Sequence
from dataclasses import dataclass, field

from ..keep import _prefer_rank
from . import codes as K
from .policy import ManualCopy, PairRule, Policy, Rule, plain_name_keys


@dataclass(slots=True)
class Unit:
    """One physical file of a group as the engine sees it."""

    rep: int  # member row of its representative
    rows: list[int]
    path: str
    vol: int
    btime: int
    mtime: int
    forced: int = 0  # SRC_FORCED_* when it may only be kept
    alt: list[str] = field(default_factory=list)  # other paths of the same single-named file
    dir: str = ""
    name: str = ""
    depth: int = 0


@dataclass(slots=True)
class Decision:
    state: list[int]  # K_NO / K_KEEP / K_LOCKED per unit
    src: list[int]  # SRC_* per unit
    ref: list[int]  # rule id, prefer rank, criterion index ... per unit
    conflict: bool = False  # two rules of one layer disagreed (the first one won)
    only_avoided: bool = False  # every candidate was in an 「不想留」 place
    safety: bool = False  # a fill had to take a copy marked 「不保留」


def _under(path: str, folder: str) -> bool:
    return _prefer_rank(path, [folder]) == 0


def _crit_key(u: Unit, crit: str, plain: int) -> tuple[int, ...]:
    if crit == "oldest_created":
        return (u.btime if u.btime > 0 else u.mtime,)
    if crit == "oldest_modified":
        return (u.mtime,)
    if crit == "newest_modified":
        return (-u.mtime,)
    if crit == "shortest_path":
        return (len(u.path),)
    if crit == "longest_path":
        return (-len(u.path),)
    if crit == "shallowest":
        return (u.depth,)
    if crit == "deepest":
        return (-u.depth,)
    if crit == "plain_name":
        return (plain,)
    return (0,)


@dataclass(slots=True)
class GroupInputs:
    """What one group's decision depends on besides its units."""

    digest: str  # hex
    manual: list[ManualCopy | None]  # per unit (any of its paths)
    keep_set: set[str] | None
    keep_masks: dict[int, list[bool]]  # keep rule id -> per unit: the selector matches
    group_rules: dict[int, bool]  # keep rule id -> the rule applies to the group (any unit matches)
    pairs: list[tuple[int, PairRule, list[bool], list[bool]]]  # (index, rule, unit on side a, on side b)
    side: dict[str, int] | None  # folder-consistent: folder -> rank (lower keeps)


def decide(units: Sequence[Unit], P: Policy, gi: GroupInputs, *, exact: bool = False) -> Decision:
    n = len(units)
    state = [K.K_NO] * n
    src = [K.SRC_NOT_KEPT] * n
    ref = [0] * n
    d = Decision(state, src, ref)
    not_keep: set[int] = set()
    for i, u in enumerate(units):
        if u.forced:
            state[i], src[i] = K.K_LOCKED, u.forced
    # 2. manual per copy
    for i, m in enumerate(gi.manual):
        if m is None or state[i] == K.K_LOCKED:
            continue
        if m.keep == "keep":
            state[i], src[i] = K.K_KEEP, K.SRC_MANUAL_KEEP
        elif m.keep == "not":
            not_keep.add(i)
            src[i] = K.SRC_MANUAL_NOT
    # 3. the group's keep set
    if gi.keep_set is not None:
        for i, u in enumerate(units):
            if state[i] == K.K_LOCKED:
                continue
            if u.path in gi.keep_set or any(a in gi.keep_set for a in u.alt):
                state[i], src[i] = K.K_KEEP, K.SRC_GROUP_SET
            else:
                not_keep.add(i)
                if state[i] == K.K_KEEP:
                    state[i] = K.K_NO
                src[i] = K.SRC_GROUP_SET
    # 4. folder pairs kept on both sides
    for pi, pr, on_a, on_b in gi.pairs:
        if pr.keep != "both":
            continue
        for i in range(n):
            if (on_a[i] or on_b[i]) and state[i] == K.K_NO and i not in not_keep:
                state[i], src[i], ref[i] = K.K_KEEP, K.SRC_PAIR_BOTH, pi
    keep_rules = [r for r in P.keep_rules if r.enabled]
    # 5. keep_all
    for r in keep_rules:
        if r.effect != "keep_all":
            continue
        mask = gi.keep_masks.get(r.id)
        if mask is None:
            continue
        for i in range(n):
            if mask[i] and state[i] == K.K_NO and i not in not_keep:
                state[i], src[i], ref[i] = K.K_KEEP, K.SRC_KEEP_ALL, r.id
    plain = plain_name_keys([u.name for u in units]) if not exact and "plain_name" in P.criteria else [0] * n
    order_rule = next(
        (r for r in keep_rules if r.effect == "order" and gi.group_rules.get(r.id) and r.criteria), None
    )
    criteria = list(order_rule.criteria) if order_rule is not None else list(P.criteria)

    def best(cands: Sequence[int]) -> tuple[int, int, int] | None:
        pool = [i for i in cands if state[i] == K.K_NO and i not in not_keep]
        if not pool:
            pool = [i for i in cands if state[i] == K.K_NO]
            if not pool:
                return None
            d.safety = True
            if len(pool) == 1:
                return pool[0], K.SRC_SAFETY_NET, 0
        # a. the side of a folder pair (the most specific rule first)
        for pi, pr, on_a, on_b in gi.pairs:
            if pr.keep not in ("a", "b"):
                continue
            want = on_a if pr.keep == "a" else on_b
            sub = [i for i in pool if want[i]]
            if sub and len(sub) < len(pool):
                pool = sub
                if len(pool) == 1:
                    return pool[0], K.SRC_PAIR_SIDE, pi
        # b. prefer rules
        hit_prefer: int | None = None
        for r in keep_rules:
            if r.effect != "prefer" or not r.folders or not gi.group_rules.get(r.id, True):
                continue
            ranks = {i: _prefer_rank(units[i].path, r.folders) for i in pool}
            top = min(ranks.values())
            if top < len(r.folders) and any(v != top for v in ranks.values()):
                if hit_prefer is not None:
                    d.conflict = True
                    continue
                hit_prefer = r.id
                pool = [i for i in pool if ranks[i] == top]
                if len(pool) == 1:
                    return pool[0], K.SRC_PREFER, r.id
        # c. avoid rules
        for r in keep_rules:
            if r.effect != "avoid":
                continue
            mask = gi.keep_masks.get(r.id)
            av = [
                i
                for i in pool
                if (any(_under(units[i].path, f) for f in r.folders) if r.folders else bool(mask and mask[i]))
            ]
            rest = [i for i in pool if i not in av]
            if av and rest:
                pool = rest
                if len(pool) == 1:
                    return pool[0], K.SRC_AVOID, r.id
            elif av and not rest:
                d.only_avoided = True
        # d. folder-consistent sides
        if gi.side:
            ranks2 = {i: gi.side.get(os.path.dirname(units[i].path), 1 << 30) for i in pool}
            top2 = min(ranks2.values())
            if top2 < (1 << 30) and sum(1 for v in ranks2.values() if v == top2) < len(pool):
                pool = [i for i in pool if ranks2[i] == top2]
                if len(pool) == 1:
                    return pool[0], K.SRC_FOLDER, 0
        # e. criteria
        for ci, crit in enumerate(criteria):
            keys = {i: _crit_key(units[i], crit, plain[i]) for i in pool}
            top3 = min(keys.values())
            sub = [i for i in pool if keys[i] == top3]
            if len(sub) < len(pool):
                pool = sub
                if len(pool) == 1:
                    return pool[0], K.SRC_CRITERION, ci
        # f. ties: the shorter path, then the smaller one
        pick = min(pool, key=lambda i: (len(units[i].path), units[i].path))
        return pick, (K.SRC_TIE if len(pool) > 1 else K.SRC_CRITERION), 0

    def take(i: int, s: int, r: int, why: int, why_ref: int) -> None:
        state[i] = K.K_KEEP
        # the constraint that asked for a keeper is the reason, unless a rule chose this one
        if s in (K.SRC_CRITERION, K.SRC_TIE) and why in (K.SRC_PER_VOLUME, K.SRC_PER_FOLDER, K.SRC_KEEP_N):
            src[i], ref[i] = why, why_ref
        else:
            src[i], ref[i] = s, r

    vols = sorted({u.vol for u in units})
    if P.per_volume and len(vols) > 1:
        for vi, v in enumerate(vols):
            members = [i for i, u in enumerate(units) if u.vol == v]
            if any(state[i] != K.K_NO for i in members):
                continue
            b = best(members)
            if b is not None:
                take(b[0], b[1], b[2], K.SRC_PER_VOLUME if vi > 0 else 0, v)
    for r in keep_rules:
        if r.effect != "per_folder" or not gi.group_rules.get(r.id):
            continue
        mask = gi.keep_masks.get(r.id) or [True] * n
        by_dir: dict[str, list[int]] = {}
        for i, u in enumerate(units):
            if mask[i]:
                by_dir.setdefault(os.path.dirname(u.path), []).append(i)
        for members in by_dir.values():
            if any(state[i] != K.K_NO for i in members):
                continue
            b = best(members)
            if b is not None:
                take(b[0], b[1], b[2], K.SRC_PER_FOLDER, r.id)
    need = max([1, *[max(1, r.n) for r in keep_rules if r.effect == "keep_n" and gi.group_rules.get(r.id)]])
    need = min(need, n)
    while sum(1 for s in state if s != K.K_NO) < need:
        b = best(range(n))
        if b is None:
            break
        take(b[0], b[1], b[2], K.SRC_KEEP_N if need > 1 else 0, need)
    return d


def keep_floor(P: Policy, gi: GroupInputs, n: int) -> int:
    """Copies a group must keep at least: max(1, keep_n of the rules that apply)."""
    rules: list[Rule] = [
        r for r in P.keep_rules if r.enabled and r.effect == "keep_n" and gi.group_rules.get(r.id)
    ]
    return min(n, max([1, *[max(1, r.n) for r in rules]]))

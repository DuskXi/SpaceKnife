"""Edits of a cleanup plan and their previews (docs/dupes.md §16.7.4).

An edit names its scope (groups, copies, or a filter), the backend resolves it, applies it to the
policy, checks the floors (a group keeps one copy at least, one per volume when that is on) and
recomputes what it touched. A bulk change to an action that cannot be done somewhere leaves those
copies as they were (the preview counts them by reason); copies chosen by hand keep their choice
unless ``include_overridden``. Every edit can be previewed first: applied, measured, rolled back.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import numpy as np

from . import codes as K
from . import risks as RK
from .plan import CleanupPlan, Refused, _hex
from .policy import CRITERIA, ManualCopy, ManualGroup, PairRule, Policy, Rule, Selector, preset
from .query import evaluate, group_signature


@dataclass(slots=True)
class Scope:
    groups: list[int] = field(default_factory=list)
    copies: list[tuple[int, int]] = field(default_factory=list)  # (group, unit representative row)


def resolve(plan: CleanupPlan, scope: Any) -> Scope:
    out = Scope()
    if scope is None:
        return out
    ng = len(plan.groups)
    if scope.copies:
        for ref in scope.copies:
            hit = plan.find(ref.group, ref.path)
            if hit is not None:
                out.copies.append((hit[0], hit[1].rep))
        out.groups = sorted({g for g, _ in out.copies})
        return out
    gm = np.zeros(ng, bool)
    rm = np.zeros(plan.n, bool)
    if scope.groups is not None:
        for g in scope.groups:
            if 0 <= g < ng:
                gm[g] = True
        rm = gm[plan.group_of] if plan.n else rm
    if scope.filter is not None:
        fg, fr = evaluate(plan, scope.filter)
        if scope.groups is not None:
            gm &= fg
            rm &= fr
        else:
            gm, rm = fg, fr
    out.groups = np.flatnonzero(gm).tolist()
    reps = np.flatnonzero(rm & plan.is_rep).tolist()
    out.copies = [(int(plan.group_of[r]), int(r)) for r in reps]
    return out


def _manual(P: Policy, digest: str, path: str) -> ManualCopy:
    m = P.manual.get((digest, path))
    if m is None:
        m = P.manual[(digest, path)] = ManualCopy()
    return m


def _rule_from(model: Any, P: Policy, rid: int = 0) -> Rule:
    w = model.when
    sel = Selector(**{k: getattr(w, k) for k in Selector.__slots__})
    if model.kind == "action" and sel.risky and model.effect != "leave":
        raise Refused(K.TEXT["refuse_risk_rule"])
    if model.kind == "keep" and model.effect not in (
        "keep_all",
        "prefer",
        "avoid",
        "per_folder",
        "keep_n",
        "order",
    ):
        raise Refused(K.TEXT["refuse_rule"])
    if model.kind == "action" and model.effect not in K.ACTIONS:
        raise Refused(K.TEXT["refuse_rule"])
    return Rule(
        id=rid or P.new_id(),
        kind=model.kind,
        effect=model.effect,
        when=sel,
        enabled=model.enabled,
        label=model.label,
        label_key=model.label_key,
        folders=list(model.folders),
        n=model.n,
        criteria=list(model.criteria),
        fallback=list(model.fallback),
        target_under=model.target_under,
        symlink_style=model.symlink_style,
    )


def apply(plan: CleanupPlan, edit: Any) -> list[int] | None:
    """Apply ``edit`` to the plan's policy (not yet recomputed). Returns the groups touched, or
    None when every group must be recomputed. Raises :class:`Refused`."""
    P = plan.policy
    op = edit.op
    sc = resolve(plan, edit.scope)
    if op in ("keep_add", "keep_remove", "keep_clear", "keep_only", "action_set", "target_set", "keep_set"):
        from .plan import G_IN

        live = [(gi, r) for gi, r in sc.copies if plan.gstate[gi] == G_IN]
        if len(live) < len(sc.copies) and edit.scope.copies is not None:
            raise Refused(K.TEXT["refuse_stale"])
        sc.copies = live
        sc.groups = [gi for gi in sc.groups if plan.gstate[gi] == G_IN]
    if op == "keep_add":
        for gi, r in sc.copies:
            u = plan.unit_of_row(r)
            if u is None or u[1].forced:
                continue
            _manual(P, _hex(plan.groups[gi].digest), u[1].path).keep = "keep"
        groups = sc.groups
        plan.check_floor(groups)
        return groups
    if op == "keep_remove":
        for gi, r in sc.copies:
            u = plan.unit_of_row(r)
            if u is None:
                continue
            if u[1].forced:
                raise Refused(K.TEXT["refuse_locked"])
            d = _hex(plan.groups[gi].digest)
            _manual(P, d, u[1].path).keep = "not"
            mg = P.groups.get(d)
            if mg is not None and mg.keep_set is not None:
                mg.keep_set = [p for p in mg.keep_set if p != u[1].path]
        plan.check_floor(sc.groups)
        return sc.groups
    if op == "keep_clear":
        for gi, r in sc.copies:
            u = plan.unit_of_row(r)
            if u is not None:
                m = P.manual.get((_hex(plan.groups[gi].digest), u[1].path))
                if m is not None:
                    m.keep = ""
        return sc.groups
    if op in ("keep_only", "keep_set"):
        by_group: dict[int, list[str]] = {}
        if op == "keep_only":
            for gi, r in sc.copies:
                u = plan.unit_of_row(r)
                if u is not None:
                    by_group.setdefault(gi, []).append(u[1].path)
        else:
            for gi in sc.groups:
                by_group[gi] = list(edit.paths)
        for gi, paths in by_group.items():
            d = _hex(plan.groups[gi].digest)
            P.groups.setdefault(d, ManualGroup()).keep_set = paths
            for unit in plan.gunits[gi]:
                m = P.manual.get((d, unit.path))
                if m is not None:
                    m.keep = ""
        plan.check_floor(by_group.keys())
        return sorted(by_group)
    if op == "action_set":
        if edit.action is None:
            raise Refused(K.TEXT["refuse_rule"])
        code = K.ACTION_CODE[edit.action]
        touched: set[int] = set()
        for gi, r in sc.copies:
            if plan.keep[r] != K.K_NO:
                continue
            if code != K.A_LEAVE and plan.na[r, code - 1] != K.NA_OK:
                continue  # left as it was (the preview says why)
            d = _hex(plan.groups[gi].digest)
            u = plan.unit_of_row(r)
            if u is None:
                continue
            m = P.manual.get((d, u[1].path))
            if m is not None and m.action and not edit.include_overridden and edit.scope.copies is None:
                continue
            mm = _manual(P, d, u[1].path)
            mm.action = edit.action
            if edit.target:
                mm.target = edit.target
            touched.add(gi)
        return sorted(touched)
    if op == "target_set":
        for gi, r in sc.copies:
            u = plan.unit_of_row(r)
            if u is None:
                continue
            ok = any(k.path == edit.target for k in plan.gunits[gi] if plan.keep[k.rep] != K.K_NO)
            if not ok:
                raise Refused(K.TEXT["refuse_target"])
            _manual(P, _hex(plan.groups[gi].digest), u[1].path).target = edit.target
        return sc.groups
    if op in ("exclude", "include"):
        for gi in sc.groups:
            d = _hex(plan.groups[gi].digest)
            P.groups.setdefault(d, ManualGroup()).excluded = op == "exclude"
        return sc.groups
    if op == "override_clear":
        if edit.scope.groups is None and edit.scope.copies is None and edit.scope.filter is None:
            P.manual.clear()
            P.groups.clear()
            return None
        for gi in sc.groups:
            d = _hex(plan.groups[gi].digest)
            P.groups.pop(d, None)
            for unit in plan.gunits[gi]:
                for p in (unit.path, *unit.alt):
                    P.manual.pop((d, p), None)
        return sc.groups
    if op == "rule_add":
        if edit.rule is None:
            raise Refused(K.TEXT["refuse_rule"])
        rule = _rule_from(edit.rule, P)
        lst = P.keep_rules if rule.kind == "keep" else P.action_rules
        idx = edit.index if 0 <= edit.index <= len(lst) and edit.index else len(lst)
        lst.insert(idx, rule)
        P.preset = "custom" if P.preset != "custom" else P.preset
        return None
    if op == "rule_update":
        old = P.rule(edit.rule_id)
        if old is None or edit.rule is None:
            raise Refused(K.TEXT["refuse_rule"])
        new = _rule_from(edit.rule, P, rid=old.id)
        lst = P.keep_rules if old.kind == "keep" else P.action_rules
        lst[lst.index(old)] = new
        return None
    if op == "rule_remove":
        old = P.rule(edit.rule_id)
        if old is None:
            raise Refused(K.TEXT["refuse_rule"])
        (P.keep_rules if old.kind == "keep" else P.action_rules).remove(old)
        return None
    if op == "rule_move":
        old = P.rule(edit.rule_id)
        if old is None:
            raise Refused(K.TEXT["refuse_rule"])
        lst = P.keep_rules if old.kind == "keep" else P.action_rules
        lst.remove(old)
        lst.insert(max(0, min(edit.index, len(lst))), old)
        return None
    if op == "pair_set":
        if edit.pair is None:
            raise Refused(K.TEXT["refuse_rule"])
        pr = PairRule(edit.pair.a, edit.pair.b, edit.pair.keep, edit.pair.other_action)
        P.pairs = [x for x in P.pairs if not (x.a == pr.a and x.b == pr.b)] + [pr]
        return None
    if op == "pair_remove":
        if edit.pair is None:
            raise Refused(K.TEXT["refuse_rule"])
        P.pairs = [x for x in P.pairs if not (x.a == edit.pair.a and x.b == edit.pair.b)]
        return None
    if op == "preset":
        name = edit.preset or "recommended"
        plan.policy = preset(name, P, backup_volume=edit.backup_volume)
        return None
    if op == "options":
        o = edit.options
        if o is None:
            return []
        if o.on_failure:
            P.options.on_failure = o.on_failure
        if o.verify:
            P.options.verify = o.verify
        if o.symlink_style:
            P.options.symlink_style = o.symlink_style
        if o.criteria is not None:
            P.criteria = [c for c in o.criteria if c in CRITERIA] or ["oldest_created"]
        if o.per_volume is not None:
            P.per_volume = o.per_volume
        if o.folder_consistent is not None:
            P.folder_consistent = o.folder_consistent
        if o.default_action is not None:
            P.default_action = o.default_action
        return None
    if op == "review":
        marks = plan.review_marks
        for gi in sc.groups:
            d = _hex(plan.groups[gi].digest)
            if edit.value:
                P.reviewed[d] = plan.version
                marks[gi] = group_signature(plan, gi)
            else:
                P.reviewed.pop(d, None)
                marks.pop(gi, None)
        return []
    raise Refused(K.TEXT["refuse_rule"])


_SAVED = (
    "keep",
    "ksrc",
    "kref",
    "act",
    "asrc",
    "aref",
    "want",
    "wna",
    "target",
    "refk",
    "na",
    "tgt",
    "sev",
    "gstate",
    "gsev",
    "gconflict",
    "gonly_avoided",
    "gsafety",
    "risk_rows",
    "risk_ids",
    "risk_sev",
)


def _save(plan: CleanupPlan) -> dict[str, Any]:
    st: dict[str, Any] = {k: getattr(plan, k).copy() for k in _SAVED}
    st["policy"] = plan.policy.clone()
    st["version"] = plan.version
    st["approval"] = plan.approval
    st["undo"] = list(plan.undo_stack)
    st["redo"] = list(plan.redo_stack)
    st["side"] = plan._side
    return st


def _restore(plan: CleanupPlan, st: dict[str, Any]) -> None:
    for k in _SAVED:
        setattr(plan, k, st[k])
    plan.policy = st["policy"]
    plan.version = st["version"]
    plan.approval = st["approval"]
    plan.undo_stack = st["undo"]
    plan.redo_stack = st["redo"]
    plan._side = st["side"]
    plan._freed_cache = None
    plan._changed_cache = None


def _counts(plan: CleanupPlan) -> tuple[dict[str, int], int]:
    acts = plan.act[plan.is_rep & (plan.keep == K.K_NO)]
    out: dict[str, int] = {name: int(np.count_nonzero(acts == code)) for code, name in K.ACTION_NAME.items()}
    out["keep"] = int(np.count_nonzero(plan.is_rep & (plan.keep != K.K_NO)))
    now, later = plan.freed()
    return out, int(now.sum() + later.sum())


def _risk_counts(plan: CleanupPlan) -> dict[str, int]:
    out: dict[str, int] = {}
    for i, c in zip(*np.unique(plan.risk_ids, return_counts=True), strict=True):
        out[RK.CATALOG[int(i)].id] = int(c)
    return out


def edit(plan: CleanupPlan, e: Any) -> None:
    """Apply and keep an edit (refused edits change nothing)."""
    with plan.lock:
        if e.op == "ack":
            plan.ack(e.risk)
            return
        if e.op == "unack":
            plan.unack(e.risk)
            return
        before = plan.snapshot()
        try:
            groups = apply(plan, e)
        except Refused:
            plan.policy = before
            raise
        if e.op == "review":
            plan.version += 1
            return
        plan.commit(before, groups)


def preview(plan: CleanupPlan, e: Any) -> dict[str, Any]:
    """What an edit would change, without keeping it."""
    with plan.lock:
        saved = _save(plan)
        b_counts, b_freed = _counts(plan)
        b_risks = _risk_counts(plan)
        b_act = plan.act.copy()
        b_keep = plan.keep.copy()
        refused = ""
        unavailable: dict[str, int] = {}
        protected = 0
        if e.op == "action_set" and e.action is not None:
            sc = resolve(plan, e.scope)
            code = K.ACTION_CODE[e.action]
            for gi, r in sc.copies:
                if plan.keep[r] != K.K_NO:
                    continue
                if code != K.A_LEAVE and plan.na[r, code - 1] != K.NA_OK:
                    why = K.NA_NAMES.get(int(plan.na[r, code - 1]), "unknown")
                    unavailable[why] = unavailable.get(why, 0) + 1
                    continue
                u = plan.unit_of_row(r)
                d = _hex(plan.groups[gi].digest)
                if u is not None and not e.include_overridden and e.scope.copies is None:
                    m = plan.policy.manual.get((d, u[1].path))
                    if m is not None and m.action:
                        protected += 1
        try:
            groups = apply(plan, e)
            if e.op not in ("review",):
                plan.recompute(groups)
        except Refused as exc:
            refused = str(exc)
        a_counts, a_freed = _counts(plan)
        a_risks = _risk_counts(plan)
        changed_rows = np.flatnonzero((plan.act != b_act) | (plan.keep != b_keep))
        examples = []
        for r in changed_rows[:10].tolist():
            examples.append(
                {
                    "path": plan.paths[r],
                    "before": "keep" if b_keep[r] != K.K_NO else K.ACTION_NAME.get(int(b_act[r]), "leave"),
                    "after": "keep"
                    if plan.keep[r] != K.K_NO
                    else K.ACTION_NAME.get(int(plan.act[r]), "leave"),
                }
            )
        added = {k: v - b_risks.get(k, 0) for k, v in a_risks.items() if v > b_risks.get(k, 0)}
        removed = {k: b_risks[k] - a_risks.get(k, 0) for k in b_risks if b_risks[k] > a_risks.get(k, 0)}
        out = {
            "before": b_counts,
            "after": a_counts,
            "freed_before": b_freed,
            "freed_after": a_freed,
            "unavailable": unavailable,
            "risks_added": added,
            "risks_removed": removed,
            "protected": protected,
            "changed": len(np.unique(plan.group_of[changed_rows])) if len(changed_rows) else 0,
            "examples": examples,
            "refused": refused,
        }
        _restore(plan, saved)
        return out


__all__ = ["apply", "edit", "preview", "resolve"]

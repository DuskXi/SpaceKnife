"""The filter syntax of the cleanup plan (docs/dupes.md §16.7.3).

``risk:high`` ``risk:motw_lost`` ``sev:>=medium`` · ``action:hardlink`` ``action:!leave`` ``avail:symlink``
``-avail:hardlink`` · ``vol:D:`` ``fs:refs`` ``cross:yes`` · ``in:"D:\\Photos"`` ``name:*.psd``
``ext:jpg,png`` ``class:document`` · ``size:>100MB`` ``copies:>=3`` ``mtime:<2020-01-01`` ·
``source:manual`` ``source:rule:3`` ``source:fallback`` ``source:safety_net`` ``conflict:yes`` ·
``reviewed:no`` ``changed:yes`` ``acked:no`` ``state:stale`` · ``tag:sync`` · any other word: in
the path. Terms are ANDed; a leading ``-`` negates one. A copy-level term holds for a group when one
of its copies (the representatives, not keepers unless the term is about keepers) holds it.
"""

from __future__ import annotations

import fnmatch
import os
import re
import shlex
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

import numpy as np

from ...i18n import tr
from . import codes as K
from . import risks as RK

if TYPE_CHECKING:
    from .plan import CleanupPlan

_UNITS = {
    "": 1,
    "B": 1,
    "K": 1 << 10,
    "KB": 1 << 10,
    "M": 1 << 20,
    "MB": 1 << 20,
    "G": 1 << 30,
    "GB": 1 << 30,
    "T": 1 << 40,
    "TB": 1 << 40,
}
_CMP = re.compile(r"^(>=|<=|>|<|=)?(.+)$")


def _num(text: str) -> float:
    m = re.match(r"^\s*([\d.]+)\s*([A-Za-z]*)\s*$", text)
    if not m:
        raise ValueError(text)
    return float(m.group(1)) * _UNITS.get(m.group(2).upper(), 1)


def _cmp(arr: np.ndarray, spec: str, conv: Any = _num) -> np.ndarray:
    m = _CMP.match(spec)
    if not m:
        return np.ones(len(arr), bool)
    op, val = m.group(1) or "=", m.group(2)
    v = conv(val)
    out: np.ndarray
    if op == ">":
        out = arr > v
    elif op == "<":
        out = arr < v
    elif op == ">=":
        out = arr >= v
    elif op == "<=":
        out = arr <= v
    else:
        out = arr == v
    return out


def _date(text: str) -> float:
    return datetime.fromisoformat(text).replace(tzinfo=UTC).timestamp() * 1e9


def _yes(v: str) -> bool:
    return v.lower() in ("yes", "y", "true", "1", "是", "on")  # i18n: ignore


def split_terms(query: str) -> list[str]:
    try:
        return shlex.split(query, posix=True)
    except ValueError:
        return query.split()


def evaluate(plan: CleanupPlan, query: str) -> tuple[np.ndarray, np.ndarray]:
    """(groups that match, member rows that match the copy-level terms)."""
    n = plan.n
    ng = len(plan.groups)
    gmask = np.ones(ng, bool)
    rmask = np.ones(n, bool)
    rep = plan.is_rep
    for term in split_terms(query or ""):
        if _why(term):
            # a term it cannot read matches nothing (said by problems()): a typo never widens what a
            # bulk edit on 「符合筛选的全部」 reaches
            return np.zeros(ng, bool), np.zeros(n, bool)
        neg = term.startswith("-") and len(term) > 1
        t = term[1:] if neg else term
        key, sep, val = t.partition(":")
        key = key.lower()
        rows: np.ndarray | None = None  # a copy-level term
        grp: np.ndarray | None = None  # a group-level term
        try:
            if not sep:
                low = t.lower()
                rows = np.fromiter((low in p.lower() for p in plan.paths), bool, n)
            elif key in ("risk", "sev"):
                v = val.lower()
                if v.startswith((">=", "<=", ">", "<", "=")) or v in K.SEV_CODE:
                    spec = v if v[0] in "<>=" else ">=" + v
                    m = _CMP.match(spec)
                    assert m is not None
                    level = K.SEV_CODE.get(m.group(2), K.SEV_HIGH)
                    rows = _cmp(plan.sev.astype(np.int64), (m.group(1) or "=") + str(level), float)
                else:
                    rid = RK.BY_CODE[v].id if v in RK.BY_CODE else val.upper()
                    want = RK.IDX.get(rid, -1)
                    rows = np.zeros(n, bool)
                    rows[plan.risk_rows[plan.risk_ids == want]] = True
            elif key == "action":
                v = val.lower()
                inv = v.startswith("!")
                code = K.ACTION_CODE.get(v.lstrip("!"), -1)
                rows = (plan.act == code) & rep
                if inv:
                    rows = rep & (plan.keep == K.K_NO) & (plan.act != code)
            elif key == "avail":
                code = K.ACTION_CODE.get(val.lower(), -1)
                rows = (
                    rep & (plan.keep == K.K_NO) & (plan.na[:, max(code, 1) - 1] == K.NA_OK)
                    if code > 0
                    else np.zeros(n, bool)
                )
            elif key == "vol":
                want_v = val.lower().rstrip("\\/")
                vm = np.array([vc.name.lower().rstrip("\\/") == want_v for vc in plan.vols], bool)
                rows = vm[plan.vol] if len(vm) else np.zeros(n, bool)
            elif key == "fs":
                fm = np.array([vc.fs == val.lower() for vc in plan.vols], bool)
                rows = fm[plan.vol] if len(fm) else np.zeros(n, bool)
            elif key == "cross":
                grp = plan.gcross == _yes(val)
            elif key == "in":
                base = os.path.normcase(os.path.normpath(val)).rstrip("\\/")
                per_dir = np.array(
                    [
                        os.path.normcase(d) == base or os.path.normcase(d).startswith(base + os.sep)
                        for d in plan.dirs
                    ],
                    bool,
                )
                rows = per_dir[plan.dir_of] if len(per_dir) else np.zeros(n, bool)
            elif key == "name":
                pat = val.lower()
                rows = np.fromiter((fnmatch.fnmatchcase(nm.lower(), pat) for nm in plan.names), bool, n)
            elif key == "ext":
                exts = {"." + e.lower().lstrip(".") for e in val.split(",") if e}
                rows = np.fromiter((e in exts for e in plan.ext), bool, n)
            elif key == "class":
                from ...classes import active

                table = active()
                rows = np.fromiter((val in table.classes_of(e.lstrip(".")) for e in plan.ext), bool, n)
            elif key == "size":
                grp = _cmp(plan.gsize.astype(np.float64), val)
            elif key == "copies":
                units = np.array([len(u) for u in plan.gunits], np.float64)
                grp = _cmp(units, val, float)
            elif key == "mtime":
                rows = _cmp(plan.m["mtime"].astype(np.float64), val, _date)
            elif key == "source":
                v = val.lower()
                if v.startswith("rule:"):
                    rule_no = int(v.split(":", 1)[1])
                    rows = ((plan.asrc == K.SRC_ACTION_RULE) & (plan.aref == rule_no)) | (
                        np.isin(
                            plan.ksrc,
                            (K.SRC_KEEP_ALL, K.SRC_PREFER, K.SRC_AVOID, K.SRC_PER_FOLDER, K.SRC_KEEP_N),
                        )
                        & (plan.kref == rule_no)
                    )
                elif v == "manual":
                    rows = np.isin(plan.ksrc, K.MANUAL_SOURCES) | (plan.asrc == K.SRC_ACTION_MANUAL)
                elif v == "fallback":
                    rows = plan.asrc == K.SRC_ACTION_FALLBACK
                elif v in ("safety_net", "safety"):
                    grp = plan.gsafety.copy()
                elif v == "pair":
                    rows = np.isin(plan.ksrc, (K.SRC_PAIR_SIDE, K.SRC_PAIR_BOTH)) | (
                        plan.asrc == K.SRC_ACTION_PAIR
                    )
                elif v == "preset":
                    rows = plan.asrc == K.SRC_ACTION_PRESET
                elif v == "risk":
                    rows = plan.asrc == K.SRC_ACTION_RISK
                elif v == "forced":
                    rows = np.isin(plan.ksrc, K.FORCED_SOURCES)
                else:
                    rows = np.zeros(n, bool)
            elif key == "conflict":
                grp = plan.gconflict == _yes(val)
            elif key == "reviewed":
                rev = np.array([_hexd(g) in plan.policy.reviewed for g in plan.groups], bool)
                grp = rev == _yes(val)
            elif key == "changed":
                grp = changed_since_review(plan) == _yes(val)
            elif key == "acked":
                un = plan.unacked()
                rows = np.zeros(n, bool)
                for rid in un:
                    idx = RK.IDX.get(rid, -1)
                    rows[plan.risk_rows[(plan.risk_ids == idx) & (plan.risk_sev >= K.SEV_HIGH)]] = True
                if _yes(val):
                    hi = np.zeros(n, bool)
                    hi[plan.risk_rows[plan.risk_sev >= K.SEV_HIGH]] = True
                    rows = hi & ~rows
            elif key == "state":
                from .plan import G_NAMES

                code_s = {v: k for k, v in G_NAMES.items()}.get(val.lower(), -1)
                grp = plan.gstate == code_s
            elif key == "tag":
                bits = 0
                for tn in val.split(","):
                    bits |= K.TAG_CODE.get(tn.lower(), 0)
                rows = (plan.tags & np.uint64(bits)) != 0
            elif key == "keep":
                rows = plan.keep != K.K_NO if _yes(val) else (plan.keep == K.K_NO) & rep
            else:
                low = t.lower()
                rows = np.fromiter((low in p.lower() for p in plan.paths), bool, n)
        except (ValueError, KeyError, AssertionError):
            continue
        if rows is not None:
            if neg:
                rows = ~rows
            rmask &= rows
            hit = np.zeros(ng, bool)
            if n and ng:
                hit = np.bincount(plan.group_of[rows], minlength=ng)[:ng] > 0
            gmask &= hit
        elif grp is not None:
            gmask &= ~grp if neg else grp
    return gmask, rmask & gmask[plan.group_of] if n else rmask


_YES_NO = ("yes", "y", "true", "1", "是", "on", "no", "n", "false", "0", "否", "off")  # i18n: ignore
_SOURCES = ("manual", "fallback", "safety_net", "safety", "pair", "preset", "risk", "forced")
_STATES = ("in", "stale", "shared", "excluded", "no_extra", "cleaned")


def _why(term: str) -> str:
    """Why :func:`evaluate` cannot read one term ("" when it can)."""
    t = term[1:] if term.startswith("-") and len(term) > 1 else term
    key, sep, val = t.partition(":")
    key = key.lower()
    why = ""
    if not sep:
        return ""
    v = val.lower()
    if key in ("risk", "sev"):
        m = _CMP.match(v) if v else None
        level = m.group(2) if m else ""
        if not (level in K.SEV_CODE or v in RK.BY_CODE or val.upper() in RK.IDX):
            why = tr("要写风险的等级（high、medium、low、info）或风险编号")
    elif key in ("action", "avail"):
        if v.lstrip("!" if key == "action" else "") not in K.ACTION_CODE:
            why = tr("处理要写 leave、trash、hardlink、symlink 或 clone")
    elif key in ("cross", "conflict", "reviewed", "changed", "acked", "keep"):
        if v not in _YES_NO:
            why = tr("要写 yes 或 no")
    elif key in ("in", "name", "ext", "vol", "fs", "class"):
        if not val.strip():
            why = tr("冒号后面是空的")
    elif key in ("size", "copies"):
        m = _CMP.match(v) if v else None
        try:
            if m is None:
                raise ValueError(v)
            _num(m.group(2)) if key == "size" else float(m.group(2))
        except ValueError:
            why = tr("要写数值，例如 size:>100MB、copies:>=3")
    elif key == "mtime":
        m = _CMP.match(v) if v else None
        try:
            if m is None:
                raise ValueError(v)
            _date(m.group(2))
        except ValueError:
            why = tr("要写日期，例如 mtime:<2020-01-01")
    elif key == "source":
        if not (v in _SOURCES or (v.startswith("rule:") and v[5:].isdigit())):
            why = tr("来源要写 manual、rule:3、fallback、safety_net、pair、preset、risk 或 forced")
    elif key == "state":
        if v not in _STATES:
            why = tr("状态要写 in、stale、shared、excluded、no_extra 或 cleaned")
    elif key == "tag":
        bad = [x for x in v.split(",") if x not in K.TAG_CODE]
        if bad or not v:
            why = tr("不认识的标签：{tags}", tags=", ".join(bad) or "—")
    return why


def problems(query: str) -> list[str]:
    """The terms of a filter that :func:`evaluate` cannot read, in the user's words: such a filter
    matches nothing, and the interface says why under it."""
    out: list[str] = []
    for term in split_terms(query or ""):
        why = _why(term)
        if why:
            out.append(tr("看不懂「{term}」：{why}", term=term, why=why))
    return out


def _hexd(g: object) -> str:
    d = getattr(g, "digest", None) or b""
    return bytes(d).hex()


def changed_since_review(plan: CleanupPlan) -> np.ndarray:
    """Groups reviewed in an earlier version whose decisions changed since (``changed:yes``)."""
    out = np.zeros(len(plan.groups), bool)
    seen = plan.review_marks
    for gi in range(len(plan.groups)):
        mark = seen.get(gi)
        if mark is not None and mark != group_signature(plan, gi):
            out[gi] = True
    return out


def group_signature(plan: CleanupPlan, gi: int) -> int:
    g = plan.groups[gi]
    sl = slice(g.start, g.start + g.count)
    blob = plan.keep[sl].tobytes() + plan.act[sl].tobytes() + plan.target[sl].tobytes()
    return hash(blob)

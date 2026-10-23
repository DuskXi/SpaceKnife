"""What the interface reads of a cleanup plan (docs/dupes.md §16.7, §16.13): pages, details, the
summary and its risk buckets, facets, folder pairs, the volume matrix, exports.

Every text is made here, in the user's words, with the evidence quoted; only for what is shown.
"""

from __future__ import annotations

import csv
import io
import json
import os
import random
import sys
import time
from datetime import UTC, datetime
from typing import Any

import numpy as np

from spaceknife_protocol import dupes_cleanup as pc

from ... import fmt
from ...i18n import tr
from . import codes as K
from . import risks as RK
from .plan import G_IN, G_NAMES, CleanupPlan, _hex
from .policy import Rule
from .query import evaluate

MAC = sys.platform == "darwin"


def sev_name(s: int) -> str:
    return K.SEVERITIES[min(max(int(s), 0), 5)]


def rule_label(rule: Rule) -> str:
    if rule.label:
        return rule.label
    if rule.label_key:
        txt = K.TEXT[rule.label_key]
        if "{vol}" in txt:
            return txt.format(vol=rule.when.volumes[0] if rule.when.volumes else rule.target_under)
        return txt
    sel = rule.when
    bits: list[str] = []
    if sel.under:
        bits.append(tr("在 {list} 里", list=K.TEXT["list_sep"].join(sel.under)))
    if sel.not_under:
        bits.append(tr("不在 {list} 里", list=K.TEXT["list_sep"].join(sel.not_under)))
    if sel.volumes:
        bits.append(tr("卷 {list}", list=K.TEXT["list_sep"].join(sel.volumes)))
    if sel.ext:
        bits.append(tr("扩展名 {list}", list=K.TEXT["list_sep"].join(sel.ext)))
    if sel.classes:
        bits.append(tr("类别 {list}", list=K.TEXT["list_sep"].join(sel.classes)))
    if sel.name_glob:
        bits.append(tr("名字 {pat}", pat=sel.name_glob))
    if sel.tags:
        bits.append(tr("标签 {list}", list=K.TEXT["list_sep"].join(sel.tags)))
    if sel.can:
        acts = K.TEXT["list_sep"].join(K.act_label(K.ACTION_CODE[a], mac=MAC) for a in sel.can)
        bits.append(tr("能{act}的", act=acts))
    if sel.risk_at_least:
        bits.append(
            tr("有{sev}风险的", sev=K.TEXT[f"sev_{sel.risk_at_least}"] if sel.risk_at_least != "none" else "")
        )
    if sel.risk_any:
        bits.append(tr("有风险 {list} 的", list=K.TEXT["list_sep"].join(sel.risk_any)))
    where = K.TEXT["comma"].join(bits) or tr("全部")
    if rule.kind == "keep":
        eff = {
            "keep_all": tr("全部保留"),
            "prefer": tr("优先保留 {list}", list=K.TEXT["list_sep"].join(rule.folders)),
            "avoid": tr("尽量不留在 {list}", list=K.TEXT["list_sep"].join(rule.folders) or where),
            "per_folder": tr("每个文件夹各留一份"),
            "keep_n": tr("至少留 {n} 份", n=rule.n),
            "order": tr(
                "补位时按 {list}", list=K.TEXT["list_sep"].join(K.TEXT[f"crit_{c}"] for c in rule.criteria)
            ),
        }.get(rule.effect, rule.effect)
        return f"{where} → {eff}" if rule.effect not in ("prefer",) or sel.empty() is False else eff
    return f"{where} → {K.act_label(K.ACTION_CODE.get(rule.effect, 1), mac=MAC)}"


def _rule(plan: CleanupPlan, rid: int) -> Rule | None:
    return plan.policy.rule(rid)


def keep_source(plan: CleanupPlan, r: int) -> str:
    src = int(plan.ksrc[r])
    ref = int(plan.kref[r])
    gi = int(plan.group_of[r])
    if src == K.SRC_FORCED_ALIAS:
        units = plan.gunits[gi]
        u = next((x for x in units if r in x.rows), None)
        return K.TEXT["src_forced_alias"].format(path=plan.paths[r], n=len(u.rows) if u else 2)
    if src in (K.SRC_PREFER, K.SRC_AVOID, K.SRC_KEEP_ALL, K.SRC_PER_FOLDER):
        rule = _rule(plan, ref)
        folder = ""
        if rule is not None:
            from ..keep import _prefer_rank

            folders = rule.folders or rule.when.under
            rank = _prefer_rank(plan.paths[r], folders) if folders else 0
            folder = folders[rank] if folders and rank < len(folders) else (folders[0] if folders else "")
            if src == K.SRC_PREFER:
                return K.TEXT["src_prefer"].format(folder=folder, rank=rank + 1)
        if src == K.SRC_AVOID:
            return K.TEXT["src_avoid"].format(folder=folder or (rule_label(rule) if rule else ""))
        if src == K.SRC_KEEP_ALL:
            return K.TEXT["src_keep_all"].format(folder=folder or (rule_label(rule) if rule else ""))
        return K.TEXT["src_per_folder"]
    if src == K.SRC_PER_VOLUME:
        v = int(plan.vol[r])
        return K.TEXT["src_per_volume"].format(vol=plan.vols[v].name if v < len(plan.vols) else v)
    if src == K.SRC_KEEP_N:
        return K.TEXT["src_keep_n"].format(n=ref)
    if src == K.SRC_CRITERION:
        crit = plan.policy.criteria[ref] if ref < len(plan.policy.criteria) else "oldest_created"
        return K.TEXT["src_criterion"].format(criterion=K.TEXT[f"crit_{crit}"])
    if src == K.SRC_PAIR_SIDE and ref < len(plan.policy.pairs):
        pr = plan.policy.pairs[ref]
        return K.TEXT["src_pair_side"].format(side=pr.a if pr.keep == "a" else pr.b, a=pr.a, b=pr.b)
    name = K.SRC_NAMES.get(src)
    return K.TEXT[f"src_{name}"] if name and f"src_{name}" in K.TEXT else ""


def action_source(plan: CleanupPlan, r: int) -> str:
    src = int(plan.asrc[r])
    act = int(plan.act[r])
    ref = int(plan.aref[r])
    label = K.act_label(act, mac=MAC)
    if src == K.SRC_ACTION_MANUAL:
        return K.TEXT["src_action_manual"].format(act=label)
    if src == K.SRC_ACTION_RULE:
        rule = _rule(plan, ref)
        idx = next((i + 1 for i, x in enumerate(plan.policy.action_rules) if x.id == ref), ref)
        return K.TEXT["src_action_rule"].format(n=idx, label=rule_label(rule) if rule else "")
    if src == K.SRC_ACTION_FALLBACK:
        want = int(plan.want[r])
        why = availability_text(plan, r, int(plan.wna[r]))
        return K.TEXT["src_action_fallback"].format(want=K.act_label(want, mac=MAC), why=why, act=label)
    if src == K.SRC_ACTION_PAIR:
        return K.TEXT["src_action_pair"].format(act=label)
    if src == K.SRC_ACTION_RISK:
        idx = next((i + 1 for i, x in enumerate(plan.policy.action_rules) if x.id == ref), ref)
        rule = _rule(plan, ref)
        what = (
            K.TEXT[f"sev_{rule.when.risk_at_least}"] + tr("风险")
            if rule is not None and rule.when.risk_at_least
            else (", ".join(rule.when.risk_any) if rule is not None else "")
        )
        return K.TEXT["src_action_risk"].format(n=idx, risk=what)
    if src == K.SRC_ACTION_PRESET:
        return K.TEXT["src_action_preset"].format(preset=K.TEXT[f"preset_{plan.policy.preset}"], act=label)
    return ""


def availability_text(plan: CleanupPlan, r: int, code: int) -> str:
    if code == K.NA_OK:
        return ""
    name = K.NA_NAMES.get(code, "keeper")
    v = int(plan.vol[r])
    fs = plan.vols[v].fs if v < len(plan.vols) else ""
    tmpl = K.TEXT[f"na_{name}"]
    fields: dict[str, Any] = {
        "fs": fs.upper() or tr("这个文件系统"),
        "limit": plan.vols[v].link_max if v < len(plan.vols) else 0,
    }
    if "{provider}" in tmpl:
        f = plan.tagx.dir_facts(int(plan.scan[r]), plan.dirs[int(plan.dir_of[r])])
        fields["provider"] = f.sync or tr("同步盘")
    if "{why}" in tmpl:
        fields["why"] = keep_source(plan, r)
    return tmpl.format(**fields)


def _ago(ns: int) -> str:
    s = max(0.0, time.time() - ns / 1e9)
    if s < 3600:
        return RK.TEXT["a_moment"]
    if s < 86400 * 2:
        return RK.TEXT["hours_ago"].format(n=int(s // 3600))
    return RK.TEXT["days_ago"].format(n=int(s // 86400))


def _date(ns: int) -> str:
    return datetime.fromtimestamp(ns / 1e9, tz=UTC).astimezone().strftime("%Y-%m-%d %H:%M") if ns else "—"


def _where(plan: CleanupPlan, r: int) -> str:
    t = int(plan.tags[r])
    f = plan.tagx.dir_facts(int(plan.scan[r]), plan.dirs[int(plan.dir_of[r])])
    if f.app:
        return K.TEXT["quoted"].format(name=f.app)
    for bit, key in (
        (K.T_DOWNLOADS, "where_downloads"),
        (K.T_TEMP, "where_temp"),
        (K.T_CACHE, "where_cache"),
        (K.T_GAME, "where_game"),
        (K.T_PROGRAM, "where_program"),
        (K.T_APP, "where_app"),
    ):
        if t & bit:
            return RK.TEXT[key]
    return plan.dirs[int(plan.dir_of[r])]


def risk_text(plan: CleanupPlan, r: int, rid: str) -> str:
    d = RK.BY_ID[rid]
    code = d.code
    act = int(plan.act[r])
    ref = int(plan.target[r]) if plan.target[r] >= 0 else int(plan.refk[r])

    def vol_name(row: int) -> str:
        return plan.vols[int(plan.vol[row])].name if row >= 0 and int(plan.vol[row]) < len(plan.vols) else ""

    probe = plan.probes.get(r)
    kprobe = plan.probes.get(ref) if ref >= 0 else None
    facts = plan.tagx.dir_facts(int(plan.scan[r]), plan.dirs[int(plan.dir_of[r])])
    kfacts = plan.tagx.dir_facts(int(plan.scan[ref]), plan.dirs[int(plan.dir_of[ref])]) if ref >= 0 else facts
    fields: dict[str, Any] = {
        "vol": vol_name(r),
        "kvol": vol_name(ref),
        "keeper": plan.paths[ref] if ref >= 0 else "",
        "folder": plan.dirs[int(plan.dir_of[r])],
        "provider": facts.sync or tr("同步盘"),
        "root": facts.sync_root,
        "kprovider": kfacts.sync or tr("同步盘"),
        "repo": facts.repo,
        "app": facts.app or _where(plan, r),
        "where": _where(plan, ref if code in ("keeper_volatile", "keeper_program") else r),
        "lib": facts.media_lib or tr("媒体库"),
        "marker": facts.project or "package.json",
        "name": plan.names[r],
        "kname": plan.names[ref] if ref >= 0 else "",
        "owner": probe.owner if probe and probe.ok else K.TEXT["unknown_owner"],
        "kowner": kprobe.owner if kprobe and kprobe.ok else K.TEXT["unknown_owner"],
        "ago": _ago(max(int(plan.m["mtime"][ref if code == "keeper_recent" else r]), 0)),
        "mtime": _date(int(plan.m["mtime"][r])),
        "kmtime": _date(int(plan.m["mtime"][ref])) if ref >= 0 else "",
        "cls": "",
        "n": 0,
        "size": fmt.size(int(plan.gsize[int(plan.group_of[r])]) * 2),
        "s": max(1, int(2 * int(plan.gsize[int(plan.group_of[r])]) / 400e6)),
        "before": 0,
        "after": 0,
        "links": 0,
        "acl": "",
        "names": "",
    }
    if code == "backup_like":
        fields["folder"] = facts.backup or plan.dirs[int(plan.dir_of[r])]
        why = {K.A_TRASH: "trash", K.A_HARDLINK: "hardlink", K.A_SYMLINK: "symlink", K.A_CLONE: "clone"}.get(
            act, "trash"
        )
        fields["why"] = RK.TEXT[f"m_backup_why_{why}"]
    if code == "redundancy_drop":
        gi = int(plan.group_of[r])
        units = plan.gunits[gi]
        links = sum(1 for u in units if plan.act[u.rep] in (K.A_HARDLINK, K.A_SYMLINK))
        gone = sum(1 for u in units if plan.act[u.rep] == K.A_TRASH)
        fields["before"] = len(units)
        fields["after"] = len(units) - links - gone
        fields["links"] = links
    if code in ("extra_streams", "streams_lost") and probe and probe.ok:
        names = [nm for nm, _ in probe.streams]
        shown = K.TEXT["list_sep"].join(names[:3])
        fields["names"] = RK.TEXT["names_more"].format(names=shown, n=len(names)) if len(names) > 3 else shown
        fields["size"] = fmt.size(sum(sz for _, sz in probe.streams))
    if code == "owner_acl":
        dif = int(plan.diff[r]) if hasattr(plan, "diff") else 0
        if dif & K.D_UNKNOWN:
            return RK.TEXT["m_owner_acl_unknown"]
        fields["acl"] = (
            RK.TEXT["m_owner_acl_acl"] if dif & (K.D_ACL_EXPLICIT | K.D_ACL_INHERITED | K.D_MODE) else ""
        )
    if code == "edits_shared":
        t = int(plan.tags[r])
        fields["cls"] = (
            tr("文档、表格或代码")
            if t & K.T_DOCLIKE
            else tr("就地写入的格式")
            if t & K.T_INPLACE
            else tr("图片、音频或视频")
            if t & K.T_MEDIA
            else tr("压缩包或安装包")
            if t & K.T_ARCHIVE
            else tr("文件")
        )
    if code == "many_names":
        fields["n"] = kprobe.nlink if kprobe and kprobe.ok else 0
    if code == "many_links_to_keeper":
        fields["n"] = int(np.count_nonzero((plan.act == K.A_SYMLINK) & (plan.target == plan.target[r])))
    if code == "link_style":
        text = plan.symlink_text(r, ref) if ref >= 0 else ""
        return RK.TEXT["m_link_style_absolute"] if os.path.isabs(text) else RK.TEXT["m_link_style_relative"]
    tmpl = RK.TEXT[f"m_{code}"]
    try:
        return tmpl.format(**fields)
    except (KeyError, IndexError):
        return tmpl


def copy_model(plan: CleanupPlan, r: int, scan_ids: list[str], *, risks: bool = True) -> pc.CleanupCopy:
    m = plan.m[r]
    v = int(m["vol"])
    vc = plan.vols[v] if v < len(plan.vols) else None
    keep = "locked" if plan.keep[r] == K.K_LOCKED else ("keep" if plan.keep[r] == K.K_KEEP else "none")
    act = int(plan.act[r])
    rep = bool(plan.is_rep[r])
    view_of = ""
    if not rep:
        u = plan.unit_of_row(r)
        if u is not None:
            view_of = plan.paths[u[1].rep]
    target = int(plan.target[r])
    now, later = plan.freed()
    av: dict[str, str] = {}
    if keep == "none" and rep:
        for code, name in K.ACTION_NAME.items():
            av[name] = availability_text(plan, r, int(plan.na[r, code - 1])) if code != K.A_LEAVE else ""
    acked = plan.acks
    rl: list[pc.CleanupRisk] = []
    if risks and rep:
        for rid, sev in plan.risks_of(r):
            d = RK.BY_ID[rid]
            a = acked.get(rid)
            rl.append(
                pc.CleanupRisk(
                    id=rid,
                    code=d.code,
                    severity=sev_name(sev),  # type: ignore[arg-type]
                    title=RK.TEXT[f"t_{d.code}"],
                    text=risk_text(plan, r, rid),
                    acked=bool(a and int(plan.keys[r]) in a.keys),
                )
            )
    src = keep_source(plan, r) if keep != "none" or not rep else action_source(plan, r)
    manual = bool(int(plan.ksrc[r]) in K.MANUAL_SOURCES or int(plan.asrc[r]) == K.SRC_ACTION_MANUAL)
    tags = [
        name
        for bit, name in K.TAG_NAMES.items()
        if int(plan.tags[r]) & bit and bit not in (K.T_RECENT7, K.T_VIEW)
    ]
    return pc.CleanupCopy(
        row=r,
        scan_id=scan_ids[int(m["scan"])] if int(m["scan"]) < len(scan_ids) else "",
        node=int(m["node"]),
        path=plan.paths[r],
        volume=vc.name if vc else "",
        fs=vc.fs if vc else "",
        keep=keep,  # type: ignore[arg-type]
        action=K.ACTION_NAME.get(act) if keep == "none" and rep else None,
        target=plan.paths[target] if target >= 0 else "",
        symlink_text=plan.symlink_text(r, target) if act == K.A_SYMLINK and target >= 0 else "",
        source=src,
        source_code=K.SRC_NAMES.get(int(plan.ksrc[r]) if keep != "none" else int(plan.asrc[r]), ""),
        manual=manual,
        available=av,
        risks=rl,
        severity=sev_name(int(plan.sev[r])),  # type: ignore[arg-type]
        freed_now=int(now[r]),
        freed_later=int(later[r]),
        alloc=int(plan.alloc[r]),
        mtime=int(m["mtime"]) // 1_000_000_000,
        btime=int(plan.bt[r]) // 1_000_000_000,
        view_of=view_of,
        tags=tags,
    )


def out_reason(plan: CleanupPlan, gi: int) -> str:
    st = int(plan.gstate[gi])
    return K.TEXT[f"out_{G_NAMES[st]}"] if st != G_IN else ""


def keep_line(plan: CleanupPlan, gi: int) -> str:
    units = plan.gunits[gi]
    keepers = [u for u in units if plan.keep[u.rep] != K.K_NO]
    if int(plan.gstate[gi]) != G_IN:
        return out_reason(plan, gi)
    if not keepers:
        return ""
    if len(keepers) == 1:
        return K.TEXT["keep_line"].format(how=keep_source(plan, keepers[0].rep), path=keepers[0].path)
    hows = sorted({keep_source(plan, u.rep) for u in keepers})
    return K.TEXT["keep_line_many"].format(n=len(keepers), how=K.TEXT["semi"].join(hows))


def group_model(plan: CleanupPlan, gi: int, scan_ids: list[str], *, limit: int = 8) -> pc.CleanupGroup:
    g = plan.groups[gi]
    units = plan.gunits[gi]
    rows = list(range(g.start, g.start + g.count))
    order = sorted(rows, key=lambda r: (plan.keep[r] == K.K_NO, not plan.is_rep[r], plan.paths[r].lower()))
    acts: dict[str, int] = {}
    for u in units:
        if plan.keep[u.rep] != K.K_NO:
            acts["keep"] = acts.get("keep", 0) + 1
        else:
            name = K.ACTION_NAME.get(int(plan.act[u.rep]), "leave")
            acts[name] = acts.get(name, 0) + 1
    now, later = plan.freed()
    sl = slice(g.start, g.start + g.count)
    srcs = set()
    for u in units:
        r = u.rep
        if int(plan.ksrc[r]) in K.MANUAL_SOURCES or int(plan.asrc[r]) == K.SRC_ACTION_MANUAL:
            srcs.add("manual")
        elif (
            int(plan.ksrc[r]) in (K.SRC_PAIR_SIDE, K.SRC_PAIR_BOTH) or int(plan.asrc[r]) == K.SRC_ACTION_PAIR
        ):
            srcs.add("pair")
        elif int(plan.ksrc[r]) == K.SRC_SAFETY_NET:
            srcs.add("safety")
        else:
            srcs.add("rule")
    source = srcs.pop() if len(srcs) == 1 else ("mixed" if srcs else "rule")
    d = _hex(g.digest)
    risk_count = int(np.count_nonzero((plan.risk_rows >= g.start) & (plan.risk_rows < g.start + g.count)))
    changed = plan.changed_marks()
    return pc.CleanupGroup(
        id=gi,
        digest16=d[:16],
        name=os.path.basename(plan.paths[g.start]),
        names=g.names,
        size=g.size,
        copies=len(units),
        keepers=sum(1 for u in units if plan.keep[u.rep] != K.K_NO),
        actions=acts,
        freed_now=int(now[sl].sum()),
        freed_later=int(later[sl].sum()),
        severity=sev_name(int(plan.gsev[gi]) if gi < len(plan.gsev) else 0),  # type: ignore[arg-type]
        risk_count=risk_count,
        source=source,
        reviewed=d in plan.policy.reviewed,
        changed_since_review=bool(changed[gi]) if gi < len(changed) else False,
        state=G_NAMES[int(plan.gstate[gi])],  # type: ignore[arg-type]
        keep_line=keep_line(plan, gi),
        cross=len(g.volumes) > 1,
        volumes=[plan.vols[v].name for v in g.volumes if v < len(plan.vols)],
        conflict=bool(plan.gconflict[gi]),
        only_avoided=bool(plan.gonly_avoided[gi]),
        safety_net=bool(plan.gsafety[gi]),
        members=[copy_model(plan, r, scan_ids) for r in order[:limit]],
        total_members=g.count,
    )


def _sorted_groups(plan: CleanupPlan, gmask: np.ndarray, sort: str) -> list[int]:
    ids = np.flatnonzero(gmask)
    if not len(ids):
        return []
    now, later = plan.freed()
    ng = len(plan.groups)
    freed = np.bincount(plan.group_of, weights=(now + later).astype(np.float64), minlength=ng)[:ng]
    if sort == "risk":
        key = np.lexsort((ids, -freed[ids], -plan.gsev[ids].astype(np.int64)))
    elif sort == "size":
        key = np.lexsort((ids, -plan.gsize[ids]))
    elif sort == "copies":
        units = np.array([len(u) for u in plan.gunits], np.int64)
        key = np.lexsort((ids, -plan.gsize[ids], -units[ids]))
    elif sort == "path":
        names = [plan.paths[plan.groups[i].start].lower() for i in ids.tolist()]
        return [int(ids[i]) for i in sorted(range(len(ids)), key=lambda i: (names[i], int(ids[i])))]
    elif sort == "action":
        first = np.array(
            [
                int(plan.act[plan.groups[i].start : plan.groups[i].start + plan.groups[i].count].max())
                for i in ids.tolist()
            ]
        )
        key = np.lexsort((ids, -freed[ids], -first))
    else:
        key = np.lexsort((ids, -plan.gsize[ids], -freed[ids]))
    return [int(x) for x in ids[key].tolist()]


def groups_page(plan: CleanupPlan, p: pc.CleanupGroupsParams, scan_ids: list[str]) -> pc.CleanupGroupsPage:
    gmask, _ = evaluate(plan, p.filter)
    if not p.include_out and "state:" not in p.filter:
        gmask &= plan.gstate == G_IN
    order = _sorted_groups(plan, gmask, p.sort)
    items = [group_model(plan, gi, scan_ids) for gi in order[p.offset : p.offset + p.limit]]
    return pc.CleanupGroupsPage(version=plan.version, total=len(order), offset=p.offset, items=items)


def _facts_rows(plan: CleanupPlan, r: int, k: int) -> list[pc.CleanupFactRow]:
    if k < 0:
        return []
    out: list[pc.CleanupFactRow] = []

    def row(label: str, a: str, b: str) -> None:
        out.append(pc.CleanupFactRow(label=label, this=a, keeper=b, differs=a != b))

    unk = K.TEXT["unknown_owner"]
    a, b = plan.probes.get(r), plan.probes.get(k)

    def vn(x: int) -> str:
        vc = plan.vols[int(plan.vol[x])]
        return f"{vc.name} · {vc.fs.upper()}"

    def perm(p: Any) -> str:
        if not (p and p.ok):
            return unk
        return f"{p.mode:o}" if p.mode else f"{p.acl_explicit:016x}"[:8]

    def attrs(x: int) -> str:
        names = (
            (K.T_READONLY, tr("只读")),
            (K.T_HIDDEN, tr("隐藏")),
            (K.T_SYSTEM, tr("系统")),
            (K.T_ENCRYPTED, tr("加密")),
            (K.T_SPARSE, tr("稀疏")),
        )
        return K.TEXT["list_sep"].join(n for bit, n in names if int(plan.tags[x]) & bit) or "—"

    def motw(p: Any) -> str:
        return (tr("有") if p.bits & 1 else tr("没有")) if p and p.ok else unk

    def streams(p: Any) -> str:
        if not (p and p.ok):
            return unk
        return K.TEXT["list_sep"].join(f"{n} ({fmt.size(sz)})" for n, sz in p.streams[:4]) or "—"

    def links(p: Any) -> str:
        return str(p.nlink) if p and p.ok else unk

    row(tr("路径"), plan.paths[r], plan.paths[k])
    row(tr("卷和文件系统"), vn(r), vn(k))
    row(tr("修改时间"), _date(int(plan.m["mtime"][r])), _date(int(plan.m["mtime"][k])))
    row(tr("创建时间"), _date(int(plan.bt[r])), _date(int(plan.bt[k])))
    row(tr("所有者"), a.owner if a and a.ok else unk, b.owner if b and b.ok else unk)
    row(tr("权限"), perm(a), perm(b))
    row(tr("属性"), attrs(r), attrs(k))
    row(tr("「来自网络」标记"), motw(a), motw(b))
    row(tr("备用流和扩展属性"), streams(a), streams(b))
    row(tr("链接数"), links(a), links(b))
    return out


def _chains(plan: CleanupPlan, r: int) -> tuple[list[pc.CleanupReason], list[pc.CleanupReason]]:
    keep_chain: list[pc.CleanupReason] = []
    act_chain: list[pc.CleanupReason] = []
    gi = int(plan.group_of[r])
    d = _hex(plan.groups[gi].digest)
    P = plan.policy
    u = plan.unit_of_row(r)
    path = u[1].path if u else plan.paths[r]
    if u and u[1].forced:
        keep_chain.append(pc.CleanupReason(layer=tr("必须保留"), text=keep_source(plan, r), decided=True))
    m = P.manual.get((d, path))
    if m and m.keep:
        keep_chain.append(
            pc.CleanupReason(
                layer=tr("每份手动"),
                text=K.TEXT["src_manual_keep" if m.keep == "keep" else "src_manual_not_keep"],
                decided=int(plan.ksrc[r]) in (K.SRC_MANUAL_KEEP, K.SRC_MANUAL_NOT),
            )
        )
    mg = P.groups.get(d)
    if mg and mg.keep_set is not None:
        keep_chain.append(
            pc.CleanupReason(
                layer=tr("每组手动"),
                text=K.TEXT["src_group_keep_set"],
                decided=int(plan.ksrc[r]) == K.SRC_GROUP_SET,
            )
        )
    for rule in P.keep_rules:
        if not rule.enabled:
            continue
        hit = bool(plan._match(rule.when)[r]) if not rule.when.empty() else True
        if hit:
            decided = int(plan.kref[r]) == rule.id and int(plan.ksrc[r]) in (
                K.SRC_KEEP_ALL,
                K.SRC_PREFER,
                K.SRC_AVOID,
                K.SRC_PER_FOLDER,
                K.SRC_KEEP_N,
            )
            keep_chain.append(pc.CleanupReason(layer=tr("保留规则"), text=rule_label(rule), decided=decided))
    if plan.policy.folder_consistent:
        keep_chain.append(
            pc.CleanupReason(
                layer=tr("文件夹一致"), text=K.TEXT["src_folder"], decided=int(plan.ksrc[r]) == K.SRC_FOLDER
            )
        )
    crits = "→".join(K.TEXT[f"crit_{c}"] for c in P.criteria)
    keep_chain.append(
        pc.CleanupReason(
            layer=tr("基本判据"),
            text=crits,
            decided=int(plan.ksrc[r]) in (K.SRC_CRITERION, K.SRC_TIE, K.SRC_PER_VOLUME, K.SRC_NOT_KEPT),
        )
    )
    final = keep_source(plan, r) if plan.keep[r] != K.K_NO else K.TEXT["src_not_kept"]
    keep_chain.append(pc.CleanupReason(layer=tr("结果"), text=final, decided=True))
    if plan.keep[r] == K.K_NO and plan.is_rep[r]:
        if m and m.action:
            act_chain.append(
                pc.CleanupReason(
                    layer=tr("每份手动"),
                    text=K.TEXT["src_action_manual"].format(
                        act=K.act_label(K.ACTION_CODE.get(m.action, 1), mac=MAC)
                    ),
                    decided=int(plan.asrc[r]) == K.SRC_ACTION_MANUAL,
                )
            )
        for i, rule in enumerate(P.action_rules):
            if not rule.enabled:
                continue
            hit = bool(plan._match(rule.when)[r])
            if hit:
                act_chain.append(
                    pc.CleanupReason(
                        layer=tr("处理规则 {n}", n=i + 1),
                        text=rule_label(rule),
                        decided=int(plan.aref[r]) == rule.id
                        and int(plan.asrc[r])
                        in (K.SRC_ACTION_RULE, K.SRC_ACTION_FALLBACK, K.SRC_ACTION_RISK),
                    )
                )
        act_chain.append(pc.CleanupReason(layer=tr("结果"), text=action_source(plan, r), decided=True))
    return keep_chain, act_chain


def group_detail(plan: CleanupPlan, gi: int, scan_ids: list[str]) -> pc.CleanupGroupDetail:
    g = plan.groups[gi]
    copies: list[pc.CleanupCopyDetail] = []
    rows = sorted(
        range(g.start, g.start + g.count),
        key=lambda r: (plan.keep[r] == K.K_NO, not plan.is_rep[r], plan.paths[r].lower()),
    )
    for r in rows:
        k = int(plan.target[r]) if plan.target[r] >= 0 else int(plan.refk[r])
        kc, ac = _chains(plan, r)
        copies.append(
            pc.CleanupCopyDetail(
                item=copy_model(plan, r, scan_ids),
                keeper=plan.paths[k] if k >= 0 else "",
                keep_chain=kc,
                action_chain=ac,
                facts=_facts_rows(plan, r, k) if plan.keep[r] == K.K_NO else [],
            )
        )
    return pc.CleanupGroupDetail(
        version=plan.version,
        group=group_model(plan, gi, scan_ids, limit=0),
        copies=copies,
        out_reason=out_reason(plan, gi),
    )


def bucket(plan: CleanupPlan, i: int) -> pc.CleanupBucket:
    d = RK.CATALOG[i]
    on = plan.risk_ids == i
    rows = plan.risk_rows[on]
    sev = int(plan.risk_sev[on].max()) if len(rows) else 0
    groups = len(np.unique(plan.group_of[rows])) if len(rows) else 0
    un = plan.unacked().get(d.id, 0) if sev >= K.SEV_HIGH else 0
    ack = plan.acks.get(d.id)
    example = risk_text(plan, int(rows[0]), d.id) if len(rows) else ""
    return pc.CleanupBucket(
        id=d.id,
        code=d.code,
        severity=sev_name(sev),  # type: ignore[arg-type]
        title=RK.TEXT[f"t_{d.code}"],
        copies=len(rows),
        groups=int(groups),
        bytes=int(plan.alloc[rows].sum()) if len(rows) else 0,
        acked=bool(ack) and un == 0,
        unacked=un,
        example=example,
    )


def summary(
    plan: CleanupPlan, gates: list[str], *, recovery: dict[int, str] | None = None
) -> pc.CleanupSummary:
    now, later = plan.freed()
    reps = plan.is_rep & (plan.keep == K.K_NO)
    by_action: list[pc.CleanupActionTotal] = []
    for code, name in K.ACTION_NAME.items():
        sel = reps & (plan.act == code)
        if not sel.any():
            continue
        by_action.append(
            pc.CleanupActionTotal(
                action=name,
                copies=int(sel.sum()),
                groups=len(np.unique(plan.group_of[sel])),
                freed_now=int(now[sel].sum()),
                freed_later=int(later[sel].sum()),
            )
        )
    by_volume: list[pc.CleanupVolumeTotal] = []
    for vc in plan.vols:
        sel = reps & (plan.vol == vc.index) & (plan.act > K.A_LEAVE)
        if not sel.any():
            continue
        by_volume.append(
            pc.CleanupVolumeTotal(
                volume=vc.name,
                fs=vc.fs,
                freed_now=int(now[sel].sum()),
                freed_later=int(later[sel].sum()),
                copies=int(sel.sum()),
                recovery=(recovery or {}).get(vc.index, ""),
                trash="" if vc.trash else (vc.trash_reason or K.TEXT["no_trash_recovery"]),
                snapshots_note=RK.TEXT["m_snapshot_held"]
                if vc.fs == "btrfs" and np.any(sel & (plan.act == K.A_CLONE))
                else "",
            )
        )
    sev_counts = {
        s: int(np.count_nonzero(reps & (plan.act > K.A_LEAVE) & (plan.sev == i)))
        for i, s in enumerate(K.SEVERITIES)
    }
    buckets = [bucket(plan, int(i)) for i in np.unique(plan.risk_ids).tolist()]
    buckets.sort(key=lambda b: (-K.SEV_CODE[b.severity], -b.copies, b.id))
    not_done: list[pc.CleanupNotDone] = []
    for st in (1, 2, 3, 5):
        c = int(np.count_nonzero(plan.gstate == st))
        if c:
            not_done.append(
                pc.CleanupNotDone(
                    reason=G_NAMES[st], text=K.TEXT[f"out_{G_NAMES[st]}"], count=c, unit="groups"
                )
            )
    forced = int(
        np.count_nonzero(plan.is_rep & (plan.keep == K.K_LOCKED) & (plan.gstate[plan.group_of] == G_IN))
    )
    if forced:
        not_done.append(pc.CleanupNotDone(reason="forced", text=tr("必须保留"), count=forced))
    for code, why in (
        (K.NA_IN_SYNC_ROOT, "in_sync_root"),
        (K.NA_NO_SYMLINK_RIGHT, "no_symlink_right"),
        (K.NA_OTHER_VOLUME, "other_volume"),
    ):
        c = int(np.count_nonzero(reps & (plan.asrc == K.SRC_ACTION_FALLBACK) & (plan.wna == code)))
        if c:
            not_done.append(
                pc.CleanupNotDone(
                    reason=why, text=K.TEXT[f"na_{why}"].format(provider="", fs="", limit=0), count=c
                )
            )
    c = int(np.count_nonzero(reps & (plan.asrc == K.SRC_ACTION_RISK)))
    if c:
        not_done.append(pc.CleanupNotDone(reason="risk_rule", text=tr("高风险 → 不动"), count=c))
    c = int(np.count_nonzero(plan.gsafety))
    if c:
        not_done.append(pc.CleanupNotDone(reason="safety_net", text=tr("兜底保留"), count=c, unit="groups"))
    c = int(np.count_nonzero(plan.gconflict))
    if c:
        not_done.append(pc.CleanupNotDone(reason="conflict", text=tr("规则冲突"), count=c, unit="groups"))
    c = int(np.count_nonzero(plan.gonly_avoided))
    if c:
        not_done.append(
            pc.CleanupNotDone(reason="only_avoided", text=tr("只能留在不想留的地方"), count=c, unit="groups")
        )
    in_plan = int(np.count_nonzero(plan.gstate == G_IN))
    reviewed = sum(
        1 for g in plan.groups if _hex(g.digest) in plan.policy.reviewed and plan.gstate[g.id] == G_IN
    )
    sel = reps & (plan.act > K.A_LEAVE)
    compare = int(np.count_nonzero(sel & (plan.act == K.A_CLONE) & (sys.platform == "win32")))
    compare_bytes = (
        int((2 * plan.gsize[plan.group_of][sel & (plan.act == K.A_CLONE)]).sum()) if compare else 0
    )
    return pc.CleanupSummary(
        version=plan.version,
        groups=len(np.unique(plan.group_of[sel])) if sel.any() else 0,
        copies=int(sel.sum()),
        freed_now=int(now.sum()),
        freed_later=int(later.sum()),
        by_action=by_action,
        by_volume=by_volume,
        by_severity=sev_counts,
        buckets=buckets,
        not_done=not_done,
        gates=gates,
        reviewed=reviewed,
        in_plan=in_plan,
        compare_bytes=compare_bytes,
    )


def rule_view(plan: CleanupPlan, rule: Rule, hits: np.ndarray | None = None) -> pc.CleanupRuleView:
    from dataclasses import asdict

    if rule.kind == "action":
        sel = (
            (plan.asrc == K.SRC_ACTION_RULE)
            | (plan.asrc == K.SRC_ACTION_FALLBACK)
            | (plan.asrc == K.SRC_ACTION_RISK)
        )
        mine = sel & (plan.aref == rule.id) & plan.is_rep
    else:
        mine = (
            (plan.kref == rule.id)
            & np.isin(plan.ksrc, (K.SRC_KEEP_ALL, K.SRC_PREFER, K.SRC_AVOID, K.SRC_PER_FOLDER, K.SRC_KEEP_N))
            & plan.is_rep
        )
    return pc.CleanupRuleView(
        id=rule.id,
        kind=rule.kind,
        effect=rule.effect,
        enabled=rule.enabled,
        label=rule_label(rule),
        when={k: v for k, v in asdict(rule.when).items() if v not in (None, [], "")},
        folders=list(rule.folders),
        n=rule.n,
        criteria=list(rule.criteria),
        fallback=list(rule.fallback),
        target_under=rule.target_under,
        risky=rule.when.risky,
        hits=int(np.count_nonzero(mine)),
        bytes=int(plan.alloc[mine].sum()),
        preset=bool(rule.label_key),
    )


def facets(plan: CleanupPlan, query: str) -> pc.CleanupFacets:
    gmask, rmask = evaluate(plan, query)
    reps = plan.is_rep & rmask & (plan.gstate[plan.group_of] == G_IN)
    out_act: dict[str, int] = {}
    for code, name in K.ACTION_NAME.items():
        out_act[name] = int(np.count_nonzero(reps & (plan.keep == K.K_NO) & (plan.act == code)))
    out_act["keep"] = int(np.count_nonzero(reps & (plan.keep != K.K_NO)))
    sev = {s: int(np.count_nonzero(reps & (plan.sev == i))) for i, s in enumerate(K.SEVERITIES) if i}
    risk: dict[str, int] = {}
    on = rmask[plan.risk_rows] if len(plan.risk_rows) else np.zeros(0, bool)
    for i, c in zip(*np.unique(plan.risk_ids[on], return_counts=True), strict=True):
        risk[RK.CATALOG[int(i)].id] = int(c)
    vol = {vc.name: int(np.count_nonzero(reps & (plan.vol == vc.index))) for vc in plan.vols}
    fs: dict[str, int] = {}
    for vc in plan.vols:
        fs[vc.fs] = fs.get(vc.fs, 0) + vol[vc.name]
    src: dict[str, int] = {}
    for r in np.flatnonzero(reps & (plan.keep == K.K_NO)).tolist():
        k = K.SRC_NAMES.get(int(plan.asrc[r]), "")
        src[k] = src.get(k, 0) + 1
    state = {G_NAMES[s]: int(np.count_nonzero(gmask & (plan.gstate == s))) for s in G_NAMES}
    tag: dict[str, int] = {}
    for bit, tname in K.TAG_NAMES.items():
        c = int(np.count_nonzero(reps & ((plan.tags & np.uint64(bit)) != 0)))
        if c:
            tag[tname] = c
    return pc.CleanupFacets(
        version=plan.version,
        total=int(np.count_nonzero(gmask & (plan.gstate == G_IN))),
        action=out_act,
        severity=sev,
        risk=risk,
        volume=vol,
        fs=fs,
        source=src,
        state=state,
        tag=tag,
    )


def pairs_page(plan: CleanupPlan, p: pc.CleanupPairsParams) -> pc.CleanupPairsPage:
    """Folder pairs: for each in-plan group, every two copies in different folders make a pair."""
    agg: dict[tuple[str, str], list[int]] = {}  # (a, b) -> [groups, bytes, kept on a, kept on b]
    files: dict[str, set[int]] = {}
    for gi, units in enumerate(plan.gunits):
        if int(plan.gstate[gi]) != G_IN or len(units) > 16:
            continue
        dirs = sorted({u.dir for u in units})
        for d in dirs:
            files.setdefault(d, set()).add(gi)
        for i in range(len(dirs)):
            for j in range(i + 1, len(dirs)):
                a, b = dirs[i], dirs[j]
                e = agg.setdefault((a, b), [0, 0, 0, 0])
                e[0] += 1
                e[1] += int(plan.gsize[gi])
                kept_a = any(u.dir == a and plan.keep[u.rep] != K.K_NO for u in units)
                kept_b = any(u.dir == b and plan.keep[u.rep] != K.K_NO for u in units)
                e[2] += int(kept_a)
                e[3] += int(kept_b)
    merged: dict[tuple[str, str], int] = {}
    if p.merge:
        # fold child pairs whose folders have the same name into their parents' pair
        changed = True
        while changed:
            changed = False
            parents: dict[tuple[str, str], list[tuple[str, str]]] = {}
            for a, b in agg:
                if os.path.basename(a).lower() == os.path.basename(b).lower() and os.path.dirname(a) != a:
                    parents.setdefault((os.path.dirname(a), os.path.dirname(b)), []).append((a, b))
            for parent, kids in parents.items():
                if len(kids) < 2 or parent[0] == parent[1]:
                    continue
                tot = agg.setdefault(parent, [0, 0, 0, 0])
                for kid in kids:
                    e = agg.pop(kid)
                    for k in range(4):
                        tot[k] += e[k]
                    merged[parent] = merged.get(parent, 0) + 1 + merged.pop(kid, 0)
                changed = True
    rows = sorted(agg.items(), key=lambda kv: (-kv[1][1], kv[0]))
    rules = {(x.a, x.b): x for x in plan.policy.pairs}
    items: list[pc.CleanupPairRow] = []
    for (a, b), (ng, nb, ka, kb) in rows[p.offset : p.offset + p.limit]:
        fa = len(files.get(a, ())) or ng
        fb = len(files.get(b, ())) or ng
        keep = (
            "both"
            if ka == ng and kb == ng
            else "a"
            if ka >= ng and kb == 0
            else "b"
            if kb >= ng and ka == 0
            else ("mixed" if ka or kb else "none")
        )
        rule = rules.get((a, b)) or rules.get((b, a))
        items.append(
            pc.CleanupPairRow(
                a=a,
                b=b,
                groups=ng,
                bytes=nb,
                a_in_b=min(1.0, ng / max(1, fa)),
                b_in_a=min(1.0, ng / max(1, fb)),
                keep=keep,
                rule=pc.CleanupPairView(
                    a=rule.a, b=rule.b, keep=rule.keep, other_action=rule.other_action, auto=rule.auto
                )
                if rule
                else None,
                merged=merged.get((a, b), 0),
            )
        )
    return pc.CleanupPairsPage(version=plan.version, total=len(rows), items=items)


def volume_matrix(plan: CleanupPlan) -> pc.CleanupVolumeMatrix:
    cells: dict[tuple[int, int], list[int]] = {}
    for gi, g in enumerate(plan.groups):
        if int(plan.gstate[gi]) != G_IN or len(g.volumes) < 2:
            continue
        vs = sorted(g.volumes)
        for i in range(len(vs)):
            for j in range(i + 1, len(vs)):
                e = cells.setdefault((vs[i], vs[j]), [0, 0])
                e[0] += 1
                e[1] += g.size

    def name(v: int) -> str:
        return plan.vols[v].name if v < len(plan.vols) else str(v)

    return pc.CleanupVolumeMatrix(
        version=plan.version,
        volumes=[vc.name for vc in plan.vols],
        cells=[
            pc.CleanupVolumeCell(a=name(a), b=name(b), groups=c[0], bytes=c[1])
            for (a, b), c in sorted(cells.items())
        ],
    )


def risk_copies(
    plan: CleanupPlan, rid: str, offset: int, limit: int, scan_ids: list[str]
) -> pc.CleanupRiskCopies:
    i = RK.IDX.get(rid if rid in RK.IDX else (RK.BY_CODE[rid].id if rid in RK.BY_CODE else ""), -1)
    rows = plan.risk_rows[plan.risk_ids == i] if i >= 0 else np.zeros(0, np.int64)
    b = (
        bucket(plan, i)
        if i >= 0
        else pc.CleanupBucket(id=rid, code=rid, severity="none", title=rid, copies=0, groups=0, bytes=0)
    )
    items = [copy_model(plan, int(r), scan_ids) for r in rows[offset : offset + limit].tolist()]
    return pc.CleanupRiskCopies(version=plan.version, risk=b, total=len(rows), items=items)


def sample(plan: CleanupPlan, per_bucket: int, seed: int | None = None) -> list[int]:
    """Groups to look at: per action and per high or medium risk kind, a few at random."""
    rnd = random.Random(seed if seed is not None else plan.version)
    out: list[int] = []
    seen: set[int] = set()
    reps = plan.is_rep & (plan.keep == K.K_NO)
    strata: list[np.ndarray] = []
    for code in (K.A_TRASH, K.A_HARDLINK, K.A_SYMLINK, K.A_CLONE):
        strata.append(np.unique(plan.group_of[reps & (plan.act == code)]))
    for i in np.unique(plan.risk_ids).tolist():
        on = (plan.risk_ids == i) & (plan.risk_sev >= K.SEV_MEDIUM)
        if on.any():
            strata.append(np.unique(plan.group_of[plan.risk_rows[on]]))
    for s in strata:
        ids = [int(x) for x in s.tolist() if int(plan.gstate[int(x)]) == G_IN]
        rnd.shuffle(ids)
        for g in ids[:per_bucket]:
            if g not in seen:
                seen.add(g)
                out.append(g)
    return out


# ------------------------------------------------------------------ export / import
def export_json(plan: CleanupPlan, scan_ids: list[str]) -> dict[str, Any]:
    now, later = plan.freed()
    groups: list[dict[str, Any]] = []
    for gi, g in enumerate(plan.groups):
        copies = []
        for u in plan.gunits[gi]:
            r = u.rep
            copies.append(
                {
                    "path": u.path,
                    "volume": plan.vols[u.vol].name if u.vol < len(plan.vols) else "",
                    "keep": "locked"
                    if plan.keep[r] == K.K_LOCKED
                    else "keep"
                    if plan.keep[r] == K.K_KEEP
                    else "none",
                    "action": K.ACTION_NAME.get(int(plan.act[r])) if plan.keep[r] == K.K_NO else None,
                    "target": plan.paths[int(plan.target[r])] if plan.target[r] >= 0 else "",
                    "source": keep_source(plan, r) if plan.keep[r] != K.K_NO else action_source(plan, r),
                    "risks": [
                        {"id": rid, "severity": sev_name(s), "text": risk_text(plan, r, rid)}
                        for rid, s in plan.risks_of(r)
                    ],
                    "freed_now": int(now[r]),
                    "freed_later": int(later[r]),
                    "mtime": int(plan.m["mtime"][r]),
                }
            )
        groups.append(
            {
                "group": gi,
                "digest": _hex(g.digest),
                "size": g.size,
                "state": G_NAMES[int(plan.gstate[gi])],
                "copies": copies,
            }
        )
    body = {
        "format": "spaceknife.dupes-cleanup.v1",
        "exported": datetime.now(UTC).isoformat(),
        "scans": scan_ids,
        "version": plan.version,
        "policy": plan.policy.to_json(),
        "groups": groups,
    }
    import hashlib

    digest = hashlib.blake2b(
        json.dumps(body, sort_keys=True).encode("utf-8", "surrogatepass"), digest_size=32
    ).hexdigest()
    body["checksum"] = digest
    return body


CSV_COLUMNS = (
    "group",
    "digest16",
    "size",
    "copies",
    "path",
    "volume",
    "fs",
    "keep",
    "action",
    "target",
    "risks",
    "severity",
    "source",
    "mtime",
    "same_owner",
)


def export_csv(plan: CleanupPlan) -> str:
    buf = io.StringIO()
    w = csv.writer(buf)
    w.writerow(CSV_COLUMNS)
    for gi, g in enumerate(plan.groups):
        if int(plan.gstate[gi]) != G_IN:
            continue
        for u in plan.gunits[gi]:
            r = u.rep
            vc = plan.vols[u.vol] if u.vol < len(plan.vols) else None
            a, b = plan.probes.get(r), plan.probes.get(int(plan.refk[r])) if plan.refk[r] >= 0 else None
            same = "" if not (a and b and a.ok and b.ok) else ("yes" if a.owner_key == b.owner_key else "no")
            w.writerow(
                [
                    gi,
                    _hex(g.digest)[:16],
                    g.size,
                    len(plan.gunits[gi]),
                    u.path,
                    vc.name if vc else "",
                    vc.fs if vc else "",
                    "locked" if plan.keep[r] == K.K_LOCKED else "keep" if plan.keep[r] == K.K_KEEP else "no",
                    K.ACTION_NAME.get(int(plan.act[r]), "") if plan.keep[r] == K.K_NO else "",
                    plan.paths[int(plan.target[r])] if plan.target[r] >= 0 else "",
                    " ".join(rid for rid, _ in plan.risks_of(r)),
                    sev_name(int(plan.sev[r])),
                    K.SRC_NAMES.get(int(plan.ksrc[r]) if plan.keep[r] != K.K_NO else int(plan.asrc[r]), ""),
                    datetime.fromtimestamp(int(plan.m["mtime"][r]) / 1e9, tz=UTC).isoformat(),
                    same,
                ]
            )
    return buf.getvalue()


def import_json(plan: CleanupPlan, data: dict[str, Any]) -> tuple[int, list[str], Any]:
    """Take a file's rules, options and decisions: per (digest, path) that matches, its decision; the
    rest is listed. Only edits: the plan is reviewed, acknowledged and approved again."""
    from .policy import ManualCopy, Policy

    if data.get("format") != "spaceknife.dupes-cleanup.v1":
        raise ValueError("format")
    pol = Policy.from_json(data.get("policy", {}))
    pol.manual = {}
    pol.groups = {}
    by_digest = {_hex(g.digest): gi for gi, g in enumerate(plan.groups)}
    applied = 0
    unmatched: list[str] = []
    for grp in data.get("groups", []):
        gi = by_digest.get(str(grp.get("digest", "")))
        for c in grp.get("copies", []):
            path = str(c.get("path", ""))
            if gi is None or plan.find(gi, path) is None:
                unmatched.append(path)
                continue
            d = str(grp["digest"])
            if c.get("keep") == "keep":
                pol.manual[(d, path)] = ManualCopy(keep="keep")
                applied += 1
            elif c.get("keep") == "none" and c.get("action"):
                pol.manual[(d, path)] = ManualCopy(
                    keep="not", action=str(c["action"]), target=str(c.get("target", ""))
                )
                applied += 1
    return applied, unmatched, pol

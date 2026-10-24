// The plan's group table (docs/dupes.md §16.13.3): one row per group (tick, name, copies, size,
// what it frees, its actions in short, the highest risk, where the decisions came from, reviewed),
// expanded: one row per copy with its keep switch, its action (only what can be done here) and
// target, its risks and the reason for its decision. A bulk bar acts on the ticked groups, or on
// everything the filter shows, always through a preview.

import { useEffect, useRef } from "react";

import type { CleanupCopy, CleanupGroup } from "@/protocol/generated";
import {
  actionsLine,
  bulkAction,
  bulkClearManual,
  bulkExclude,
  closeCleanup,
  inspect,
  keepAdd,
  keepOnly,
  keepRemove,
  loadGroups,
  markReviewed,
  setAction,
  setCleanup,
  setFilter,
  setTarget,
  sevLabel,
  toggleExpand,
  toggleSelect,
  useCleanup,
} from "@/state/cleanup";
import { openDrawer } from "@/state/dupes";
import { distinctPaths, fmtInt, fmtSize, plural } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { ActionPicker, Freed, RiskChips, Sev } from "./parts";

const sourceLabel = (s: string): string => ({ rule: t("规则"), manual: t("手动"), pair: t("文件夹对"), safety: t("兜底"), mixed: t("混合") })[s] ?? s;

/** Where a copy's decision came from, in a word or two (the full sentence is its tooltip). */
export const shortSource = (m: CleanupCopy): string =>
  m.manual
    ? t("手动")
    : ({
        forced_needed: t("程序需要"),
        forced_alias: t("同一个文件的另一个名字"),
        forced_pinned: t("只能保留"),
        forced_vcs: t("版本库内部"),
        forced_network: t("网络盘"),
        pair_side: t("文件夹对"),
        pair_both: t("文件夹对"),
        keep_all: t("全部保留的文件夹"),
        per_volume: t("每卷一份"),
        per_folder: t("每个文件夹一份"),
        keep_n: t("至少留几份"),
        prefer: t("优先保留的文件夹"),
        avoid: t("只能留在不想留的地方"),
        folder: t("文件夹一致"),
        safety_net: t("兜底"),
        not_kept: t("保留了另一份"),
      } as Record<string, string>)[m.source_code] ?? m.source;

export function GroupTable() {
  const groups = useCleanup((s) => s.groups);
  const total = useCleanup((s) => s.total);
  const selected = useCleanup((s) => s.selected);
  return (
    <div className="cl-table" role="grid" aria-label={t("清理方案的组")} aria-rowcount={total}>
      <div className="cl-thead" role="row">
        <span role="columnheader" className="cl-c-tick" />
        <span role="columnheader" className="cl-c-name">
          {t("名字")}
        </span>
        <span role="columnheader" className="cl-c-n">
          {t("份")}
        </span>
        <span role="columnheader" className="cl-c-size">
          {t("每份")}
        </span>
        <span role="columnheader" className="cl-c-freed">
          {t("腾出")}
        </span>
        <span role="columnheader" className="cl-c-acts">
          {t("处理")}
        </span>
        <span role="columnheader" className="cl-c-sev">
          {t("风险")}
        </span>
        <span role="columnheader" className="cl-c-src">
          {t("来源")}
        </span>
        <span role="columnheader" className="cl-c-rev" title={t("已过目")}>
          ✓
        </span>
      </div>
      {groups.map((g) => (
        <GroupRow key={g.id} g={g} ticked={selected.includes(g.id)} />
      ))}
      {!groups.length && <Empty />}
      {groups.length > 0 && total <= groups.length && <NotListed />}
      {total > groups.length && (
        <button className="ghost small cl-more" onClick={() => void loadGroups(true)}>
          {plural(total - groups.length, t("再显示一些（还有 {n} 组）", { n: fmtInt(total - groups.length) }), "Show more (1 group left)")}
        </button>
      )}
    </div>
  );
}

/** Groups out of the plan (cleaned, changed since the check): counted under the list. */
function NotListed() {
  const notDone = useCleanup((s) => s.state?.summary.not_done ?? []);
  const filter = useCleanup((s) => s.filter);
  const out = notDone.filter((x) => x.unit === "groups" && (x.reason === "cleaned" || x.reason === "stale"));
  if (!out.length || filter) return null;
  return (
    <div className="muted small cl-notlisted">
      {t("不在表里：")}
      {out.map((x) => `${x.text} ${t("{n} 组", { n: fmtInt(x.count) })}`).join(" · ")}
      {" · "}
      <button className="link" onClick={() => setFilter(`state:${out[0]!.reason}`)}>
        {t("看看这些")}
      </button>
    </div>
  );
}

/** No group to show: say why (a filter, everything cleaned, files changed) and what to do. */
function Empty() {
  const filter = useCleanup((s) => s.filter);
  const notDone = useCleanup((s) => s.state?.summary.not_done ?? []);
  if (filter)
    return (
      <div className="muted small cl-empty">
        {t("没有符合筛选的组")}{" "}
        <button className="ghost tiny" onClick={() => setFilter("")}>
          {t("清掉筛选")}
        </button>
      </div>
    );
  const out = notDone.filter((x) => x.unit === "groups" && (x.reason === "cleaned" || x.reason === "stale"));
  return (
    <div className="muted small cl-empty">
      <div>{t("方案里没有要处理的组")}</div>
      {out.map((x) => (
        <div key={x.reason}>
          {x.text} · {t("{n} 组", { n: fmtInt(x.count) })}
        </div>
      ))}
      {out.length > 0 && (
        <div className="cl-empty-row">
          {t("撤销过的、改过的文件要再查一次，才回到方案里")}
          <button
            className="ghost tiny"
            onClick={() => {
              void closeCleanup().then(() => openDrawer(true));
            }}
          >
            <Icon name="refresh" size={11} /> {t("回到查找结果")}
          </button>
        </div>
      )}
    </div>
  );
}

function GroupRow({ g, ticked }: { g: CleanupGroup; ticked: boolean }) {
  const expanded = useCleanup((s) => s.expanded.includes(g.id));
  const focus = useCleanup((s) => s.focus === g.id);
  const ref = useRef<HTMLDivElement>(null);
  // the keyboard cursor stays in view
  useEffect(() => {
    if (focus) ref.current?.scrollIntoView?.({ block: "nearest" });
  }, [focus]);
  const live = g.state === "in";
  const names = g.names > 1 ? t("{name}（另有 {n} 个不同的文件名）", { name: g.name, n: g.names - 1 }) : g.name;
  return (
    <div ref={ref} className={`cl-group${focus ? " focus" : ""}${live ? "" : " out"}`} role="rowgroup">
      <div
        className="cl-row"
        role="row"
        aria-selected={ticked}
        onClick={() => {
          void inspect(g.id);
          setCleanup({ sheet: true });
        }}
        onDoubleClick={() => toggleExpand(g.id)}
      >
        <span className="cl-c-tick" role="gridcell">
          <input
            type="checkbox"
            aria-label={t("勾选这一组")}
            checked={ticked}
            onClick={(e) => e.stopPropagation()}
            onChange={(e) => toggleSelect(g.id, (e.nativeEvent as MouseEvent).shiftKey)}
          />
        </span>
        <span className="cl-c-name" role="gridcell">
          <button className="icon-btn tiny" aria-label={expanded ? t("收起") : t("展开")} aria-expanded={expanded} onClick={(e) => (e.stopPropagation(), toggleExpand(g.id))}>
            <Icon name={expanded ? "chevronDown" : "chevronRight"} size={12} />
          </button>
          <span className="cl-name" title={g.keep_line}>
            {names}
          </span>
          {g.cross && <span className="cl-tag">{g.volumes.join(" · ")}</span>}
          {g.conflict && <span className="cl-tag warn" title={t("同一层的两条规则结论不同，按顺序取了第一条")}>{t("规则冲突")}</span>}
          {g.safety_net && <span className="cl-tag warn">{t("兜底保留")}</span>}
          {g.only_avoided && <span className="cl-tag warn">{t("只能留在不想留的地方")}</span>}
        </span>
        <span className="cl-c-n mono" role="gridcell">
          {g.copies}
        </span>
        <span className="cl-c-size mono" role="gridcell">
          {fmtSize(g.size)}
        </span>
        <span className="cl-c-freed" role="gridcell">
          <Freed now={g.freed_now} later={g.freed_later} />
        </span>
        <span className="cl-c-acts small" role="gridcell">
          {live ? actionsLine(g.actions) : <span className="muted">{g.keep_line}</span>}
        </span>
        <span className="cl-c-sev" role="gridcell" title={g.risk_count ? t("这一组共有 {n} 条风险，最高是「{sev}」", { n: g.risk_count, sev: sevLabel(g.severity) }) : undefined}>
          <Sev s={g.severity} n={g.risk_count} />
        </span>
        <span className="cl-c-src small muted" role="gridcell">
          {sourceLabel(g.source)}
        </span>
        <span className="cl-c-rev" role="gridcell">
          <input type="checkbox" aria-label={t("已过目")} checked={g.reviewed} disabled={!live} onClick={(e) => e.stopPropagation()} onChange={(e) => void markReviewed([g.id], e.target.checked)} />
          {g.changed_since_review && <span className="cl-tag warn" title={t("过目之后决定变了")}>!</span>}
        </span>
      </div>
      {expanded && <Copies g={g} />}
    </div>
  );
}

function Copies({ g }: { g: CleanupGroup }) {
  const live = g.state === "in";
  const paths = distinctPaths(
    g.members.map((m) => m.path),
    56,
  );
  const short = new Map(g.members.map((m, i) => [m.path, paths[i] ?? m.path]));
  const keepers = g.members.filter((m) => m.keep !== "none" && !m.view_of);
  const ord = new Map(g.members.filter((m) => !m.view_of).map((m, i) => [m.row, i + 1]));
  return (
    <div className="cl-copies" role="group" aria-label={t("这一组的各份")}>
      {!live && <div className="muted small cl-out">{g.keep_line}</div>}
      {g.members.map((m) => (
        <CopyRow key={m.row} g={g} m={m} n={ord.get(m.row) ?? 0} short={short} live={live} keepers={keepers} />
      ))}
      {g.total_members > g.members.length && (
        <div className="muted small">{t("另有 {n} 份：在右边的检查器里看全部", { n: fmtInt(g.total_members - g.members.length) })}</div>
      )}
    </div>
  );
}

/** A keeper in the 「指向」 list: 「第 2 份」 and its name (short enough for the row). */
function keeperLabel(k: CleanupCopy, keepers: CleanupCopy[], short: Map<string, string>): string {
  const s = short.get(k.path) ?? k.path;
  const name = s.split(/[\\/]/).pop() ?? s;
  const same = keepers.filter((x) => (x.path.split(/[\\/]/).pop() ?? "") === (k.path.split(/[\\/]/).pop() ?? "")).length > 1;
  return same ? s : name;
}

function CopyRow(props: { g: CleanupGroup; m: CleanupCopy; n: number; short: Map<string, string>; live: boolean; keepers: CleanupCopy[] }) {
  const { g, m, n, short, live, keepers } = props;
  const locked = m.keep === "locked";
  const kept = m.keep !== "none";
  const view = !!m.view_of;
  const lastKeeper = kept && keepers.length <= 1;
  const keepTitle = locked ? m.source : lastKeeper ? t("每组至少留一份：先选另一份做保留点") : kept ? t("不保留这一份") : t("也保留这一份");
  const linked = m.action === "hardlink" || m.action === "symlink" || m.action === "clone";
  return (
    <div className={`cl-copy${kept ? " kept" : ""}${lastKeeper ? " last" : ""}`} role="row">
      <span className="cl-keep" role="gridcell">
        {view ? (
          <span className="muted small" title={t("和 {path} 是同一个文件", { path: m.view_of })}>
            ↳
          </span>
        ) : locked ? (
          <span className="cl-lock" title={m.source} aria-label={t("必须保留")}>
            <Icon name="shield" size={13} />
          </span>
        ) : (
          <button
            className={`cl-keep-btn${kept ? " on" : ""}`}
            aria-pressed={kept}
            aria-label={keepTitle}
            title={keepTitle}
            disabled={!live || lastKeeper}
            onClick={() => void (kept ? keepRemove(g.id, m.path) : keepAdd(g.id, m.path))}
          >
            {kept ? "◉" : "○"}
          </button>
        )}
      </span>
      <span className="cl-copy-act" role="gridcell">
        {kept || view ? (
          <span className="small">
            <span className="strong">{view ? t("跟着它代表的那份") : locked ? t("必须保留") : t("保留")}</span>
            {lastKeeper && !locked && <span className="muted"> · {t("唯一的保留点")}</span>}
          </span>
        ) : (
          <ActionPicker copy={m} disabled={!live} onPick={(a) => void setAction(g.id, m.path, a)} />
        )}
      </span>
      <span className="cl-copy-path mono" role="gridcell" title={m.target ? `${m.path} → ${m.target}` : m.path}>
        {n > 0 && n < 10 && (
          <span className="cl-ord" title={t("按 {n} 只保留这一份", { n })}>
            {n}
          </span>
        )}
        <span className="cl-copy-name">{short.get(m.path) ?? m.path}</span>
        {m.target && linked && keepers.length > 1 ? (
          <select className="cl-target" aria-label={t("指向")} value={m.target} onClick={(e) => e.stopPropagation()} onChange={(e) => void setTarget(g.id, m.path, e.target.value)}>
            {keepers.map((k) => (
              <option key={k.path} value={k.path} title={k.path}>
                → {keeperLabel(k, keepers, short)}
              </option>
            ))}
          </select>
        ) : m.target && linked ? (
          <span className="muted cl-copy-target"> → {short.get(m.target) ?? m.target}</span>
        ) : null}
      </span>
      <span className="cl-copy-vol small muted" role="gridcell">
        {m.volume} {m.fs.toUpperCase()}
      </span>
      <span className="cl-copy-risks" role="gridcell">
        <RiskChips risks={m.risks} />
      </span>
      <span className="cl-copy-src small muted" role="gridcell" title={m.source}>
        {shortSource(m)}
      </span>
      <span className="cl-copy-only" role="gridcell">
        {!view && live && !locked && !lastKeeper && (
          <button className="ghost tiny" onClick={() => void keepOnly(g.id, m.path)} title={t("这组只保留这一份")}>
            {t("只留它")}
          </button>
        )}
      </span>
    </div>
  );
}

/** The bulk bar: on the ticked groups, or on everything the filter shows (always previewed first). */
export function BulkBar() {
  const selected = useCleanup((s) => s.selected.length);
  const total = useCleanup((s) => s.total);
  const receipt = useCleanup((s) => s.receipt);
  const n = selected || total;
  return (
    <div className="cl-bulk" role="toolbar" aria-label={t("批量改")}>
      <span className="small muted">{selected ? t("已选 {n} 组", { n: fmtInt(selected) }) : t("符合筛选的全部 {n} 组", { n: fmtInt(total) })}</span>
      {receipt && (
        <span className="cl-receipt small" role="status" aria-live="polite" title={receipt}>
          <span className="cl-receipt-text">{receipt}</span>
          <button className="icon-btn tiny" aria-label={t("关闭")} onClick={() => setCleanup({ receipt: "" })}>
            <Icon name="close" size={10} />
          </button>
        </span>
      )}
      <span className="flex" />
      <span className="small muted">{t("不保留的份：")}</span>
      {(["trash", "hardlink", "symlink", "clone", "leave"] as const).map((a) => (
        <button key={a} className="ghost tiny" disabled={!n} onClick={() => void bulkAction(a)}>
          {({ trash: t("回收站"), hardlink: t("硬链接"), symlink: t("符号链接"), clone: t("块克隆"), leave: t("不动") } as const)[a]}
        </button>
      ))}
      <button className="ghost tiny" disabled={!n} onClick={() => void bulkExclude()}>
        {t("排除")}
      </button>
      <button className="ghost tiny" disabled={!selected} onClick={() => void markReviewed(useCleanup.getState().selected)}>
        {t("已过目")}
      </button>
      <button className="ghost tiny" disabled={!n} onClick={() => void bulkClearManual()}>
        {t("清除手动")}
      </button>
    </div>
  );
}

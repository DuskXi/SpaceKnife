// The plan's group table (docs/dupes.md §16.13.3): one row per group (tick, name, copies, size,
// what it frees, its actions in short, the highest risk, where the decisions came from, reviewed),
// expanded: one row per copy with its keep switch, its action (only what can be done here) and
// target, its risks and the reason for its decision. A bulk bar acts on the ticked groups, or on
// everything the filter shows, always through a preview.

import type { CleanupCopy, CleanupGroup } from "@/protocol/generated";
import {
  actionsLine,
  bulkAction,
  bulkClearManual,
  bulkExclude,
  inspect,
  keepAdd,
  keepOnly,
  keepRemove,
  loadGroups,
  markReviewed,
  setAction,
  setTarget,
  toggleExpand,
  toggleSelect,
  useCleanup,
} from "@/state/cleanup";
import { distinctPaths, fmtInt, fmtSize, plural } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { ActionPicker, Freed, RiskChip, Sev } from "./parts";

const sourceLabel = (s: string): string => ({ rule: t("规则"), manual: t("手动"), pair: t("文件夹对"), safety: t("兜底"), mixed: t("混合") })[s] ?? s;

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
        <span role="columnheader" className="cl-c-rev">
          ✓
        </span>
      </div>
      {groups.map((g) => (
        <GroupRow key={g.id} g={g} ticked={selected.includes(g.id)} />
      ))}
      {!groups.length && <div className="muted small cl-empty">{t("没有符合条件的组")}</div>}
      {total > groups.length && (
        <button className="ghost small cl-more" onClick={() => void loadGroups(true)}>
          {plural(total - groups.length, t("再显示一些（还有 {n} 组）", { n: fmtInt(total - groups.length) }), "Show more (1 group left)")}
        </button>
      )}
    </div>
  );
}

function GroupRow({ g, ticked }: { g: CleanupGroup; ticked: boolean }) {
  const expanded = useCleanup((s) => s.expanded.includes(g.id));
  const focus = useCleanup((s) => s.focus === g.id);
  const live = g.state === "in";
  const names = g.names > 1 ? t("{name}（另有 {n} 个不同的文件名）", { name: g.name, n: g.names - 1 }) : g.name;
  return (
    <div className={`cl-group${focus ? " focus" : ""}${live ? "" : " out"}`} role="rowgroup">
      <div
        className="cl-row"
        role="row"
        aria-selected={ticked}
        onClick={() => void inspect(g.id)}
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
        <span className="cl-c-sev" role="gridcell">
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
  const keepers = g.members.filter((m) => m.keep !== "none" && !m.view_of);
  return (
    <div className="cl-copies" role="group" aria-label={t("这一组的各份")}>
      {!live && <div className="muted small cl-out">{g.keep_line}</div>}
      {g.members.map((m, i) => (
        <CopyRow key={m.row} g={g} m={m} short={paths[i] ?? m.path} live={live} keepers={keepers} lastKeeper={keepers.length <= 1 && m.keep === "keep"} />
      ))}
      {g.total_members > g.members.length && (
        <div className="muted small">{t("另有 {n} 份：在右边的检查器里看全部", { n: fmtInt(g.total_members - g.members.length) })}</div>
      )}
    </div>
  );
}

function CopyRow(props: { g: CleanupGroup; m: CleanupCopy; short: string; live: boolean; keepers: CleanupCopy[]; lastKeeper: boolean }) {
  const { g, m, short, live, keepers, lastKeeper } = props;
  const locked = m.keep === "locked";
  const kept = m.keep !== "none";
  const view = !!m.view_of;
  const keepTitle = locked ? m.source : lastKeeper ? t("每组至少留一份：先选另一份做保留点") : kept ? t("不保留这一份") : t("也保留这一份");
  return (
    <div className={`cl-copy${kept ? " kept" : ""}`} role="row">
      <span className="cl-keep" role="gridcell">
        {view ? (
          <span className="muted small" title={t("和 {path} 是同一个文件", { path: m.view_of })}>
            ↳
          </span>
        ) : locked ? (
          <span title={m.source} aria-label={t("必须保留")}>
            <Icon name="shield" size={13} />
          </span>
        ) : (
          <button
            className={`cl-keep-btn${kept ? " on" : ""}`}
            aria-pressed={kept}
            aria-label={keepTitle}
            title={keepTitle}
            disabled={!live || (kept && lastKeeper)}
            onClick={() => void (kept ? keepRemove(g.id, m.path) : keepAdd(g.id, m.path))}
          >
            {kept ? "◉" : "○"}
          </button>
        )}
      </span>
      <span className="cl-copy-act" role="gridcell">
        {kept || view ? (
          <span className="small strong">{kept ? t("保留") : t("跟着它代表的那份")}</span>
        ) : (
          <ActionPicker copy={m} disabled={!live} onPick={(a) => void setAction(g.id, m.path, a)} />
        )}
      </span>
      <span className="cl-copy-path mono" role="gridcell" title={m.path}>
        {short}
        {m.target && (m.action === "hardlink" || m.action === "symlink" || m.action === "clone") && keepers.length > 1 ? (
          <select className="cl-target" aria-label={t("指向")} value={m.target} onChange={(e) => void setTarget(g.id, m.path, e.target.value)}>
            {keepers.map((k) => (
              <option key={k.path} value={k.path}>
                → {k.path}
              </option>
            ))}
          </select>
        ) : m.target ? (
          <span className="muted"> → {m.target}</span>
        ) : null}
      </span>
      <span className="cl-copy-vol small muted" role="gridcell">
        {m.volume} {m.fs.toUpperCase()}
      </span>
      <span className="cl-copy-risks" role="gridcell">
        {m.risks
          .filter((r) => r.severity !== "info")
          .slice(0, 3)
          .map((r) => (
            <RiskChip key={r.id} r={r} />
          ))}
      </span>
      <span className="cl-copy-src small muted" role="gridcell" title={m.source}>
        {m.manual ? t("手动") : m.source}
      </span>
      <span className="cl-copy-only" role="gridcell">
        {!view && live && !locked && (
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
  const n = selected || total;
  return (
    <div className="cl-bulk" role="toolbar" aria-label={t("批量改")}>
      <span className="small muted">{selected ? t("已选 {n} 组", { n: fmtInt(selected) }) : t("符合筛选的全部 {n} 组", { n: fmtInt(total) })}</span>
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

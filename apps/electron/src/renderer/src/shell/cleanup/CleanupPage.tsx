// Cleanup mode (docs/dupes.md §16.13.2): the tab shows the duplicate-file cleanup plan instead of
// its normal panes, the way compare mode shows a change tree. Head: version, the risk check, what
// is freed, the severities. Toolbar: the preset, the pages (groups, folder pairs, volumes, review
// and run, history), the filter, undo / redo, 「退出清理模式」. Groups page: rules | groups | the
// inspector over the picture. Nothing here runs the plan except the review page's 「执行…」.

import { useEffect, useRef, useState } from "react";

import type { Tab } from "@/state/store";
import {
  cleanupKey,
  closeCleanup,
  edit,
  loadGroups,
  presetLabel,
  setFilter,
  setPreset,
  setSort,
  setView,
  undoEdit,
  useCleanup,
  type CleanupSort,
  type CleanupView,
} from "@/state/cleanup";
import { focusGroup } from "@/state/dupes";
import { setViewSettings } from "@/state/actions";
import { fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { CleanupDialogs } from "./Dialogs";
import { BulkBar, GroupTable } from "./GroupTable";
import { History, RunView } from "./History";
import { CleanupInspector } from "./Inspector";
import { ReviewPage } from "./Review";
import { RuleBar } from "./RuleBar";
import { Sev } from "./parts";

const VIEWS: CleanupView[] = ["groups", "pairs", "volumes", "review", "run", "history"];
const viewLabel = (v: CleanupView): string =>
  ({ groups: t("组"), pairs: t("文件夹对"), volumes: t("卷"), review: t("审阅并执行…"), run: t("执行"), history: t("清理记录") })[v];
const SORTS: CleanupSort[] = ["freed", "risk", "size", "copies", "path", "action"];
const sortLabel = (s: CleanupSort): string => ({ freed: t("腾出"), risk: t("风险"), size: t("单份大小"), copies: t("份数"), path: t("路径"), action: t("处理") })[s];
const PRESETS = ["recommended", "conservative", "links", "watch", "custom"];

export function CleanupPage({ tab, map }: { tab: Tab; map?: React.ReactNode }) {
  const st = useCleanup((s) => s.state);
  const view = useCleanup((s) => s.view);
  const planId = useCleanup((s) => s.planId);
  const focus = useCleanup((s) => s.focus);
  const jobId = useCleanup((s) => s.jobId);
  // the picture: the 「重复」 colouring, the group in the inspector lit
  useEffect(() => {
    const vid = tab.viewIds[0];
    if (vid) void setViewSettings(vid, { color_mode: "dupes" }).catch(() => undefined);
  }, [tab.viewIds]);
  useEffect(() => {
    if (jobId && focus !== null) focusGroup(jobId, focus, tab.viewIds);
    return () => focusGroup(null, null);
  }, [jobId, focus, tab.viewIds]);
  const historyOnly = !planId;
  return (
    <div className="content column cl-page" aria-label={t("重复文件清理方案")}>
      <Head historyOnly={historyOnly} />
      <div className="cl-toolbar" role="toolbar" aria-label={t("清理方案")}>
        {!historyOnly && (
          <select aria-label={t("预设")} value={st?.preset ?? "recommended"} onChange={(e) => void setPreset(e.target.value)}>
            {PRESETS.map((p) => (
              <option key={p} value={p}>
                {t("预设：{p}", { p: presetLabel(p) })}
              </option>
            ))}
          </select>
        )}
        <div className="segmented small" role="tablist">
          {VIEWS.filter((v) => !historyOnly || v === "history").map((v) => (
            <button key={v} role="tab" aria-selected={view === v} className={view === v ? "on" : ""} onClick={() => void setView(v)}>
              {viewLabel(v)}
            </button>
          ))}
        </div>
        {!historyOnly && view === "groups" && <FilterInput />}
        <span className="flex" />
        {!historyOnly && (
          <>
            <button className="icon-btn small" aria-label={t("撤回")} title={t("撤回（Ctrl+Z）")} disabled={!st?.can_undo} onClick={() => void undoEdit(false)}>
              ↶
            </button>
            <button className="icon-btn small" aria-label={t("重做")} title={t("重做（Ctrl+Shift+Z）")} disabled={!st?.can_redo} onClick={() => void undoEdit(true)}>
              ↷
            </button>
          </>
        )}
        <button className="ghost small" onClick={() => void closeCleanup()}>
          <Icon name="close" size={12} /> {t("退出清理模式")}
        </button>
      </div>
      {view === "groups" && !historyOnly && <GroupsView map={map} />}
      {view === "pairs" && <PairsView />}
      {view === "volumes" && <VolumesView />}
      {view === "review" && <ReviewPage />}
      {view === "run" && <RunView />}
      {view === "history" && <History />}
      <Receipt />
      <CleanupDialogs />
    </div>
  );
}

function Head({ historyOnly }: { historyOnly: boolean }) {
  const st = useCleanup((s) => s.state);
  if (historyOnly || !st) return <div className="cl-head strong">{t("重复文件清理记录")}</div>;
  const sm = st.summary;
  const pct = st.facts_total ? Math.floor((100 * st.facts_done) / st.facts_total) : 100;
  return (
    <div className="cl-head" role="status">
      <span className="strong">{t("清理方案 第 {v} 版", { v: fmtInt(st.version) })}</span>
      <span className="muted small">{st.approval && st.approval.version === st.version ? t("已批准") : t("编辑中")}</span>
      <span className="muted small">{pct < 100 ? t("风险核查 {pct}%", { pct }) : t("风险核查完成")}</span>
      <span className="small">
        {t("腾出：马上 {now} · 清空或清除之后 +{later} · {g} 组 {n} 份", {
          now: fmtSize(sm.freed_now),
          later: fmtSize(sm.freed_later),
          g: fmtInt(sm.groups),
          n: fmtInt(sm.copies),
        })}
      </span>
      <span className="flex" />
      {(["high", "medium", "low", "info"] as const).map((s) =>
        sm.by_severity[s] ? (
          <span key={s} className="small">
            <Sev s={s} n={sm.by_severity[s]} />
          </span>
        ) : null,
      )}
    </div>
  );
}

function FilterInput() {
  const filter = useCleanup((s) => s.filter);
  const sort = useCleanup((s) => s.sort);
  const [text, setText] = useState(filter);
  useEffect(() => setText(filter), [filter]);
  return (
    <>
      <input
        className="cl-filter mono"
        aria-label={t("筛选")}
        placeholder={t("筛选：risk:high action:hardlink in:\"D:\\Photos\" tag:sync …")}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") setFilter(text);
          if (e.key === "Escape") setFilter("");
        }}
      />
      <select aria-label={t("排序")} value={sort} onChange={(e) => setSort(e.target.value as CleanupSort)}>
        {SORTS.map((s) => (
          <option key={s} value={s}>
            {t("按{s}排", { s: sortLabel(s) })}
          </option>
        ))}
      </select>
    </>
  );
}

function GroupsView({ map }: { map?: React.ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  return (
    <div className="cl-body">
      <RuleBar />
      <div
        className="cl-center"
        ref={ref}
        tabIndex={0}
        onKeyDown={(e) => {
          if ((e.target as HTMLElement).closest("input, select, textarea")) return;
          const key = e.code.startsWith("Digit") ? e.code.slice(5) : e.key;
          if (cleanupKey({ key, shiftKey: e.shiftKey, ctrlKey: e.ctrlKey, metaKey: e.metaKey })) e.preventDefault();
        }}
        aria-label={t("组表（j k 上下，1–9 只保留第几份，t h s c l 改处理，r 过目，x 排除，Ctrl+Z 撤回）")}
      >
        <GroupTable />
        <BulkBar />
      </div>
      <div className="cl-right">
        <CleanupInspector />
        {map && <div className="cl-map">{map}</div>}
      </div>
    </div>
  );
}

function PairsView() {
  const pairs = useCleanup((s) => s.pairs);
  const total = useCleanup((s) => s.pairsTotal);
  return (
    <div className="cl-pairs">
      <div className="muted small">{t("每组里两份所在的文件夹记一对；两边相对路径一致的子文件夹对合并到上一级")}</div>
      {pairs.map((p) => (
        <div key={`${p.a}|${p.b}`} className="cl-pair">
          <div className="mono small">
            {p.a} ⇄ {p.b}
          </div>
          <div className="small muted">
            {t("{g} 组 · {size}", { g: fmtInt(p.groups), size: fmtSize(p.bytes) })} ·{" "}
            {t("后者的 {x}% 在前者有一份 · 前者的 {y}% 在后者有", { x: Math.round(100 * p.b_in_a), y: Math.round(100 * p.a_in_b) })}
            {p.merged > 0 && ` · ${t("合并了 {n} 对子文件夹", { n: fmtInt(p.merged) })}`} ·{" "}
            {{ a: t("现在保留在前者"), b: t("现在保留在后者"), both: t("两边都留"), mixed: t("有的留前者，有的留后者"), none: "" }[p.keep] ?? ""}
          </div>
          <div className="cl-pair-buttons">
            <button className="ghost tiny" onClick={() => void edit({ op: "pair_set", pair: { a: p.a, b: p.b, keep: "a", other_action: "", auto: false } })}>
              {t("保留前者")}
            </button>
            <button className="ghost tiny" onClick={() => void edit({ op: "pair_set", pair: { a: p.a, b: p.b, keep: "b", other_action: "", auto: false } })}>
              {t("保留后者")}
            </button>
            <button className="ghost tiny" onClick={() => void edit({ op: "pair_set", pair: { a: p.a, b: p.b, keep: "both", other_action: "", auto: false } })}>
              {t("两边都留")}
            </button>
            {p.rule && (
              <button className="ghost tiny" onClick={() => void edit({ op: "pair_remove", pair: { a: p.a, b: p.b, keep: "a", other_action: "", auto: false } })}>
                {t("去掉这条规则")}
              </button>
            )}
            <button
              className="ghost tiny"
              onClick={() => {
                setFilter(`in:"${p.a}"`);
                void setView("groups");
              }}
            >
              {t("看这些组")}
            </button>
          </div>
        </div>
      ))}
      {!pairs.length && <div className="muted small cl-empty">{t("没有跨文件夹的组")}</div>}
      {total > pairs.length && <div className="muted small">{t("只列出最大的 {n} 对", { n: fmtInt(pairs.length) })}</div>}
    </div>
  );
}

function VolumesView() {
  const m = useCleanup((s) => s.volumes);
  if (!m) return null;
  if (!m.cells.length) return <div className="muted small cl-empty">{t("没有跨卷的组")}</div>;
  return (
    <div className="cl-pairs">
      {m.cells.map((c) => (
        <div key={`${c.a}|${c.b}`} className="cl-pair">
          <div className="small strong">
            {c.a} ⇄ {c.b}
          </div>
          <div className="small muted">{t("{g} 组 · {size}", { g: fmtInt(c.groups), size: fmtSize(c.bytes) })}</div>
          <div className="cl-pair-buttons">
            <button className="ghost tiny" onClick={() => void edit({ op: "pair_set", pair: { a: c.a, b: c.b, keep: "a", other_action: "", auto: false } })}>
              {t("保留 {v} 上的", { v: c.a })}
            </button>
            <button className="ghost tiny" onClick={() => void edit({ op: "pair_set", pair: { a: c.a, b: c.b, keep: "b", other_action: "", auto: false } })}>
              {t("保留 {v} 上的", { v: c.b })}
            </button>
            <button
              className="ghost tiny"
              onClick={() => {
                setFilter(`vol:${c.a} cross:yes`);
                void setView("groups");
              }}
            >
              {t("看这些组")}
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}

function Receipt() {
  const receipt = useCleanup((s) => s.receipt);
  if (!receipt) return null;
  return (
    <div className="cl-receipt small muted" role="status" aria-live="polite">
      {receipt}
      <button className="icon-btn tiny" aria-label={t("关闭")} onClick={() => useCleanup.setState({ receipt: "" })}>
        <Icon name="close" size={10} />
      </button>
    </div>
  );
}

/** Keep the page's lists in step when it becomes visible again. */
export function useCleanupRefresh(): void {
  useEffect(() => void loadGroups(), []);
}

// Cleanup mode (docs/dupes.md §16.13.2): the tab shows the duplicate-file cleanup plan instead of
// its normal panes, the way compare mode shows a change tree. Head: version, the risk check, what
// is freed, the severities. Toolbar: the preset, the pages (groups, folder pairs, volumes, review
// and run, history), the filter, undo / redo, 「退出清理模式」. Groups page: rules | groups | the
// inspector over the picture; narrower windows fold the rules into a drawer and stack the
// inspector and the picture as tabs (below 900 px the inspector opens as a sheet over the table).
// Nothing here runs the plan except the review page's 「执行…」.

import { useEffect, useRef, useState } from "react";

import type { Tab } from "@/state/store";
import {
  cleanupKey,
  closeCleanup,
  edit,
  loadGroups,
  presetLabel,
  setCleanup,
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
import { Icon, Kbd } from "@/ui/Icon";

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

/** wide ≥ 1280 px: three columns; medium: rules in a drawer, inspector and picture as tabs;
 * narrow < 900 px: the table alone, the inspector as a sheet. */
export type CleanupLayout = "wide" | "medium" | "narrow";
export const layoutFor = (width: number): CleanupLayout => (width >= 1280 ? "wide" : width >= 900 ? "medium" : "narrow");

function useLayout(ref: React.RefObject<HTMLDivElement | null>): CleanupLayout {
  const [layout, setLayout] = useState<CleanupLayout>("wide");
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => setLayout(layoutFor(el.clientWidth)));
    ro.observe(el);
    setLayout(layoutFor(el.clientWidth));
    return () => ro.disconnect();
  }, [ref]);
  return layout;
}

export function CleanupPage({ tab, map }: { tab: Tab; map?: React.ReactNode }) {
  const st = useCleanup((s) => s.state);
  const view = useCleanup((s) => s.view);
  const planId = useCleanup((s) => s.planId);
  const focus = useCleanup((s) => s.focus);
  const jobId = useCleanup((s) => s.jobId);
  const page = useRef<HTMLDivElement>(null);
  const layout = useLayout(page);
  const [rulesOpen, setRulesOpen] = useState(false);
  // the picture: the 「重复」 colouring, the group in the inspector lit
  useEffect(() => {
    const vid = tab.viewIds[0];
    if (vid) void setViewSettings(vid, { color_mode: "dupes" }).catch(() => undefined);
  }, [tab.viewIds]);
  useEffect(() => {
    if (jobId && focus !== null) focusGroup(jobId, focus, tab.viewIds);
    return () => focusGroup(null, null);
  }, [jobId, focus, tab.viewIds]);
  useEffect(() => {
    if (layout === "wide") setRulesOpen(false);
  }, [layout]);
  const historyOnly = !planId;
  return (
    <div ref={page} className={`content column cl-page ${layout}`} aria-label={t("重复文件清理方案")}>
      <Head historyOnly={historyOnly} />
      <div className="cl-toolbar" role="toolbar" aria-label={t("清理方案")}>
        {!historyOnly && layout !== "wide" && view === "groups" && (
          <button className={`ghost small${rulesOpen ? " on" : ""}`} aria-expanded={rulesOpen} onClick={() => setRulesOpen(!rulesOpen)}>
            <Icon name="sidebar" size={13} /> {t("规则")}
          </button>
        )}
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
        {view !== "groups" && <Receipt />}
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
      {!historyOnly && view === "groups" && <Problems />}
      {view === "groups" && !historyOnly && <GroupsView map={map} layout={layout} rulesOpen={rulesOpen} onCloseRules={() => setRulesOpen(false)} />}
      {view === "pairs" && <PairsView />}
      {view === "volumes" && <VolumesView />}
      {view === "review" && <ReviewPage />}
      {view === "run" && <RunView />}
      {view === "history" && <History />}
      <KeysHelp />
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
      <span className={`cl-pill small${st.approval && st.approval.version === st.version ? " ok" : ""}`}>{st.approval && st.approval.version === st.version ? t("已批准") : t("编辑中")}</span>
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
      <span className="cl-sevs small" aria-label={t("各级风险的份数")}>
        {(["high", "medium", "low", "info"] as const).map((s) => (sm.by_severity[s] ? <Sev key={s} s={s} n={sm.by_severity[s]} word /> : null))}
      </span>
    </div>
  );
}

const filterHelp = (): [string, string][] => [
  ["risk:high  sev:>=medium  risk:R02", t("按风险的等级或编号")],
  ["action:hardlink  action:!leave", t("按处理（! 表示不是）")],
  ["avail:symlink  -avail:hardlink", t("能不能做（- 表示反过来）")],
  ["vol:D:  fs:refs  cross:yes", t("卷、文件系统、组是否跨卷")],
  ['in:"D:\\Photos"  name:*.psd  ext:jpg,png', t("位置、名字、扩展名")],
  ["size:>100MB  copies:>=3  mtime:<2020-01-01", t("大小、份数、修改时间")],
  ["source:manual  source:rule:3  source:risk", t("决定从哪来")],
  ["reviewed:no  acked:no  state:stale", t("过目、确认、状态")],
  ["tag:sync  tag:vcs  tag:backup", t("事实标签")],
  ["Photos", t("其他文字：路径里有它")],
];

function FilterInput() {
  const filter = useCleanup((s) => s.filter);
  const sort = useCleanup((s) => s.sort);
  const [text, setText] = useState(filter);
  const [help, setHelp] = useState(false);
  useEffect(() => setText(filter), [filter]);
  return (
    <>
      <span className="cl-filter-box">
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
        <button className="icon-btn tiny cl-filter-help" aria-label={t("筛选的写法")} aria-expanded={help} title={t("筛选的写法")} onClick={() => setHelp(!help)}>
          ?
        </button>
        {help && (
          <div className="cl-filter-pop" role="dialog" aria-label={t("筛选的写法")}>
            <div className="small muted">{t("几项之间是「并且」；回车生效，Esc 清掉")}</div>
            <table className="small">
              <tbody>
                {filterHelp().map(([code, what]) => (
                  <tr key={code}>
                    <td className="mono">{code}</td>
                    <td>{what}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </span>
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

/** Terms of the filter that were left out, said under the toolbar. */
function Problems() {
  const problems = useCleanup((s) => s.problems);
  if (!problems.length) return null;
  return (
    <div className="cl-problems small" role="alert">
      <Icon name="warning" size={12} />
      <span>
        {problems.join(" · ")}
        {t("（筛选里有读不懂的部分时，一组也不列）")}
      </span>
    </div>
  );
}

function GroupsView({ map, layout, rulesOpen, onCloseRules }: { map?: React.ReactNode; layout: CleanupLayout; rulesOpen: boolean; onCloseRules: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const sheet = useCleanup((s) => s.sheet);
  const focus = useCleanup((s) => s.focus);
  const [side, setSide] = useState<"inspector" | "map">("inspector");
  const center = (
    <div
      className="cl-center"
      ref={ref}
      tabIndex={0}
      onKeyDown={(e) => {
        const el = e.target as HTMLElement;
        if (el.closest("input, select, textarea, [role=listbox], .cl-act")) return;
        if (el.closest("button") && (e.key === " " || e.key === "Enter")) return;
        if (e.key === "/") {
          e.preventDefault();
          document.querySelector<HTMLInputElement>(".cl-filter")?.focus();
          return;
        }
        if (e.key === "Enter" && focus !== null) {
          e.preventDefault();
          if (layout === "narrow") setCleanup({ sheet: true });
          document.querySelector<HTMLElement>(".cl-inspector")?.focus();
          return;
        }
        const key = e.code.startsWith("Digit") ? e.code.slice(5) : e.key;
        if (cleanupKey({ key, shiftKey: e.shiftKey, ctrlKey: e.ctrlKey, metaKey: e.metaKey })) e.preventDefault();
      }}
      aria-label={t("组表（j k 上下，1–9 只保留第几份，t h s c l 改处理，r 过目，x 排除，Ctrl+Z 撤回，? 看全部按键）")}
    >
      <GroupTable />
      <BulkBar />
    </div>
  );
  if (layout === "wide")
    return (
      <div className="cl-body">
        <RuleBar />
        {center}
        <div className="cl-right">
          <CleanupInspector />
          {map && <div className="cl-map">{map}</div>}
        </div>
      </div>
    );
  return (
    <div className="cl-body">
      {rulesOpen && (
        <>
          <div className="cl-scrim" onClick={onCloseRules} />
          <div className="cl-drawer">
            <RuleBar />
          </div>
        </>
      )}
      {center}
      {layout === "medium" ? (
        <div className="cl-right">
          <div className="segmented small cl-side-tabs" role="tablist">
            <button role="tab" aria-selected={side === "inspector"} className={side === "inspector" ? "on" : ""} onClick={() => setSide("inspector")}>
              {t("检查器")}
            </button>
            <button role="tab" aria-selected={side === "map"} className={side === "map" ? "on" : ""} onClick={() => setSide("map")}>
              {t("图")}
            </button>
          </div>
          {side === "inspector" ? <CleanupInspector /> : map && <div className="cl-map">{map}</div>}
        </div>
      ) : (
        sheet &&
        focus !== null && (
          <div className="cl-sheet" role="dialog" aria-label={t("检查器")}>
            <div className="cl-sheet-head">
              <span className="small muted">{t("检查器")}</span>
              <span className="flex" />
              <button className="icon-btn tiny" aria-label={t("关闭")} onClick={() => setCleanup({ sheet: false })}>
                <Icon name="close" size={11} />
              </button>
            </div>
            <CleanupInspector />
          </div>
        )
      )}
    </div>
  );
}

function PairsView() {
  const pairs = useCleanup((s) => s.pairs);
  const total = useCleanup((s) => s.pairsTotal);
  return (
    <div className="cl-pairs">
      <div className="muted small">{t("每组里两份所在的文件夹记一对；两边相对路径一致的子文件夹对合并到上一级")}</div>
      {pairs.map((p) => {
        const [a, b] = pairLabels(p.a, p.b);
        const side = (keep: "a" | "b" | "both") => void edit({ op: "pair_set", pair: { a: p.a, b: p.b, keep, other_action: "", auto: false } });
        return (
          <div key={`${p.a}|${p.b}`} className="cl-pair">
            <div className="cl-pair-sides">
              <PairSide path={p.a} short={a ?? p.a} share={p.a_in_b} other={b ?? p.b} kept={p.keep === "a" || p.keep === "both"} onKeep={() => side("a")} />
              <PairSide path={p.b} short={b ?? p.b} share={p.b_in_a} other={a ?? p.a} kept={p.keep === "b" || p.keep === "both"} onKeep={() => side("b")} />
            </div>
            <div className="small muted">
              {t("{g} 组 · {size}", { g: fmtInt(p.groups), size: fmtSize(p.bytes) })}
              {p.merged > 0 && ` · ${t("合并了 {n} 对子文件夹", { n: fmtInt(p.merged) })}`}
              {p.keep === "mixed" && ` · ${t("现在有的留在这边，有的留在那边")}`}
              {p.rule && ` · ${t("有你定的规则")}`}
            </div>
            <div className="cl-pair-buttons">
              <button className="ghost tiny" onClick={() => side("both")}>
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
        );
      })}
      {!pairs.length && <div className="muted small cl-empty">{t("没有跨文件夹的组")}</div>}
      {total > pairs.length && <div className="muted small">{t("只列出最大的 {n} 对", { n: fmtInt(pairs.length) })}</div>}
    </div>
  );
}

/** The two folders of a pair from where they part (one common folder kept for context):
 * 「…\Pictures\Backup\2021」 and 「…\Pictures\2021」. */
export function pairLabels(a: string, b: string): [string, string] {
  const sep = a.includes("\\") || b.includes("\\") ? "\\" : "/";
  const sa = a.split(/[\\/]+/).filter(Boolean);
  const sb = b.split(/[\\/]+/).filter(Boolean);
  let n = 0;
  while (n < sa.length && n < sb.length && sa[n]!.toLowerCase() === sb[n]!.toLowerCase()) n++;
  if (n < 2) return [a, b];
  const tail = (s: string[]) => `…${sep}${s.slice(n - 1).join(sep)}`;
  return [tail(sa), tail(sb)];
}

function PairSide({ path, short, share, other, kept, onKeep }: { path: string; short: string; share: number; other: string; kept: boolean; onKeep: () => void }) {
  return (
    <div className={`cl-pair-side${kept ? " kept" : ""}`}>
      <span className="mono small cl-pair-path" title={path}>
        {short}
      </span>
      <span className="small muted">{t("{x}% 在 {other} 有一份", { x: Math.round(100 * share), other })}</span>
      {kept ? (
        <span className="cl-pill small ok">{t("现在保留这边")}</span>
      ) : (
        <button className="ghost tiny" onClick={onKeep}>
          {t("保留这边")}
        </button>
      )}
    </div>
  );
}

function VolumesView() {
  const m = useCleanup((s) => s.volumes);
  if (!m) return null;
  if (!m.cells.length) return <div className="muted small cl-empty">{t("没有跨卷的组：这个方案里的副本都在同一个卷上")}</div>;
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
    <div className="cl-receipt small" role="status" aria-live="polite" title={receipt}>
      <span className="cl-receipt-text">{receipt}</span>
      <button className="icon-btn tiny" aria-label={t("关闭")} onClick={() => useCleanup.setState({ receipt: "" })}>
        <Icon name="close" size={10} />
      </button>
    </div>
  );
}

const keys = (): [string, string][] => [
  ["↑ ↓ / j k", t("上一组、下一组")],
  ["→ ←", t("展开、收起这一组")],
  ["1–9", t("这组只保留第几份")],
  ["Shift+1–9", t("第几份也设为保留点")],
  ["t h s c l", t("不保留的份：回收站、硬链接、符号链接、块克隆、不动")],
  ["x", t("这一组不动（排除）")],
  ["r", t("标为已过目，跳到下一组没过目的")],
  ["Space", t("勾选这一组")],
  ["Ctrl+A", t("勾选显示出来的全部组")],
  ["/", t("到筛选框")],
  ["Enter", t("到检查器")],
  ["Ctrl+Z / Ctrl+Shift+Z", t("撤回、重做")],
  ["?", t("这份按键表")],
];

/** The keys of the group table (「?」). Running, undoing and emptying have none. */
function KeysHelp() {
  const open = useCleanup((s) => s.keysHelp);
  if (!open) return null;
  return (
    <div className="modal-back" role="presentation" onClick={() => setCleanup({ keysHelp: false })}>
      <div
        className="modal cl-modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("组表的按键")}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.key === "Escape" && setCleanup({ keysHelp: false })}
      >
        <h3>{t("组表的按键")}</h3>
        <table className="cl-keys small">
          <tbody>
            {keys().map(([k, what]) => (
              <tr key={k}>
                <td>
                  <Kbd>{k}</Kbd>
                </td>
                <td>{what}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="small muted">{t("执行、撤销、清除都没有按键：只在审阅页和清理记录里，经过确认才做")}</p>
        <div className="modal-buttons">
          <button className="primary" autoFocus onClick={() => setCleanup({ keysHelp: false })}>
            {t("知道了")}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Keep the page's lists in step when it becomes visible again. */
export function useCleanupRefresh(): void {
  useEffect(() => void loadGroups(), []);
}

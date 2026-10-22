// Where reports and web panels show up (docs/plugins.md 「报告与插槽」): a tab of their own, a page
// of the details column, a dialog, a card on the start page, a section of the comparison panel, an
// item of the status bar. Each place hands its report a context (the scan, the view, the selected
// node, the comparison) and says why it is unavailable when its `when` does not hold.

import { useEffect, useMemo, useRef } from "react";

import type { PanelInfo, ReportContext, ReportInfo } from "@/protocol/generated";
import { useApp, type AppState, type Tab, set } from "@/state/store";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { openPluginTab, openTarget } from "./open";
import { PluginIcon } from "./icons";
import { PanelFrame } from "./panel/PanelFrame";
import { ReportView } from "./report/ReportView";
import { formatCell } from "./report/format";
import { useReport } from "./report/useReport";
import { pluginName } from "./store";
import { viewFor } from "./nodes";
import { scanSourceOf } from "./sources";
import { whenReason } from "./when";

/** The context of a place about `scanId`: its view, that view's selection (else the zoom root). */
export function useScanCtx(scanId: string | null | undefined, viewId?: string | null): ReportContext {
  const where = useApp((s) => (viewId ? viewId : scanId ? (viewFor(scanId, s)?.viewId ?? null) : null));
  const selected = useApp((s) => (where ? (s.views[where]?.state.selected ?? null) : null));
  const zoom = useApp((s) => (where ? (s.views[where]?.state.zoom_node ?? null) : null));
  const compare = useApp((s) => s.tabs.find((x) => x.compare && x.scanId === scanId)?.compare?.shownId ?? null);
  return useMemo(() => {
    // a real node (not an aggregate block): its key is its id
    const node = selected != null && selected > 0 && selected >>> 30 === 0 ? selected : zoom != null && zoom >= 0 ? zoom : null;
    return { scan_id: scanId ?? null, view_id: where, node, selection: node != null ? [node] : [], compare_id: compare };
  }, [scanId, where, selected, zoom, compare]);
}

/** Why a contribution can't show here (its `when`), or null. */
export function useWhen(when: readonly string[], owner: string | null, ctx: ReportContext): string | null {
  const settings = useApp((s) => s.settings);
  const scan = useApp((s) => (ctx.scan_id ? s.scans[ctx.scan_id] : undefined));
  const plugins = useApp((s) => s.extensions?.plugins);
  const caps = useApp((s) => s.capabilities);
  const compare = useApp((s) => !!s.tabs.find((x) => x.compare && x.scanId === ctx.scan_id));
  if (!when.length) return null;
  return whenReason(when, {
    scan: scan ? { state: scan.state, method: scan.method } : null,
    view: !!ctx.view_id,
    selection: ctx.node != null,
    compare,
    setting: (key) => settings?.values[key],
    capabilities: caps,
    plugins: (plugins ?? []).filter((p) => p.state === "enabled").map((p) => p.id),
    owner,
    sourceId: ctx.scan_id ? scanSourceOf(ctx.scan_id) : null,
  });
}

/** A report where it is placed: fetched with `ctx`, refreshed as its `refresh` says. */
export function ReportSlot({ info, ctx, compact, hideTitle }: { info: ReportInfo; ctx: ReportContext; compact?: boolean; hideTitle?: boolean }) {
  const why = useWhen(info.when, info.plugin, ctx);
  if (why) return <Unavailable text={why} />;
  return <LiveReport info={info} ctx={ctx} compact={compact} hideTitle={hideTitle} />;
}

function LiveReport({ info, ctx, compact, hideTitle }: { info: ReportInfo; ctx: ReportContext; compact?: boolean; hideTitle?: boolean }) {
  const src = useMemo(() => ({ method: info.method, plugin: info.plugin, refresh: info.refresh }), [info.method, info.plugin, info.refresh]);
  const r = useReport(src, ctx);
  return <ReportView {...r} owner={info.plugin} ctx={ctx} manual={info.refresh.includes("manual")} onReload={r.reload} compact={compact} hideTitle={hideTitle} />;
}

function Unavailable({ text }: { text: string }) {
  return (
    <div className="plugin-unavailable muted small">
      <Icon name="info" size={14} />
      <span>{text}</span>
    </div>
  );
}

/** A panel where it is placed, or why it can't be. */
export function PanelSlot({ info, ctx, height }: { info: PanelInfo; ctx: ReportContext; height?: number }) {
  const why = useWhen(info.when, info.plugin, ctx);
  const panels = useApp((s) => s.capabilities.includes("panels.v1"));
  if (!panels) return <Unavailable text={t("这个界面不支持 Web 面板")} />;
  if (why) return <Unavailable text={why} />;
  // a frame of its own per panel: the main process lets a frame show one plugin's files only
  return <PanelFrame key={info.id} info={info} ctx={ctx} height={height} />;
}

/** A report or panel by its full id, or a note that its plugin is off. */
export function Contribution({ kind, id, ctx, compact, height, hideTitle }: { kind: "report" | "panel"; id: string; ctx: ReportContext; compact?: boolean; height?: number; hideTitle?: boolean }) {
  const report = useApp((s) => (kind === "report" ? s.extensions?.reports.find((r) => r.id === id) : undefined));
  const panel = useApp((s) => (kind === "panel" ? s.extensions?.panels.find((p) => p.id === id) : undefined));
  if (report) return <ReportSlot info={report} ctx={ctx} compact={compact} hideTitle={hideTitle} />;
  if (panel) return <PanelSlot info={panel} ctx={ctx} height={height} />;
  return <Unavailable text={t("「{name}」不可用：它的插件已经停用或卸载", { name: id })} />;
}

// ------------------------------------------------------------------ a tab of its own

export function PluginTabPage({ tab }: { tab: Tab }) {
  const p = tab.plugin!;
  const ctx = useScanCtx(tab.scanId);
  const info = useApp((s) => (p.kind === "report" ? s.extensions?.reports.find((r) => r.id === p.id) : s.extensions?.panels.find((x) => x.id === p.id)));
  const plugin = info?.plugin ?? p.id.slice(0, p.id.lastIndexOf("."));
  const scanTitle = useApp((s) => (tab.scanId ? s.tabs.find((x) => x.kind === "scan" && x.scanId === tab.scanId)?.title : undefined));
  return (
    <div className="plugin-page">
      <header className="plugin-page-head">
        <PluginIcon plugin={plugin} name={info?.icon} size={18} />
        <div className="plugin-page-title">
          <h1>{info?.title ?? tab.title}</h1>
          <div className="muted small">
            {pluginName(plugin)}
            {scanTitle ? ` · ${scanTitle}` : ""}
          </div>
        </div>
      </header>
      <div className={`plugin-page-body${p.kind === "panel" ? " panel" : ""}`}>
        <Contribution kind={p.kind} id={p.id} ctx={ctx} hideTitle />
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ dialog

export function PluginDialogHost() {
  const d = useApp((s) => s.pluginDialog);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!d) return;
    box.current?.focus();
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !useApp.getState().confirm) {
        e.stopPropagation();
        set({ pluginDialog: null });
      }
    };
    window.addEventListener("keydown", key, true);
    return () => window.removeEventListener("keydown", key, true);
  }, [d]);
  if (!d) return null;
  const close = () => set({ pluginDialog: null });
  return (
    <div className="modal-back" role="presentation" onMouseDown={close}>
      <div ref={box} tabIndex={-1} className="modal plugin-dialog" role="dialog" aria-modal="true" aria-labelledby="sk-plugin-dialog-title" onMouseDown={(e) => e.stopPropagation()}>
        <div className="plugin-dialog-head">
          <h3 id="sk-plugin-dialog-title">{d.title}</h3>
          <span className="muted small">{pluginName(d.plugin)}</span>
          <span className="flex" />
          <button className="icon-btn small" aria-label={t("关闭")} title={t("关闭")} onClick={close}>
            <Icon name="close" />
          </button>
        </div>
        <div className="plugin-dialog-body">
          {d.document ? (
            <ReportView report={d.document} loading={false} error="" owner={d.plugin} ctx={d.ctx} hideTitle />
          ) : d.report ? (
            <Contribution kind="report" id={d.report.id} ctx={d.ctx} hideTitle />
          ) : d.panel ? (
            <Contribution kind="panel" id={d.panel.id} ctx={d.ctx} />
          ) : null}
        </div>
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ the details column

/** The plugin pages of the details column (slot "inspector"): reports and panels. */
export function inspectorPages(s: AppState): { id: string; kind: "report" | "panel"; title: string; icon: string; plugin: string }[] {
  const ext = s.extensions;
  if (!ext) return EMPTY_PAGES;
  const reports = ext.reports.filter((r) => r.slot === "inspector").map((r) => ({ id: r.id, kind: "report" as const, title: r.title, icon: r.icon, plugin: r.plugin, order: r.order }));
  const panels = ext.panels.filter((p) => p.slot === "inspector").map((p) => ({ id: p.id, kind: "panel" as const, title: p.title, icon: p.icon, plugin: p.plugin, order: 0 }));
  const all = [...reports, ...panels].sort((a, b) => a.order - b.order);
  const key = all.map((x) => `${x.kind}:${x.id}:${x.title}:${x.icon}`).join("|");
  if (pagesCache?.key === key) return pagesCache.pages;
  pagesCache = { key, pages: all };
  return all;
}
const EMPTY_PAGES: ReturnType<typeof inspectorPages> = [];
let pagesCache: { key: string; pages: ReturnType<typeof inspectorPages> } | null = null;

/** A plugin page of the details column, following the selection of its view. */
export function InspectorPluginPage({ id, viewId }: { id: string; viewId: string }) {
  const scanId = useApp((s) => s.views[viewId]?.state.scan_id ?? null);
  const ctx = useScanCtx(scanId, viewId);
  const panel = useApp((s) => !!s.extensions?.panels.find((p) => p.id === id && p.slot === "inspector"));
  return <Contribution kind={panel ? "panel" : "report"} id={id} ctx={ctx} compact hideTitle />;
}

// ------------------------------------------------------------------ the start page

/** Start page cards (slot "start") and quick ways to open plugin pages that need no scan (history
 * reports, tab panels). */
export function StartPluginCards() {
  const ext = useApp((s) => s.extensions);
  const ctx = useMemo<ReportContext>(() => ({ scan_id: null, view_id: null, node: null, selection: [] }), []);
  if (!ext) return null;
  const cards = ext.reports.filter((r) => r.slot === "start");
  // what needs no scan: history reports (snapshots), and tab panels without conditions
  const tabs = [...ext.reports.filter((r) => r.slot === "history"), ...ext.panels.filter((p) => p.slot === "tab" && !p.when.length)];
  if (!cards.length && !tabs.length) return null;
  return (
    <section className="start-plugins" aria-label={t("插件")}>
      <div className="start-title">
        <h2>{t("插件")}</h2>
      </div>
      {cards.map((r) => (
        <div key={r.id} className="start-plugin-card">
          <div className="start-plugin-card-head">
            <PluginIcon plugin={r.plugin} name={r.icon} />
            <strong>{r.title}</strong>
            <span className="muted small">{pluginName(r.plugin)}</span>
          </div>
          <ReportSlot info={r} ctx={ctx} compact hideTitle />
        </div>
      ))}
      {tabs.length > 0 && (
        <div className="start-plugin-links">
          {tabs.map((x) => (
            <button key={x.id} className="ghost small" onClick={() => openPluginTab("method" in x ? "report" : "panel", x.id, x.title)}>
              <PluginIcon plugin={x.plugin} name={x.icon} size={14} />
              {x.title}
            </button>
          ))}
        </div>
      )}
    </section>
  );
}

// ------------------------------------------------------------------ the comparison panel

/** The plugin sections of the 「快照对比」 panel (slot "compare", thin). */
export function CompareSections({ tab }: { tab: Tab }) {
  const reports = useApp((s) => s.extensions?.reports);
  const ctx = useScanCtx(tab.scanId, tab.compare?.viewId);
  const list = useMemo(() => (reports ?? []).filter((r) => r.slot === "compare"), [reports]);
  if (!list.length) return null;
  return (
    <>
      {list.map((r) => (
        <section key={r.id} className="cmp-plugin">
          <h3 className="cmp-plugin-title">
            <PluginIcon plugin={r.plugin} name={r.icon} size={14} />
            {r.title}
          </h3>
          <ReportSlot info={r} ctx={ctx} compact hideTitle />
        </section>
      ))}
    </>
  );
}

// ------------------------------------------------------------------ the status bar

/** Status bar items (slot "status", thin): the first number of the report; a click opens it. */
export function StatusPluginItems({ scanId }: { scanId: string | null }) {
  const reports = useApp((s) => s.extensions?.reports);
  const list = useMemo(() => (reports ?? []).filter((r) => r.slot === "status"), [reports]);
  const ctx = useScanCtx(scanId);
  return (
    <>
      {list.map((r) => (
        <StatusItem key={r.id} info={r} ctx={ctx} />
      ))}
    </>
  );
}

function StatusItem({ info, ctx }: { info: ReportInfo; ctx: ReportContext }) {
  const why = useWhen(info.when, info.plugin, ctx);
  const src = useMemo(() => ({ method: info.method, plugin: info.plugin, refresh: info.refresh.length ? info.refresh : ["scan"] }), [info]);
  const r = useReport(why ? null : src, ctx);
  if (why) return null;
  const stat = r.report?.blocks.find((b) => b.type === "stats");
  const first = stat && stat.type === "stats" ? stat.items[0] : undefined;
  const text = first ? `${first.label} ${formatCell(first.value, first.unit)}` : r.error ? t("{name}：出错", { name: info.title }) : info.title;
  return (
    <button className="status-item" title={t("{name}（{plugin}）", { name: info.title, plugin: pluginName(info.plugin) })} onClick={() => openTarget(`report:${info.id}`, ctx, "dialog")}>
      <PluginIcon plugin={info.plugin} name={info.icon} size={12} />
      <span>{text}</span>
    </button>
  );
}

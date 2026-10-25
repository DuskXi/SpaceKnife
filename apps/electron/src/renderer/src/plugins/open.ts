// Opening what plugins add (an `open` action, a command, the plugins page): a report or a web panel
// where its slot says (a tab of its own, a page of the details column, a dialog), or a plugin's
// settings.

import type { ReportContext } from "@/protocol/generated";
import { newTabId, type Tab, activeTab, get, set, toast } from "@/state/store";
import { openSettingsTab } from "@/state/actions";
import { t } from "@/lib/i18n";

import { panelInfo, reportInfo } from "./store";

/** An `open` action's args: what the report or panel gets as its context's args. */
const argsOf = (ctx: ReportContext | undefined): Record<string, unknown> | undefined => {
  const a = ctx?.args as Record<string, unknown> | undefined;
  return a && Object.keys(a).length ? a : undefined;
};

/** A plugin tab for a report or a panel about the active tab's scan; an open one is brought forward
 * (and told the new args, when it is opened with some: a panel hears it as sk.onContext). */
export function openPluginTab(kind: "report" | "panel", id: string, title: string, args?: Record<string, unknown>): void {
  const s = get();
  const scanId = activeTab(s).scanId;
  const open = s.tabs.find((x) => x.kind === "plugin" && x.plugin?.kind === kind && x.plugin.id === id && x.scanId === scanId);
  if (open) {
    if (!args) return set({ active: open.id });
    return set((x) => ({ active: open.id, tabs: x.tabs.map((tb) => (tb.id === open.id ? { ...tb, plugin: { kind, id, args } } : tb)) }));
  }
  const tab: Tab = { id: newTabId(), kind: "plugin", scanId, viewIds: [], focusedPane: 0, title, plugin: { kind, id, args } };
  set((x) => ({ tabs: [...x.tabs, tab], active: tab.id }));
}

/** A report or panel in the dialog, about `ctx`. */
export function openPluginDialog(kind: "report" | "panel", id: string, plugin: string, title: string, ctx: ReportContext): void {
  set({ pluginDialog: { plugin, title, report: kind === "report" ? { id } : null, panel: kind === "panel" ? { id } : null, ctx } });
}

/** A page of the details column (slot "inspector"); elsewhere than a scan tab, its own tab. */
function openInspectorPage(kind: "report" | "panel", id: string, title: string, args?: Record<string, unknown>): void {
  if (activeTab().kind !== "scan") return openPluginTab(kind, id, title, args);
  set((s) => ({
    inspectorHidden: false,
    basketOpen: false,
    inspectorPage: `plugin:${id}`,
    ...(args ? { inspectorArgs: { ...s.inspectorArgs, [id]: args } } : {}),
  }));
}

/** `report:<id>`, `panel:<id>` or `settings:<plugin>`; `slot` puts it elsewhere than it says
 * (the status bar opens its report in the dialog). */
export function openTarget(target: string, ctx: ReportContext, slot?: "tab" | "dialog"): void {
  const [what, ...rest] = target.split(":");
  const id = rest.join(":");
  if (what === "settings") {
    openSettingsTab("plugins");
    set({ pluginOpen: id || null });
    return;
  }
  if (what === "report") {
    const info = reportInfo(id);
    if (!info) return toast(t("找不到「{name}」：它的插件可能已经停用", { name: id }));
    const where = slot ?? info.slot;
    if (where === "dialog" || where === "start" || where === "status" || where === "compare") return openPluginDialog("report", id, info.plugin, info.title, ctx);
    if (where === "inspector") return openInspectorPage("report", id, info.title, argsOf(ctx));
    return openPluginTab("report", id, info.title, argsOf(ctx));
  }
  if (what === "panel") {
    const info = panelInfo(id);
    if (!info) return toast(t("找不到「{name}」：它的插件可能已经停用", { name: id }));
    const where = slot ?? info.slot;
    if (where === "dialog") return openPluginDialog("panel", id, info.plugin, info.title, ctx);
    if (where === "inspector") return openInspectorPage("panel", id, info.title, argsOf(ctx));
    return openPluginTab("panel", id, info.title, argsOf(ctx));
  }
  toast(t("不认识的打开目标：{target}", { target }));
}

// Running an action (docs/plugins.md 「动作」): what a command, a report's button, a table row or a
// field does. Commands of the app itself carry the generic ones too (view, cycle, toggle); the rest
// of them are `builtin` actions, handled by BUILTIN_ACTIONS (state/commands.ts) by command id.
//
// An action declared by a plugin (`owner`) may only call that plugin's methods, change its own
// settings (or a few display settings of the app), and needs host.open / clipboard for opening and
// copying; deleting is never more than putting files in the delete basket.

import type {
  ActionContext,
  BuiltinAction,
  CallAction,
  CleanupAction,
  CommandAction,
  CompareOpen,
  CycleAction,
  FilterAction,
  LabeledAction,
  NodeAction,
  OpenAction,
  Report,
  ReportContext,
  ScanSourceAction,
  SettingAction,
  Toast,
  ToggleAction,
  UrlAction,
  ViewAction,
  ViewSettingsParams,
} from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { basketAdd, call, editFilter, setViewSettings, updateSettings, viewCall } from "@/state/actions";
import { activeTab, focusedViewId, get, set, setting, toast } from "@/state/store";
import { cycleDirection, openCompare } from "@/state/compare";
import { t } from "@/lib/i18n";

import { callPlugin, pluginOf } from "./client";
import { colorChoices, nextColorMode } from "./colors";
import { ask } from "./confirm";
import { openTarget } from "./open";
import { granted, pluginName } from "./store";

export type Action = LabeledAction["action"];

/** Where an action runs and on what. */
export interface RunContext {
  /** the plugin that declared it; null for the app's own commands */
  owner: string | null;
  /** the node it is about (a menu's block, a report's row); else the view's selection */
  node?: number | null;
  scanId?: string | null;
  viewId?: string | null;
  /** rows chosen in a report table */
  selection?: number[];
  /** a cycle the other way (Shift with its key) */
  reverse?: boolean;
  /** commands run by commands: stop after a few */
  depth?: number;
  /** the view's tab is not brought forward (a web panel acting on its view) */
  stay?: boolean;
  /** given by who runs it (a panel's sk.host.command(id, args)): added to the action's own args */
  args?: Record<string, unknown>;
}

type Handler<A> = (a: A, rc: RunContext) => Promise<void> | void;

/** An action's args with what its runner gives (the runner's win). */
const withArgs = (own: Record<string, unknown> | undefined, rc: RunContext): Record<string, unknown> => ({ ...(own ?? {}), ...(rc.args ?? {}) });

const EXPORT_SCOPES = new Set(["zoom", "all", "selection"]);

/** report.export's args: the format, then the scope, options and filter the dialog opens with; the
 * node a menu was opened on (or a report table's chosen rows) is what 「选中项」 exports. */
function askExport(args: Record<string, unknown>, rc: RunContext): void {
  const format = typeof args.format === "string" && args.format ? args.format : undefined;
  const scope = typeof args.scope === "string" && EXPORT_SCOPES.has(args.scope) ? (args.scope as "zoom" | "all" | "selection") : undefined;
  const options: Record<string, boolean | number | string> = {};
  if (args.options && typeof args.options === "object") {
    for (const [k, v] of Object.entries(args.options as Record<string, unknown>)) if (typeof v === "boolean" || typeof v === "number" || typeof v === "string") options[k] = v;
  }
  const nodes = rc.selection?.length ? rc.selection : rc.node != null && rc.node > 0 ? [rc.node] : undefined;
  set({
    exportOpen: true,
    exportPick: format,
    exportAsk: {
      scope: scope ?? (nodes ? "selection" : undefined),
      options,
      use_filter: typeof args.use_filter === "boolean" ? args.use_filter : undefined,
      nodes,
      viewId: rc.viewId ?? null,
    },
  });
}

/** The app's own commands without a generic action, by id (state/commands.ts fills it). */
export const BUILTIN_ACTIONS: Record<string, (rc: RunContext) => Promise<void> | void> = {};

/** What actions need of the command runner (state/commands.ts fills it in: it imports this module,
 * so this one can't import it back while both load). */
export const COMMANDS: {
  runShortcut(
    id: string,
    opts: { depth?: number; node?: number | null; viewId?: string | null; scanId?: string | null; stay?: boolean; args?: Record<string, unknown> },
  ): Promise<void>;
  cycleVisual(viewId: string, dir: 1 | -1): Promise<void>;
  pickVisual(viewId: string, id: string): Promise<void>;
} = {
  runShortcut: async () => undefined,
  cycleVisual: async () => undefined,
  pickVisual: async () => undefined,
};

/** Display settings a plugin's `setting` action may change besides its own (plugin.<id>.*). */
export const PLUGIN_SETTABLE = new Set([
  "appearance.halo_levels",
  "appearance.hover_highlight",
  "appearance.mouse_trail",
  "appearance.selection_shadow",
  "appearance.trash_effect",
  "general.hover_dates",
  "general.hover_age",
  "treemap.flame_origin",
]);

/** The selected node of the focused view (a real node, not an aggregate block). */
function selection(): { scanId: string; viewId: string; node: number } | null {
  const viewId = focusedViewId();
  const view = viewId ? get().views[viewId] : undefined;
  const st = view?.state;
  if (!viewId || !st || st.selected == null || st.selected <= 0 || st.selected >>> 30 !== 0) return null;
  return { scanId: st.scan_id, viewId, node: st.selected };
}

/** What a plugin's method is told about where the user is. */
export function actionContext(rc: RunContext, args: Record<string, unknown> = {}): ActionContext & ReportContext {
  const s = get();
  const tab = activeTab(s);
  const viewId = rc.viewId ?? focusedViewId(s);
  const sel = selection();
  const node = rc.node ?? sel?.node ?? null;
  const view = viewId ? s.views[viewId] : undefined;
  return {
    scan_id: rc.scanId ?? view?.state.scan_id ?? tab.scanId ?? null,
    view_id: viewId,
    node,
    selection: rc.selection ?? (node != null ? [node] : []),
    compare_id: tab.compare?.shownId ?? null,
    args,
    // where the user is: the zoom root and the filter (a command can do what the view shows)
    zoom_node: view?.state.zoom_node ?? null,
    filter: view?.filter?.active ? view.filter.text : "",
  };
}

const errorText = (err: unknown) => (err instanceof RpcError ? err.message : String(err));

/** A plugin action that needs a permission its plugin was not granted: say so, do nothing. */
function allowed(rc: RunContext, permission: string): boolean {
  if (!rc.owner || rc.owner.startsWith("builtin.")) return true;
  if (granted(rc.owner).includes(permission)) return true;
  toast(t("插件「{plugin}」没有「{permission}」权限", { plugin: pluginName(rc.owner), permission }));
  return false;
}

/** What a call answered: a report opens in the dialog, a toast shows, a cleanup goes to review, a
 * comparison opens (snapshots.read). */
async function handleResult(value: unknown, rc: RunContext, ctx: ReportContext): Promise<void> {
  if (!value || typeof value !== "object") return;
  const typed = value as { type?: string };
  if (typed.type === "report") {
    const report = value as Report;
    set({ pluginDialog: { plugin: rc.owner ?? "", title: report.title || pluginName(rc.owner), report: null, panel: null, document: report, ctx } });
  } else if (typed.type === "toast") {
    const tv = value as Toast;
    toast(tv.text, tv.tone === "danger" || tv.tone === "warn" ? 5000 : 2500);
  } else if (typed.type === "cleanup") {
    const { reviewCleanup } = await import("./cleanup");
    reviewCleanup(rc.owner ?? "", ctx.scan_id ?? "", value as never);
  } else if (typed.type === "compare") {
    if (!allowed(rc, "snapshots.read")) return;
    const c = value as CompareOpen;
    if (typeof c.base !== "string" || !c.base) return;
    await openCompare({ base: c.base, target: typeof c.target === "string" && c.target ? c.target : "now", scanId: c.scan_id ?? ctx.scan_id });
  }
}

const HANDLERS: { [K in Action["kind"]]: Handler<Extract<Action, { kind: K }>> } = {
  async builtin(a: BuiltinAction, rc) {
    const run = BUILTIN_ACTIONS[a.name];
    if (!run) return toast(t("这个命令还没有接入"));
    await run(rc);
  },

  async call(a: CallAction, rc) {
    const target = pluginOf(a.method);
    if (rc.owner && target !== rc.owner) return toast(t("插件只能调用它自己的方法"));
    const ctx = actionContext(rc, withArgs(a.args, rc));
    try {
      await handleResult(await callPlugin(a.method, ctx), rc, ctx);
    } catch (err) {
      if (!(err instanceof RpcError && err.code === -32800)) toast(errorText(err));
    }
  },

  open(a: OpenAction, rc) {
    openTarget(a.target, actionContext(rc, withArgs(a.args, rc)));
  },

  async command(a: CommandAction, rc) {
    if ((rc.depth ?? 0) > 3) return;
    const args = withArgs(a.args, rc);
    // 「导出报告…」 opened on a format, a scope, options: { command = "report.export", args = { format = "<id>", … } };
    // from a context menu (or a report's chosen rows), on that node
    if (a.command === "report.export" && (Object.keys(args).length || rc.node != null || rc.selection?.length)) return askExport(args, rc);
    // the command runs on what this one runs on (the menu's node, the panel's view), with the args
    await COMMANDS.runShortcut(a.command, {
      depth: (rc.depth ?? 0) + 1,
      node: rc.node,
      viewId: rc.viewId,
      scanId: rc.scanId,
      stay: rc.stay,
      args: Object.keys(args).length ? args : undefined,
    });
  },

  async view(a: ViewAction, rc) {
    const viewId = rc.viewId ?? focusedViewId();
    if (!viewId) return;
    const { visual, ...rest } = a.settings as ViewSettingsParams & { visual?: string };
    if (visual) await COMMANDS.pickVisual(viewId, visual);
    if (Object.keys(rest).length) await setViewSettings(viewId, rest);
    if (!rc.stay) showViewTab(viewId);
  },

  async cycle(a: CycleAction, rc) {
    const viewId = rc.viewId ?? focusedViewId();
    const st = viewId ? get().views[viewId]?.state : undefined;
    if (!viewId || !st) return;
    const dir = (Math.sign(a.step || 1) * (rc.reverse ? -1 : 1)) as 1 | -1;
    if (a.what === "visual") return COMMANDS.cycleVisual(viewId, dir);
    if (st.compare) return cycleDirection(viewId); // a comparison's C: the changes shown (W6)
    const s = get();
    const layout = s.visuals.find((v) => v.id === st.visual)?.layout ?? s.views[viewId]?.info?.layout ?? null;
    await setViewSettings(viewId, { color_mode: nextColorMode(colorChoices(s.extensions, layout), st.color_mode, dir) });
  },

  async toggle(a: ToggleAction, rc) {
    const viewId = rc.viewId ?? focusedViewId();
    const st = viewId ? get().views[viewId]?.state : undefined;
    if (!viewId || !st) return;
    // a comparison has no free or unknown blocks; a scan without a recycle bin has nothing to show
    if (st.compare && (a.name === "show_free" || a.name === "show_unknown")) return;
    if (a.name === "show_trash" && !st.has_trash) return;
    await setViewSettings(viewId, { [a.name]: !st[a.name] });
  },

  async filter(a: FilterAction, rc) {
    const viewId = rc.viewId ?? focusedViewId();
    if (!viewId) return toast(t("需要先完成扫描"));
    await editFilter(viewId, a.expr, null, true);
    if (!rc.stay) showViewTab(viewId); // from a report's tab: the user sees what the filter did
  },

  async scan_source(a: ScanSourceAction, rc) {
    const { startSourceScan } = await import("./sources");
    let uri = a.uri;
    if (uri === "$node") {
      const ctx = actionContext(rc);
      if (!ctx.scan_id || ctx.node == null || ctx.node <= 0) return toast(t("没有选中项"));
      const r = await call("tree.node", { scan_id: ctx.scan_id, node: ctx.node });
      if (!r.on_disk) return toast(t("这个位置不在磁盘上"));
      uri = r.path;
    }
    const known = get().extensions?.sources.find((x) => x.id === a.source);
    if (!known) return toast(t("没有这个扫描来源：{id}", { id: a.source }));
    await startSourceScan(known, uri);
  },

  async setting(a: SettingAction, rc) {
    const own = rc.owner ? a.key.startsWith(`plugin.${rc.owner}.`) : true;
    if (!own && !PLUGIN_SETTABLE.has(a.key)) return toast(t("插件不能修改这项设置：{key}", { key: a.key }));
    const value = a.toggle ? !setting(get(), a.key, false) : a.value;
    if (value === null || value === undefined) return;
    await updateSettings({ [a.key]: value });
  },

  async cleanup(a: CleanupAction, rc) {
    const target = pluginOf(a.method);
    if (rc.owner && target !== rc.owner) return toast(t("插件只能调用它自己的方法"));
    const { startCleanup } = await import("./cleanup");
    await startCleanup(a.method, target ?? rc.owner ?? "", actionContext(rc, withArgs(a.args, rc)));
  },

  async node(a: NodeAction, rc) {
    const ctx = actionContext(rc);
    const scanId = ctx.scan_id;
    const node = ctx.node;
    if (!scanId || node == null || node <= 0) return toast(t("没有选中项"));
    const viewId = ctx.view_id ?? null;
    switch (a.op) {
      case "select":
        await viewCall(viewId, "view.reveal", { key: node });
        return;
      case "zoom":
        await viewCall(viewId, "view.zoom", { key: node });
        return;
      case "basket":
        await basketAdd(scanId, node, rc.owner);
        return;
      case "copy_path": {
        if (!allowed(rc, "clipboard")) return;
        const r = await call("tree.node", { scan_id: scanId, node });
        window.sk.shell.copyText(r.path);
        toast(t("已复制"));
        return;
      }
      default: {
        if (!allowed(rc, "host.open")) return;
        const r = await call("tree.node", { scan_id: scanId, node });
        if (!r.on_disk) {
          const { notOnDisk } = await import("@/shell/Inspector");
          return toast(notOnDisk());
        }
        if (a.op === "reveal") window.sk.shell.showItemInFolder(r.path);
        else if (a.op === "open") void window.sk.shell.openPath(r.path);
        else window.sk.shell.openTerminal(r.node.kind === "dir" ? r.path : r.path.replace(/[\\/][^\\/]*$/, ""));
      }
    }
  },

  async url(a: UrlAction, rc) {
    if (!/^https:\/\//i.test(a.url)) return;
    if (!allowed(rc, "host.open")) return;
    const yes = await ask({
      title: t("打开网页"),
      body: [rc.owner ? t("插件「{plugin}」要在浏览器里打开：", { plugin: pluginName(rc.owner) }) : t("要在浏览器里打开："), a.url],
      ok: t("打开"),
    });
    if (yes) window.sk.shell.openUrl(a.url);
  },
};

/** Bring the scan tab of `viewId` forward when the user is on another tab (a report's, a panel's). */
function showViewTab(viewId: string): void {
  const s = get();
  const tab = s.tabs.find((x) => x.kind === "scan" && x.viewIds.includes(viewId));
  if (tab && s.active !== tab.id) set({ active: tab.id });
}

/** Run an action; never throws (what went wrong is shown in a toast). */
export async function runAction(action: Action | null | undefined, rc: RunContext): Promise<void> {
  if (!action) return;
  const run = HANDLERS[action.kind] as Handler<Action> | undefined;
  if (!run) return toast(t("这个界面不认识这种动作：{kind}", { kind: action.kind }));
  try {
    await run(action, rc);
  } catch (err) {
    toast(errorText(err));
  }
}

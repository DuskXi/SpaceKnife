// Runs palette commands by id (W4). The backend decides which commands exist, their names, keys,
// status, availability and (plugins.v1) what most of them do: their `action` (plugins/actions.ts).
// The app's own commands without a generic action are `builtin`: BUILTIN_ACTIONS below maps their
// id to what they do here. Keys come from the same commands (plugins/keymap.ts).

import { focusFilter } from "@/shell/FilterBox";
import { notOnDisk, revealLabel } from "@/shell/Inspector";
import { t } from "@/lib/i18n";
import { Kind } from "@/visual/frame";
import { registeredLayouts } from "@/visual/registry";
import type { PaletteCommand, PaletteContext } from "@/protocol/generated";
import { BUILTIN_ACTIONS, COMMANDS, type RunContext, runAction } from "@/plugins/actions";
import { sourceScanOf, startSourceScan } from "@/plugins/sources";
import { visualWhen } from "@/plugins/whenEnv";

import {
  basketAdd,
  call,
  checkUpdates,
  closeTab,
  moveActiveTab,
  newStartTab,
  openMachineTab,
  openSettingsTab,
  openSnapshot,
  continueScan,
  pauseScan,
  rescanFolder,
  resumeScan,
  setViewSettings,
  showUpdate,
  startScan,
  toggleSplit,
  viewCall,
} from "./actions";
import { enterCompare, refreshCompareSources } from "./compare";
import { openHelp } from "./help";
import { type AppState, activeTab, basketCount, focusedViewId, get, set, toast, toggleLog } from "./store";
import { nextVisual, offered } from "./visuals";

/** The selected node of the focused view (not an aggregate block), or null. */
export function selectedNode(): { scanId: string; viewId: string; node: number } | null {
  const viewId = focusedViewId();
  const view = viewId ? get().views[viewId] : undefined;
  const frame = view?.frame;
  const row = frame?.info.selected_row;
  if (!viewId || !view || !frame || row == null || frame.kind[row]! >= Kind.SMALL || frame.node[row]! <= 0) return null;
  return { scanId: view.state.scan_id, viewId, node: frame.node[row]! };
}

/** V / Shift+V and 「切换可视化」 (view.cycle_visual's cycle action): the view's next (dir 1) or
 * previous visualization among those it can show here. */
export async function cycleVisual(viewId: string, dir: 1 | -1): Promise<void> {
  const s = get();
  const st = s.views[viewId]?.state;
  if (!st) return;
  const next = nextVisual(s.visuals, registeredLayouts(), st.visual, dir, st.compare, visualWhen(s, st.scan_id, viewId));
  if (next) await switchVisual(viewId, next);
}

/** The toolbar's visualization segments and 「可视化：…」: one this view can't show says why instead. */
export async function pickVisual(viewId: string, id: string): Promise<void> {
  const s = get();
  const st = s.views[viewId]?.state;
  if (!st || st.visual === id) return;
  const item = offered(s.visuals, registeredLayouts(), st.compare, visualWhen(s, st.scan_id, viewId)).find((o) => o.info.id === id);
  if (item?.reason) return toast(item.reason);
  await switchVisual(viewId, id);
}

/** Zoom, selection, filter and history stay (they are the backend view's). The view's context menu
 * closes: it points into the drawing being replaced. */
async function switchVisual(viewId: string, id: string): Promise<void> {
  if (get().menu?.viewId === viewId) set({ menu: null });
  await setViewSettings(viewId, { visual: id });
}

/** Where the user is, as the backend's commands see it (palette.query, palette.check). */
export function paletteContext(s: AppState = get(), on?: { viewId?: string | null; scanId?: string | null }): PaletteContext {
  // a web panel's command runs on the panel's view, whatever tab is in front
  const view = on?.viewId ? s.views[on.viewId] : undefined;
  if (view) return { tab: "scan", scan_id: on?.scanId ?? view.state.scan_id, view_id: on!.viewId!, basket: basketCount(s) };
  const tab = activeTab(s);
  // the help and disk tabs are, to the backend's commands, tabs without a scan like the settings tab;
  // a plugin's tab is about a scan (its own) but has no view
  const kind = tab.kind === "help" || tab.kind === "machine" ? "settings" : tab.kind === "plugin" ? (tab.scanId ? "scan" : "settings") : tab.kind;
  return { tab: kind, scan_id: tab.scanId ?? null, view_id: focusedViewId(s), basket: basketCount(s) };
}

/** How a key or another command runs one: `quiet` says nothing when the command is unavailable
 * (single keys of the view: Backspace at the top, a tag key with nothing selected). */
export interface ShortcutOptions {
  quiet?: boolean;
  /** a cycle the other way (Shift with its key) */
  reverse?: boolean;
  depth?: number;
  /** what it acts on, when not the focused view's selection (a context menu's block) */
  node?: number | null;
  viewId?: string | null;
  scanId?: string | null;
  /** the view's tab is not brought forward (a web panel acting on its view) */
  stay?: boolean;
}

/** A command's keyboard shortcut (plugins/keymap.ts) or a command run by an action: it runs where
 * the palette would offer it, and elsewhere says why not, as the palette's row does. Which commands
 * are available is the backend's call (palette.check), so the key and the palette never disagree. */
export async function runShortcut(id: string, opts: ShortcutOptions = {}): Promise<void> {
  const c = await call("palette.check", { id, context: paletteContext(get(), opts) });
  if (!c.enabled) {
    if (!opts.quiet) toast(c.status ? t("{command}：{reason}", { command: c.name, reason: c.status }) : c.name);
    return;
  }
  await runCommand(id, c, opts);
}

/** Run a command: its action (from the backend), else the app's own handler of that id. `cmd` is
 * the command as the palette or palette.check gave it; otherwise the one of palette.commands. */
export async function runCommand(id: string, cmd?: PaletteCommand | null, opts: ShortcutOptions = {}): Promise<void> {
  const known = cmd ?? get().commands.find((c) => c.id === id) ?? null;
  const rc: RunContext = { owner: known?.plugin ?? null, reverse: opts.reverse, depth: opts.depth, node: opts.node, viewId: opts.viewId, scanId: opts.scanId, stay: opts.stay };
  await runAction(known?.action ?? { kind: "builtin", name: id }, rc);
}

/** The path of the selected node with whether it exists on disk (reveal, terminal). */
async function selectionTarget(): Promise<{ path: string; dir: boolean; onDisk: boolean } | null> {
  const sel = selectedNode();
  if (!sel) return null;
  const r = await call("tree.node", { scan_id: sel.scanId, node: sel.node });
  return { path: r.path, dir: r.node.kind === "dir", onDisk: r.on_disk };
}

const tabNow = () => activeTab(get());
const viewNow = () => focusedViewId(get());
const stateNow = () => {
  const v = viewNow();
  return v ? get().views[v]?.state : undefined;
};

// what actions run through this module (plugins/actions.ts can't import it)
Object.assign(COMMANDS, { runShortcut: (id: string, o: ShortcutOptions) => runShortcut(id, o), cycleVisual, pickVisual });

// The app's own commands, by id: what each one does in this window (`builtin` actions).
Object.assign(BUILTIN_ACTIONS, {
  "scan.rescan_all": async () => {
    const tab = tabNow();
    const scan = tab.scanId ? get().scans[tab.scanId] : undefined;
    // a plugin's scan source lists its URI again; a disk scan reads its roots again
    const from = tab.scanId ? sourceScanOf(tab.scanId) : null;
    if (from) await startSourceScan({ id: from.source }, from.uri, tab.id);
    else if (scan) await startScan({ roots: scan.roots.map((r) => r.path) }, tab.id);
  },
  "view.detail_up": async () => {
    const st = stateNow();
    if (st) await setViewSettings(viewNow(), { detail: Math.min(15, st.detail + 1) });
  },
  "view.detail_down": async () => {
    const st = stateNow();
    if (st) await setViewSettings(viewNow(), { detail: Math.max(1, st.detail - 1) });
  },
  "snapshot.save": async () => {
    const tab = tabNow();
    if (!tab.scanId) return;
    try {
      const info = await call("snapshot.save", { scan_id: tab.scanId });
      if (tab.compare) await refreshCompareSources(tab.id);
      toast(t("快照已保存：{name}", { name: info.path.split(/[\\/]/).pop() ?? info.path }));
    } catch (err) {
      toast(t("保存快照失败：{error}", { error: String(err) }));
    }
  },
  "snapshot.compare": () => enterCompare(),
  "report.export": () => set({ exportOpen: true }),
  "basket.open": () => set({ basketOpen: !get().basketOpen }),
  "snapshot.open": async () => {
    const path = await window.sk.dialog.openSnapshot();
    if (path) await openSnapshot(path);
  },
  "app.settings": () => openSettingsTab(),
  "app.help": () => openHelp(),
  "app.log": () => toggleLog(),
  "app.check_updates": () => checkUpdates(),
  "app.show_update": () => showUpdate(),
  "app.about": () => openSettingsTab("about"),
  "app.disks": () => openMachineTab(),
  "nav.back": async () => void (await viewCall(viewNow(), "view.back")),
  "nav.forward": async () => void (await viewCall(viewNow(), "view.forward")),
  "nav.up": async () => void (await viewCall(viewNow(), "view.up")),
  "nav.home": async () => void (await viewCall(viewNow(), "view.home")),
  "filter.focus": () => focusFilter(viewNow()),
  "path.edit": () => void window.dispatchEvent(new Event("sk:edit-path")),
  "view.zoom_in": async () => {
    const st = stateNow();
    if (st?.selected != null) await viewCall(viewNow(), "view.zoom", { key: st.selected });
  },
  "basket.add": async () => {
    const sel = selectedNode();
    if (sel) await basketAdd(sel.scanId, sel.node);
  },
  "item.copy_path": async () => {
    const p = await selectionTarget();
    if (!p) return;
    window.sk.shell.copyText(p.path);
    toast(t("已复制"));
  },
  "item.reveal": async () => {
    const p = await selectionTarget();
    if (!p) return;
    if (!p.onDisk) return toast(notOnDisk());
    window.sk.shell.showItemInFolder(p.path);
    toast(revealLabel());
  },
  "item.terminal": async () => {
    const p = await selectionTarget();
    if (!p) return;
    if (!p.onDisk) return toast(notOnDisk());
    window.sk.shell.openTerminal(p.dir ? p.path : p.path.replace(/[\\/][^\\/]*$/, ""));
  },
  "tab.new": () => newStartTab(),
  "tab.close": () => closeTab(tabNow().id),
  "tab.move_left": () => moveActiveTab(-1),
  "tab.move_right": () => moveActiveTab(1),
  "tag.red": () => tag("red"),
  "tag.yellow": () => tag("yellow"),
  "tag.green": () => tag("green"),
  "tag.blue": () => tag("blue"),
  "tag.clear": () => tag(null),
  "view.split": () => toggleSplit(),
  "scan.pause": async () => {
    const id = tabNow().scanId;
    if (id) await pauseScan(id);
  },
  "scan.resume": async () => {
    const id = tabNow().scanId;
    if (id) await resumeScan(id);
  },
  "scan.stop": async () => {
    const id = tabNow().scanId;
    if (id) await call("scan.cancel", { scan_id: id });
  },
  "scan.continue": async () => {
    const id = tabNow().scanId;
    if (id) await continueScan(id);
  },
  "view.rescan": async () => {
    // the zoom root; 0 is the top of a scan of several roots: all of them are read again
    const tab = tabNow();
    const zoom = stateNow()?.zoom_node;
    if (tab.scanId && zoom != null && zoom >= 0) await rescanFolder(tab.scanId, zoom);
  },
  "scan.new": () => newStartTab(),
} satisfies Record<string, (rc: RunContext) => Promise<void> | void>);

async function tag(color: "red" | "yellow" | "green" | "blue" | null): Promise<void> {
  await viewCall(viewNow(), "view.tag", { color });
}

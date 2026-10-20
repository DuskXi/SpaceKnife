// Runs palette commands by id (W4). The backend decides which commands exist, their names, keys,
// status and availability (palette.query); this file only maps an id to what it does here.

import { focusFilter } from "@/shell/FilterBox";
import { revealLabel } from "@/shell/Inspector";
import { t } from "@/lib/i18n";
import { Kind } from "@/visual/frame";
import { registeredLayouts } from "@/visual/registry";
import type { PaletteContext } from "@/protocol/generated";

import {
  basketAdd,
  call,
  checkUpdates,
  closeTab,
  newStartTab,
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
import { cycleDirection, enterCompare, refreshCompareSources } from "./compare";
import { openHelp } from "./help";
import { type AppState, activeTab, basketCount, focusedViewId, get, set, toast, toggleLog } from "./store";
import { type ColorModeName, nextColor, nextVisual, offered } from "./visuals";

/** The selected node of the focused view (not an aggregate block), or null. */
export function selectedNode(): { scanId: string; viewId: string; node: number } | null {
  const viewId = focusedViewId();
  const view = viewId ? get().views[viewId] : undefined;
  const frame = view?.frame;
  const row = frame?.info.selected_row;
  if (!viewId || !view || !frame || row == null || frame.kind[row]! >= Kind.SMALL || frame.node[row]! <= 0) return null;
  return { scanId: view.state.scan_id, viewId, node: frame.node[row]! };
}

/** V / Shift+V and 「切换可视化」: the view's next (dir 1) or previous visualization among those it
 * can show here. The backend's command view.cycle_visual (palette/commands.py) names V as its key and
 * App.tsx handles V itself: keep the two in step by hand. */
export async function cycleVisual(viewId: string, dir: 1 | -1): Promise<void> {
  const s = get();
  const st = s.views[viewId]?.state;
  if (!st) return;
  const next = nextVisual(s.visuals, registeredLayouts(), st.visual, dir, st.compare);
  if (next) await switchVisual(viewId, next);
}

/** The toolbar's visualization segments and 「可视化：…」: one this view can't show says why instead. */
export async function pickVisual(viewId: string, id: string): Promise<void> {
  const s = get();
  const st = s.views[viewId]?.state;
  if (!st || st.visual === id) return;
  const item = offered(s.visuals, registeredLayouts(), st.compare).find((o) => o.info.id === id);
  if (item?.reason) return toast(item.reason);
  await switchVisual(viewId, id);
}

/** Zoom, selection, filter and history stay (they are the backend view's). The view's context menu
 * closes: it points into the drawing being replaced. */
async function switchVisual(viewId: string, id: string): Promise<void> {
  if (get().menu?.viewId === viewId) set({ menu: null });
  await setViewSettings(viewId, { visual: id });
}

async function selectionPath(): Promise<{ path: string; dir: boolean } | null> {
  const sel = selectedNode();
  if (!sel) return null;
  const r = await call("tree.node", { scan_id: sel.scanId, node: sel.node });
  return { path: r.path, dir: r.node.kind === "dir" };
}

/** Where the user is, as the backend's commands see it (palette.query, palette.check). */
export function paletteContext(s: AppState = get()): PaletteContext {
  const tab = activeTab(s);
  // the help tab is, to the backend's commands, a tab without a scan like the settings tab
  return { tab: tab.kind === "help" ? "settings" : tab.kind, scan_id: tab.scanId ?? null, view_id: focusedViewId(s), basket: basketCount(s) };
}

/** The command a key runs through the palette's rules (runShortcut), or null: F5 / Shift+F5 / Ctrl+S,
 * the keys palette/commands.py names for view.rescan, scan.rescan_all and snapshot.save. */
export function shortcutCommand(e: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "shiftKey" | "altKey">): string | null {
  const ctrl = e.ctrlKey || e.metaKey;
  if (e.key === "F5" && !ctrl && !e.altKey) return e.shiftKey ? "scan.rescan_all" : "view.rescan";
  if (ctrl && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "s") return "snapshot.save";
  return null;
}

/** A command's keyboard shortcut (App.tsx): it runs where the palette would offer the command, and
 * elsewhere says why not, as the palette's row does. Which commands are available is the backend's
 * call (palette.check), so the key and the palette never disagree. */
export async function runShortcut(id: string): Promise<void> {
  const c = await call("palette.check", { id, context: paletteContext() });
  if (!c.enabled) return toast(c.status ? t("{command}：{reason}", { command: c.name, reason: c.status }) : c.name);
  await runCommand(id);
}

export async function runCommand(id: string): Promise<void> {
  const s = get();
  const tab = activeTab(s);
  const viewId = focusedViewId(s);
  const st = viewId ? s.views[viewId]?.state : undefined;
  const scan = tab.scanId ? s.scans[tab.scanId] : undefined;
  switch (id) {
    case "scan.rescan_all":
      if (scan) await startScan({ roots: scan.roots.map((r) => r.path) }, tab.id);
      return;
    case "view.cycle_color":
      if (st?.compare && viewId) return cycleDirection(viewId);
      if (st) await setViewSettings(viewId, { color_mode: nextColor(st.color_mode) });
      return;
    case "view.color.class":
    case "view.color.flat":
    case "view.color.time":
    case "view.color.branch":
      await setViewSettings(viewId, { color_mode: id.slice(11) as ColorModeName });
      return;
    case "view.cycle_visual":
      if (viewId) await cycleVisual(viewId, 1);
      return;
    case "view.visual.treemap":
    case "view.visual.flame":
    case "view.visual.flame_polar":
      if (viewId) await pickVisual(viewId, id.slice(12));
      return;
    case "view.toggle_free":
      if (st) await setViewSettings(viewId, { show_free: !st.show_free });
      return;
    case "view.toggle_unknown":
      if (st) await setViewSettings(viewId, { show_unknown: !st.show_unknown });
      return;
    case "view.detail_up":
      if (st) await setViewSettings(viewId, { detail: Math.min(15, st.detail + 1) });
      return;
    case "view.detail_down":
      if (st) await setViewSettings(viewId, { detail: Math.max(1, st.detail - 1) });
      return;
    case "snapshot.save":
      if (tab.scanId) {
        try {
          const info = await call("snapshot.save", { scan_id: tab.scanId });
          if (tab.compare) await refreshCompareSources(tab.id);
          toast(t("快照已保存：{name}", { name: info.path.split(/[\\/]/).pop() ?? info.path }));
        } catch (err) {
          toast(t("保存快照失败：{error}", { error: String(err) }));
        }
      }
      return;
    case "snapshot.compare":
      return enterCompare();
    case "report.export":
      set({ exportOpen: true });
      return;
    case "basket.open":
      set({ basketOpen: !s.basketOpen });
      return;
    case "snapshot.open": {
      const path = await window.sk.dialog.openSnapshot();
      if (path) await openSnapshot(path);
      return;
    }
    case "app.settings":
      return openSettingsTab();
    case "app.help":
      return openHelp();
    case "app.log":
      toggleLog();
      return;
    case "app.check_updates":
      return checkUpdates();
    case "app.show_update":
      return showUpdate();
    case "nav.back":
      await viewCall(viewId, "view.back");
      return;
    case "nav.forward":
      await viewCall(viewId, "view.forward");
      return;
    case "nav.up":
      await viewCall(viewId, "view.up");
      return;
    case "nav.home":
      await viewCall(viewId, "view.home");
      return;
    case "filter.focus":
      focusFilter(viewId);
      return;
    case "path.edit":
      window.dispatchEvent(new Event("sk:edit-path"));
      return;
    case "view.zoom_in":
      if (st?.selected != null) await viewCall(viewId, "view.zoom", { key: st.selected });
      return;
    case "basket.add": {
      const sel = selectedNode();
      if (sel) basketAdd(sel.scanId, sel.node);
      return;
    }
    case "item.copy_path": {
      const p = await selectionPath();
      if (p) {
        window.sk.shell.copyText(p.path);
        toast(t("已复制"));
      }
      return;
    }
    case "item.reveal": {
      const p = await selectionPath();
      if (p) {
        window.sk.shell.showItemInFolder(p.path);
        toast(revealLabel());
      }
      return;
    }
    case "item.terminal": {
      const p = await selectionPath();
      if (p) window.sk.shell.openTerminal(p.dir ? p.path : p.path.replace(/[\\/][^\\/]*$/, ""));
      return;
    }
    case "tab.new":
      return newStartTab();
    case "tab.close":
      await closeTab(tab.id);
      return;
    case "tag.red":
    case "tag.yellow":
    case "tag.green":
    case "tag.blue":
      await viewCall(viewId, "view.tag", { color: id.slice(4) as "red" | "yellow" | "green" | "blue" });
      return;
    case "tag.clear":
      await viewCall(viewId, "view.tag", { color: null });
      return;
    case "view.split":
      await toggleSplit();
      return;
    case "scan.pause":
      if (tab.scanId) await pauseScan(tab.scanId);
      return;
    case "scan.resume":
      if (tab.scanId) await resumeScan(tab.scanId);
      return;
    case "scan.stop":
      if (tab.scanId) await call("scan.cancel", { scan_id: tab.scanId });
      return;
    case "scan.continue":
      if (tab.scanId) await continueScan(tab.scanId);
      return;
    case "view.rescan": {
      // the zoom root; 0 is the top of a scan of several roots: all of them are read again
      const zoom = viewId ? get().views[viewId]?.state.zoom_node : undefined;
      if (tab.scanId && zoom != null && zoom >= 0) await rescanFolder(tab.scanId, zoom);
      return;
    }
    case "scan.new":
      return newStartTab();
    default:
      toast(t("这个命令还没有接入"));
  }
}

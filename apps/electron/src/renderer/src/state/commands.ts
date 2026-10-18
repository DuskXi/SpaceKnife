// Runs palette commands by id (W4). The backend decides which commands exist, their names, keys,
// status and availability (palette.query); this file only maps an id to what it does here.

import { focusFilter } from "@/shell/FilterBox";
import { revealLabel } from "@/shell/Inspector";
import { t } from "@/lib/i18n";
import { Kind } from "@/treemap/frame";

import {
  basketAdd,
  call,
  closeTab,
  newStartTab,
  openSettingsTab,
  openSnapshot,
  continueScan,
  pauseScan,
  rescanFolder,
  resumeScan,
  setViewSettings,
  startScan,
  toggleSplit,
  viewCall,
} from "./actions";
import { cycleDirection, enterCompare, refreshCompareSources } from "./compare";
import { activeTab, focusedViewId, get, set, toast, toggleLog } from "./store";

const COLORS = ["class", "flat", "time"] as const;

/** The selected node of the focused view (not an aggregate block), or null. */
export function selectedNode(): { scanId: string; viewId: string; node: number } | null {
  const viewId = focusedViewId();
  const view = viewId ? get().views[viewId] : undefined;
  const frame = view?.frame;
  const row = frame?.info.selected_row;
  if (!viewId || !view || !frame || row == null || frame.kind[row]! >= Kind.SMALL || frame.node[row]! <= 0) return null;
  return { scanId: view.state.scan_id, viewId, node: frame.node[row]! };
}

async function selectionPath(): Promise<{ path: string; dir: boolean } | null> {
  const sel = selectedNode();
  if (!sel) return null;
  const r = await call("tree.node", { scan_id: sel.scanId, node: sel.node });
  return { path: r.path, dir: r.node.kind === "dir" };
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
      if (st) await setViewSettings(viewId, { color_mode: COLORS[(COLORS.indexOf(st.color_mode) + 1) % 3] });
      return;
    case "view.color.class":
    case "view.color.flat":
    case "view.color.time":
      await setViewSettings(viewId, { color_mode: id.slice(11) as (typeof COLORS)[number] });
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
    case "app.log":
      toggleLog();
      return;
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
      const zoom = viewId ? get().views[viewId]?.state.zoom_node : undefined;
      if (tab.scanId && zoom != null && zoom > 0) await rescanFolder(tab.scanId, zoom);
      return;
    }
    case "scan.new":
      return newStartTab();
    default:
      toast(t("这个命令还没有接入"));
  }
}

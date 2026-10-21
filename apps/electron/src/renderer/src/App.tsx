import { useEffect } from "react";

import { basketAdd, boot, call, closeTab, newStartTab, openSettingsTab, setViewSettings, settingsHistory, viewCall } from "@/state/actions";
import { BrowserLayer } from "@/shell/BrowserLayer";
import { ContextMenu } from "@/shell/ContextMenu";
import { ExportDialog } from "@/shell/ExportDialog";
import { FullDiskAccessDialog } from "@/shell/FullDiskAccessDialog";
import { OpToast } from "@/shell/OpToast";
import { activeTab, focusedViewId, get, set, setting, toast, toggleLog, useApp } from "@/state/store";
import { notOnDisk, revealLabel } from "@/shell/Inspector";
import { focusFilter } from "@/shell/FilterBox";
import { inBrowser } from "@/lib/host";
import { t } from "@/lib/i18n";
import { LogPanel } from "@/shell/LogPanel";
import { Palette } from "@/shell/Palette";
import { Kind } from "@/visual/frame";
import { hosts } from "@/visual/hosts";
import { HIT_ROOT, type VisualFrame } from "@/visual/types";
import { StatusBar } from "@/shell/StatusBar";
import { TitleBar } from "@/shell/TitleBar";
import { Toolbar } from "@/shell/Toolbar";
import { UpdatePrompt } from "@/shell/UpdateCard";
import { HelpPage } from "@/pages/HelpPage";
import { MachinePage } from "@/pages/MachinePage";
import { ScanPage } from "@/pages/ScanPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { StartPage } from "@/pages/StartPage";
import { cycleDirection, exitCompare } from "@/state/compare";
import { cycleVisual, runShortcut, shortcutCommand } from "@/state/commands";
import { helpHistory, openHelp, openHelpTab } from "@/state/help";
import { visibleSurface } from "@/state/updates";
import { nextColor } from "@/state/visuals";

export function App() {
  const tab = useApp((s) => activeTab(s));
  const conn = useApp((s) => s.conn);
  const message = useApp((s) => s.connMessage);
  const toastText = useApp((s) => s.toast);
  const paletteOpen = useApp((s) => s.paletteOpen);
  const updateDialog = useApp((s) => visibleSurface(s.update, s.updateOpen) === "dialog");
  useApp((s) => s.lang); // 「界面语言」: a change re-renders the whole tree, every t() follows (M7.5)

  useEffect(() => {
    void boot();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <>
      <div className="app" inert={paletteOpen || updateDialog}>
        <TitleBar />
        <Toolbar />
        {tab.kind === "scan" ? (
          <ScanPage tab={tab} />
        ) : tab.kind === "settings" ? (
          <SettingsPage />
        ) : tab.kind === "help" ? (
          <HelpPage />
        ) : tab.kind === "machine" ? (
          <MachinePage />
        ) : (
          <StartPage tab={tab} />
        )}
        <LogPanel />
        <StatusBar />
        {conn === "failed" && (
          <div className="fatal">
            <h2>{inBrowser() ? t("连不上 SpaceKnife") : t("后端没有启动")}</h2>
            <p className="mono">{message}</p>
          </div>
        )}
        {toastText && (
          <div className="toast" role="status">
            {toastText}
          </div>
        )}
      </div>
      <Palette />
      <ContextMenu />
      <OpToast />
      <ExportDialog />
      <FullDiskAccessDialog />
      <UpdatePrompt />
      <BrowserLayer />
    </>
  );
}

/** Global shortcuts (UI-MAIN-58). Single-key shortcuts never fire inside text fields. */
function onKey(e: KeyboardEvent): unknown {
  const target = e.target as HTMLElement | null;
  const typing = !!target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
  const ctrl = e.ctrlKey || e.metaKey;
  const key = e.key;
  const s = get();
  const tab = activeTab(s);
  const viewId = focusedViewId(s);
  const view = viewId ? s.views[viewId] : undefined;
  const st = view?.state;
  const done = () => e.preventDefault();

  if (visibleSurface(s.update, s.updateOpen) === "dialog") return; // a major release's dialog is modal: no shortcuts behind it
  if (ctrl && key.toLowerCase() === "k") return (done(), set({ paletteOpen: !s.paletteOpen }));
  // F1: the help, on the page for where the user is (the backend's app.help names the key too)
  const help = key === "F1" && !ctrl && !e.altKey && !e.shiftKey;
  if (s.paletteOpen) {
    // the palette is modal: it handles its own keys (UI-W4-03); F1 leaves it for its help page
    if (help) return (done(), set({ paletteOpen: false }), openHelpTab("command-palette"));
    return;
  }
  if (help) return (done(), openHelp());
  // F5 / Shift+F5 / Ctrl+S: they run where the palette offers their command, anywhere else a toast
  // says why not; held down, they run once
  const shortcut = shortcutCommand(e);
  if (shortcut) {
    done();
    if (!e.repeat) void runShortcut(shortcut);
    return;
  }
  if (ctrl && key.toLowerCase() === "j") return (done(), toggleLog()); // W8 待定 2
  if (ctrl && key.toLowerCase() === "t") return (done(), newStartTab());
  if (ctrl && key.toLowerCase() === "w") return (done(), void closeTab(tab.id));
  if (ctrl && key === ",") return (done(), openSettingsTab());
  if (ctrl && (key === "Tab" || key === "PageDown" || key === "PageUp")) {
    const idx = s.tabs.findIndex((x) => x.id === tab.id);
    const step = (e.shiftKey && key === "Tab") || key === "PageUp" ? -1 : 1;
    return (done(), set({ active: s.tabs[(idx + step + s.tabs.length) % s.tabs.length]!.id }));
  }
  if (tab.kind === "help") {
    // the help tab has its own search and page history, as the settings tab has
    if (typing) return;
    if ((key === "/" && !ctrl) || (ctrl && key.toLowerCase() === "f")) return (done(), document.getElementById("sk-help-search")?.focus());
    if ((key === "Backspace" && !ctrl) || (e.altKey && key === "ArrowLeft")) return (done(), helpHistory(-1));
    if (e.altKey && key === "ArrowRight") return (done(), helpHistory(1));
    return;
  }
  if (tab.kind === "settings" && !typing) {
    // the settings tab has its own search and page history (UI-W7-03, UI-W7-05)
    if ((key === "/" && !ctrl) || (ctrl && key.toLowerCase() === "f")) return (done(), document.getElementById("sk-settings-search")?.focus());
    if (key === "Backspace" || (e.altKey && key === "ArrowLeft")) return (done(), settingsHistory(-1));
    if (e.altKey && key === "ArrowRight") return (done(), settingsHistory(1));
    return;
  }
  if (typing || !viewId || !st) return;
  const bareTags = setting(s, "tags.omit_ctrl", true); // 「树图有焦点时可省略 Ctrl」 (UI-W7-34)
  // a focused button, radio or menu item keeps Enter / Space / Delete / Escape for itself (UI-W3-38)
  const onControl = !!target?.closest("button, a, select, [role=button], [role=radio], [role=menuitem], [role=menuitemradio], [role=option], [role=switch], [role=tab]");
  // a focused drop-down picks its options by letter, so C and V don't cycle while it has the focus
  const typeAhead = target?.tagName === "SELECT";

  if ((key === "/" && !ctrl) || (ctrl && key.toLowerCase() === "f")) return (done(), focusFilter(viewId));
  if (ctrl && key.toLowerCase() === "l") return (done(), window.dispatchEvent(new Event("sk:edit-path")));
  if (key === "Backspace") return (done(), void viewCall(viewId, e.shiftKey ? "view.forward" : "view.back"));
  if (e.altKey && key === "ArrowRight") return (done(), void viewCall(viewId, "view.forward"));
  if (e.altKey && key === "ArrowLeft") return (done(), void viewCall(viewId, "view.back"));
  if (e.altKey && key === "ArrowUp") return (done(), void viewCall(viewId, "view.up"));
  if (ctrl && key === "Home") return (done(), void viewCall(viewId, "view.home"));
  if (ctrl && (key === "=" || key === "+")) return (done(), void setViewSettings(viewId, { detail: Math.min(15, st.detail + 1) }));
  if (ctrl && key === "-") return (done(), void setViewSettings(viewId, { detail: Math.max(1, st.detail - 1) }));
  // Shift+F10 / the menu key: the context menu of the selected block, beside it (UI-W5-02)
  if ((key === "ContextMenu" || (e.shiftKey && key === "F10")) && !onControl) {
    done();
    if (view?.frame) keyboardMenu(viewId, st.scan_id, view.frame);
    return;
  }
  if (key === "Enter" && !onControl && st.selected != null) return (done(), void viewCall(viewId, "view.zoom", { key: st.selected }));
  if (key === "Escape" && !onControl) {
    // compare mode: Esc clears the selection, then leaves the comparison (W6 待定 16)
    if (tab.compare && st.selected == null) return (done(), void exitCompare(tab.id));
    return (done(), void viewCall(viewId, "view.select", { key: null }));
  }
  if (key === "Delete" && !ctrl && !onControl) {
    const frame = view?.frame;
    const row = frame?.info.selected_row;
    if (row != null && frame && frame.kind[row]! < Kind.SMALL && frame.node[row]! > 0) return (done(), basketAdd(st.scan_id, frame.node[row]!));
  }
  if (ctrl && "01234".includes(key) && key.length === 1) {
    return (
      done(),
      void viewCall(viewId, "view.tag", {
        color: ([null, "red", "yellow", "green", "blue"] as const)[Number(key)] ?? null,
      })
    );
  }
  if (bareTags && !ctrl && !e.altKey && "1234".includes(key) && key.length === 1) {
    return (
      done(),
      void viewCall(viewId, "view.tag", {
        color: (["red", "yellow", "green", "blue"] as const)[Number(key) - 1] ?? null,
      })
    );
  }
  if (bareTags && !ctrl && !e.altKey && key === "0") return (done(), void viewCall(viewId, "view.tag", { color: null }));
  const lower = key.toLowerCase();
  if (st.compare && !ctrl && !e.altKey && (lower === "f" || lower === "u")) return; // no free / unknown blocks
  if (st.compare && !ctrl && !e.altKey && !typeAhead && lower === "c") return (done(), cycleDirection(viewId));
  if (!ctrl && !e.altKey && lower === "f") return (done(), void setViewSettings(viewId, { show_free: !st.show_free }));
  if (!ctrl && !e.altKey && lower === "u") return (done(), void setViewSettings(viewId, { show_unknown: !st.show_unknown }));
  if (!ctrl && !e.altKey && lower === "r" && st.has_trash) return (done(), void setViewSettings(viewId, { show_trash: !st.show_trash }));
  if (!ctrl && !e.altKey && !typeAhead && lower === "c") return (done(), void setViewSettings(viewId, { color_mode: nextColor(st.color_mode) }));
  // V / Shift+V: the next / previous visualization, comparisons included. The key is also listed by
  // the backend's view.cycle_visual (palette/commands.py): change both together.
  if (!ctrl && !e.altKey && !typeAhead && lower === "v") return (done(), void cycleVisual(viewId, e.shiftKey ? -1 : 1));
  if (ctrl && (lower === "c" || lower === "e" || key === "`")) {
    const frame = view?.frame;
    const row = frame?.info.selected_row;
    if (row == null || !frame || frame.node[row]! <= 0) return;
    done();
    void (async () => {
      const r = await call("tree.node", {
        scan_id: st.scan_id,
        node: frame.node[row]!,
      });
      if (lower === "c") {
        window.sk.shell.copyText(r.path);
        toast(t("已复制"));
      } else if (!r.on_disk) {
        toast(notOnDisk());
      } else if (lower === "e") {
        window.sk.shell.showItemInFolder(r.path);
        toast(revealLabel());
      } else window.sk.shell.openTerminal(r.node.kind === "dir" ? r.path : r.path.replace(/[\\/][^\\/]*$/, ""));
    })();
  }
}

/** The keyboard's context menu: for the selected row, or for the zoom root when nothing is selected
 * (the flame graph's root bar, the polar flame graph's centre disc). The view's host says where the
 * row is drawn; nothing opens for a row it doesn't draw. */
function keyboardMenu(viewId: string, scanId: string, frame: Pick<VisualFrame, "info" | "key" | "node">): void {
  const sel = frame.info.selected_row;
  const target = sel != null ? { key: frame.key[sel]!, node: frame.node[sel]! } : frame.info.root;
  const at = target && hosts.get(viewId)?.menuAnchor(sel ?? HIT_ROOT);
  if (!target || !at) return;
  set({ menu: { viewId, scanId, key: target.key, node: target.node > 0 ? target.node : null, ...at } });
}

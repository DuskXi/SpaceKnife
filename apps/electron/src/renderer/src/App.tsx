import { useEffect } from "react";

import { basketAdd, boot, call, closeTab, newStartTab, openSettingsTab, setViewSettings, settingsHistory, viewCall } from "@/state/actions";
import { activeTab, focusedViewId, get, set, setting, toast, toggleLog, useApp } from "@/state/store";
import { NOT_ON_DISK, revealLabel } from "@/shell/Inspector";
import { focusFilter } from "@/shell/FilterBox";
import { LogPanel } from "@/shell/LogPanel";
import { Palette } from "@/shell/Palette";
import { Kind } from "@/treemap/frame";
import { StatusBar } from "@/shell/StatusBar";
import { TitleBar } from "@/shell/TitleBar";
import { Toolbar } from "@/shell/Toolbar";
import { ScanPage } from "@/pages/ScanPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { StartPage } from "@/pages/StartPage";

export function App() {
  const tab = useApp((s) => activeTab(s));
  const conn = useApp((s) => s.conn);
  const message = useApp((s) => s.connMessage);
  const toastText = useApp((s) => s.toast);
  const paletteOpen = useApp((s) => s.paletteOpen);

  useEffect(() => {
    void boot();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <>
      <div className="app" inert={paletteOpen}>
        <TitleBar />
        <Toolbar />
        {tab.kind === "scan" ? <ScanPage tab={tab} /> : tab.kind === "settings" ? <SettingsPage /> : <StartPage tab={tab} />}
        <LogPanel />
        <StatusBar />
        {conn === "failed" && (
          <div className="fatal">
            <h2>后端没有启动</h2>
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

  if (ctrl && key.toLowerCase() === "k") return (done(), set({ paletteOpen: !s.paletteOpen }));
  if (s.paletteOpen) return; // the palette is modal: it handles its own keys (UI-W4-03)
  if (ctrl && key.toLowerCase() === "j") return (done(), toggleLog()); // W8 待定 2
  if (ctrl && key.toLowerCase() === "t") return (done(), newStartTab());
  if (ctrl && key.toLowerCase() === "w") return (done(), void closeTab(tab.id));
  if (ctrl && key === ",") return (done(), openSettingsTab());
  if (ctrl && (key === "Tab" || key === "PageDown" || key === "PageUp")) {
    const idx = s.tabs.findIndex((t) => t.id === tab.id);
    const step = (e.shiftKey && key === "Tab") || key === "PageUp" ? -1 : 1;
    return (done(), set({ active: s.tabs[(idx + step + s.tabs.length) % s.tabs.length]!.id }));
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

  if ((key === "/" && !ctrl) || (ctrl && key.toLowerCase() === "f")) return (done(), focusFilter(viewId));
  if (ctrl && key.toLowerCase() === "l") return (done(), window.dispatchEvent(new Event("sk:edit-path")));
  if (key === "Backspace") return (done(), void viewCall(viewId, e.shiftKey ? "view.forward" : "view.back"));
  if (e.altKey && key === "ArrowRight") return (done(), void viewCall(viewId, "view.forward"));
  if (e.altKey && key === "ArrowLeft") return (done(), void viewCall(viewId, "view.back"));
  if (e.altKey && key === "ArrowUp") return (done(), void viewCall(viewId, "view.up"));
  if (ctrl && key === "Home") return (done(), void viewCall(viewId, "view.home"));
  if (ctrl && (key === "=" || key === "+")) return (done(), void setViewSettings(viewId, { detail: Math.min(15, st.detail + 1) }));
  if (ctrl && key === "-") return (done(), void setViewSettings(viewId, { detail: Math.max(1, st.detail - 1) }));
  if (key === "Enter" && !onControl && st.selected != null) return (done(), void viewCall(viewId, "view.zoom", { key: st.selected }));
  if (key === "Escape" && !onControl) return (done(), void viewCall(viewId, "view.select", { key: null }));
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
  if (!ctrl && !e.altKey && lower === "f") return (done(), void setViewSettings(viewId, { show_free: !st.show_free }));
  if (!ctrl && !e.altKey && lower === "u") return (done(), void setViewSettings(viewId, { show_unknown: !st.show_unknown }));
  if (!ctrl && !e.altKey && lower === "c") {
    const order = ["class", "flat", "time"] as const;
    return (
      done(),
      void setViewSettings(viewId, {
        color_mode: order[(order.indexOf(st.color_mode) + 1) % 3],
      })
    );
  }
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
        toast("已复制");
      } else if (!r.on_disk) {
        toast(NOT_ON_DISK);
      } else if (lower === "e") {
        window.sk.shell.showItemInFolder(r.path);
        toast(revealLabel());
      } else window.sk.shell.openTerminal(r.node.kind === "dir" ? r.path : r.path.replace(/[\\/][^\\/]*$/, ""));
    })();
  }
}

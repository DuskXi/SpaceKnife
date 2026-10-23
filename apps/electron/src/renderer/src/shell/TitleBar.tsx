// Title bar (UI-MAIN-02…09): brand, tabs, new tab, update badge, settings, window buttons; the empty
// space drags. In a browser tab (headless mode) no window buttons: the computer's chip instead.

import { useEffect, useRef, useState } from "react";

import { activate, closeTab, moveTab, newStartTab, openMachineTab, openSettingsTab, restoreTabOrder } from "@/state/actions";
import { dropIndex } from "@/state/tabOrder";
import { openHelp } from "@/state/help";
import { useApp } from "@/state/store";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { RemoteChip } from "./BrowserLayer";
import { UpdateBadge } from "./UpdateBadge";
import { pluginTabTitle } from "@/plugins/slots";

export function TitleBar() {
  const tabs = useApp((s) => s.tabs);
  const extensions = useApp((s) => s.extensions); // plugin tabs' titles (they follow the language)
  const active = useApp((s) => s.active);
  const scans = useApp((s) => s.scans);
  const native = window.sk.host === "app";
  const mac = native && window.sk.platform === "darwin"; // the traffic lights' room
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    void window.sk.window.isMaximized().then(setMaximized);
    return window.sk.window.onMaximized(setMaximized);
  }, []);

  // UI-MAIN-04: drag a tab sideways to move it; it takes its neighbour's slot once the pointer
  // crosses that tab's middle. Esc puts every tab back where the drag started.
  const row = useRef<HTMLDivElement>(null);
  const drag = useRef<{ id: string; x: number; order: string[]; moved: boolean } | null>(null);
  const [dragging, setDragging] = useState<string | null>(null);
  useEffect(() => {
    if (!dragging) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !drag.current) return;
      e.preventDefault();
      e.stopPropagation();
      restoreTabOrder(drag.current.order);
      drag.current = null;
      setDragging(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [dragging]);
  const boxes = () =>
    [...(row.current?.querySelectorAll<HTMLElement>("[data-tab-id]") ?? [])].map((el) => {
      const r = el.getBoundingClientRect();
      return { id: el.dataset.tabId!, left: r.left, width: r.width };
    });
  const endDrag = () => {
    drag.current = null;
    setDragging(null);
  };

  return (
    <header className={`titlebar${mac ? " mac" : ""}`} onDoubleClick={(e) => e.target === e.currentTarget && window.sk.window.toggleMaximize()}>
      <div className="brand">
        <span className="logo">
          <Icon name="logo" />
        </span>
        <span>SpaceKnife</span>
      </div>
      <div ref={row} className={`tabs${dragging ? " reordering" : ""}`} role="tablist" aria-label={t("扫描视图")}>
        {tabs.map((tab0) => {
          // a plugin tab is named as its report or panel is now (a language switch renames it)
          const tab = tab0.kind === "plugin" && extensions ? { ...tab0, title: pluginTabTitle(tab0, { extensions }) } : tab0;
          const status = tab.scanId ? scans[tab.scanId] : undefined;
          const scanning = status?.state === "scanning" || status?.state === "queued";
          // a stopped or failed scan keeps its partial results (W2 待定 16)
          const state = scanning ? (status?.paused ? t("已暂停") : t("扫描中")) : status?.state === "cancelled" ? t("未完成") : status?.state === "failed" ? t("扫描失败") : "";
          const suffix = state ? ` · ${state}` : "";
          const on = tab.id === active;
          const vol = status?.roots[0]?.volume;
          const hint = vol ? `${vol.fs_type} · ${fmtSize(vol.total, true)}` : tab.title;
          return (
            <div
              key={tab.id}
              role="tab"
              aria-selected={on}
              tabIndex={on ? 0 : -1}
              data-tab-id={tab.id}
              className={`tab${on ? " on" : ""}${dragging === tab.id ? " dragging" : ""}`}
              title={dragging ? undefined : `${tab.title}${suffix}\n${hint}\n${t("拖动可调整顺序")}`}
              onPointerDown={(e) => {
                if (e.button === 1) return void closeTab(tab.id);
                if (e.button !== 0) return;
                activate(tab.id);
                drag.current = { id: tab.id, x: e.clientX, order: tabs.map((x) => x.id), moved: false };
                e.currentTarget.setPointerCapture(e.pointerId);
              }}
              onPointerMove={(e) => {
                const d = drag.current;
                if (!d || d.id !== tab.id) return;
                if (!d.moved) {
                  if (Math.abs(e.clientX - d.x) < 5) return; // a click, not a drag
                  d.moved = true;
                  setDragging(tab.id);
                }
                const list = boxes();
                const to = dropIndex(list, tab.id, e.clientX);
                if (list.findIndex((b) => b.id === tab.id) !== to) moveTab(tab.id, to);
              }}
              onPointerUp={endDrag}
              onPointerCancel={endDrag}
              onLostPointerCapture={endDrag}
            >
              <Icon name={tab.kind === "settings" ? "settings" : tab.kind === "help" ? "info" : tab.kind === "machine" ? "diskGauge" : "drive"} size={14} />
              <span className="tab-title">
                {tab.title}
                {suffix && <span className="tab-suffix">{suffix}</span>}
              </span>
              <button
                className="tab-close"
                aria-label={t("关闭标签 {title}（Ctrl+W）", { title: tab.title })}
                title={t("关闭标签（Ctrl+W）")}
                onPointerDown={(e) => e.stopPropagation()}
                onMouseDown={(e) => e.stopPropagation()}
                onClick={() => void closeTab(tab.id)}
              >
                <Icon name="close" size={12} />
              </button>
            </div>
          );
        })}
        <button className="tab-new" aria-label={t("新建标签（Ctrl+T）")} title={t("新建标签（Ctrl+T）")} onClick={newStartTab}>
          <Icon name="plus" />
        </button>
      </div>
      <div className="drag" />
      {!native && <RemoteChip />}
      <UpdateBadge />
      <button className="title-btn" aria-label={t("本机磁盘")} title={t("本机磁盘：每块盘的容量、链路、读写量与健康")} onClick={openMachineTab}>
        <Icon name="diskGauge" />
      </button>
      <button className="title-btn" aria-label={t("帮助（F1）")} title={t("帮助（F1）")} onClick={() => openHelp()}>
        <Icon name="info" />
      </button>
      <button className="title-btn" aria-label={t("设置（Ctrl+,）")} title={t("设置（Ctrl+,）")} onClick={() => openSettingsTab()}>
        <Icon name="settings" />
      </button>
      {native && !mac && (
        <div className="window-btns">
          <button aria-label={t("最小化")} title={t("最小化")} onClick={() => window.sk.window.minimize()}>
            <Icon name="windowMinimize" strokeWidth={1.25} />
          </button>
          <button aria-label={maximized ? t("还原") : t("最大化")} title={maximized ? t("还原") : t("最大化")} onClick={() => window.sk.window.toggleMaximize()}>
            <Icon name="windowMaximize" strokeWidth={1.25} />
          </button>
          <button className="close" aria-label={t("关闭")} title={t("关闭")} onClick={() => window.sk.window.close()}>
            <Icon name="windowClose" strokeWidth={1.25} />
          </button>
        </div>
      )}
    </header>
  );
}

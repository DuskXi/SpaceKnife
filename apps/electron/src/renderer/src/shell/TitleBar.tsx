// Title bar (UI-MAIN-02…09): brand, tabs, new tab, update badge, settings, window buttons; the empty
// space drags. In a browser tab (headless mode) no window buttons: the computer's chip instead.

import { useEffect, useState } from "react";

import { activate, closeTab, newStartTab, openMachineTab, openSettingsTab } from "@/state/actions";
import { openHelp } from "@/state/help";
import { useApp } from "@/state/store";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { RemoteChip } from "./BrowserLayer";
import { UpdateBadge } from "./UpdateBadge";

export function TitleBar() {
  const tabs = useApp((s) => s.tabs);
  const active = useApp((s) => s.active);
  const scans = useApp((s) => s.scans);
  const native = window.sk.host === "app";
  const mac = native && window.sk.platform === "darwin"; // the traffic lights' room
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    void window.sk.window.isMaximized().then(setMaximized);
    return window.sk.window.onMaximized(setMaximized);
  }, []);

  return (
    <header className={`titlebar${mac ? " mac" : ""}`} onDoubleClick={(e) => e.target === e.currentTarget && window.sk.window.toggleMaximize()}>
      <div className="brand">
        <span className="logo">
          <Icon name="logo" />
        </span>
        <span>SpaceKnife</span>
      </div>
      <div className="tabs" role="tablist" aria-label={t("扫描视图")}>
        {tabs.map((tab) => {
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
              className={`tab${on ? " on" : ""}`}
              title={`${tab.title}${suffix}\n${hint}`}
              onMouseDown={(e) => {
                if (e.button === 1) void closeTab(tab.id);
                else activate(tab.id);
              }}
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

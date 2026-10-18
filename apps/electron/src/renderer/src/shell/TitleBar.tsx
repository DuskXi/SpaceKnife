// Title bar (UI-MAIN-02…09): brand, tabs, new tab, settings, window buttons; the empty space drags.

import { useEffect, useState } from "react";

import { activate, closeTab, newStartTab, openSettingsTab } from "@/state/actions";
import { useApp } from "@/state/store";
import { fmtSize } from "@/lib/format";
import { Icon } from "@/ui/Icon";

export function TitleBar() {
  const tabs = useApp((s) => s.tabs);
  const active = useApp((s) => s.active);
  const scans = useApp((s) => s.scans);
  const mac = window.sk.platform === "darwin";
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
      <div className="tabs" role="tablist" aria-label="扫描视图">
        {tabs.map((t) => {
          const status = t.scanId ? scans[t.scanId] : undefined;
          const scanning = status?.state === "scanning" || status?.state === "queued";
          // a stopped or failed scan keeps its partial results (W2 待定 16)
          const suffix = scanning ? (status?.paused ? " · 已暂停" : " · 扫描中") : status?.state === "cancelled" ? " · 未完成" : status?.state === "failed" ? " · 扫描失败" : "";
          const on = t.id === active;
          const vol = status?.roots[0]?.volume;
          const hint = vol ? `${vol.fs_type} · ${fmtSize(vol.total, true)}` : t.title;
          return (
            <div
              key={t.id}
              role="tab"
              aria-selected={on}
              tabIndex={on ? 0 : -1}
              className={`tab${on ? " on" : ""}`}
              title={`${t.title}${suffix}\n${hint}`}
              onMouseDown={(e) => {
                if (e.button === 1) void closeTab(t.id);
                else activate(t.id);
              }}
            >
              <Icon name={t.kind === "settings" ? "settings" : "drive"} size={14} />
              <span className="tab-title">
                {t.title}
                {suffix && <span className="tab-suffix">{suffix}</span>}
              </span>
              <button
                className="tab-close"
                aria-label={`关闭标签 ${t.title}（Ctrl+W）`}
                title="关闭标签（Ctrl+W）"
                onMouseDown={(e) => e.stopPropagation()}
                onClick={() => void closeTab(t.id)}
              >
                <Icon name="close" size={12} />
              </button>
            </div>
          );
        })}
        <button className="tab-new" aria-label="新建标签（Ctrl+T）" title="新建标签（Ctrl+T）" onClick={newStartTab}>
          <Icon name="plus" />
        </button>
      </div>
      <div className="drag" />
      <button className="title-btn" aria-label="设置（Ctrl+,）" title="设置（Ctrl+,）" onClick={() => openSettingsTab()}>
        <Icon name="settings" />
      </button>
      {!mac && (
        <div className="window-btns">
          <button aria-label="最小化" title="最小化" onClick={() => window.sk.window.minimize()}>
            <Icon name="windowMinimize" strokeWidth={1.25} />
          </button>
          <button aria-label={maximized ? "还原" : "最大化"} title={maximized ? "还原" : "最大化"} onClick={() => window.sk.window.toggleMaximize()}>
            <Icon name="windowMaximize" strokeWidth={1.25} />
          </button>
          <button className="close" aria-label="关闭" title="关闭" onClick={() => window.sk.window.close()}>
            <Icon name="windowClose" strokeWidth={1.25} />
          </button>
        </div>
      )}
    </header>
  );
}

// The stack of plugins' notices (ctx.notify) at the bottom right: title, text, the plugin's name, a
// button running one of its commands, a close button. A notice leaves after NOTICE_MS of the window
// being looked at (its time stops while the window is in the background or the pointer is on it).

import { useEffect, useRef } from "react";

import type { PluginNoticeEvent } from "@/protocol/generated";
import { useApp } from "@/state/store";
import { t } from "@/lib/i18n";
import { Icon, type IconName } from "@/ui/Icon";

import { dismissNotice, noticeButton, runNotice, tickNotices } from "./notices";
import { pluginName } from "./store";

const TICK_MS = 250;

const ICON: Record<PluginNoticeEvent["tone"], IconName> = { info: "info", ok: "check", warn: "warning", danger: "warning" };

export function NoticeStack() {
  const notices = useApp((s) => s.notices);
  const shown = useRef(new Map<string, number>());
  const hovered = useRef(false);

  useEffect(() => {
    if (!notices.length) return;
    const timer = setInterval(() => {
      if (hovered.current || !document.hasFocus()) return;
      for (const id of tickNotices(
        shown.current,
        notices.map((n) => n.id),
        TICK_MS,
      ))
        dismissNotice(id);
    }, TICK_MS);
    return () => clearInterval(timer);
  }, [notices]);
  useEffect(() => window.sk.window?.onNotice?.((id) => void runNotice(id)), []);

  if (!notices.length) return null;
  return (
    <div
      className="plugin-notices"
      role="region"
      aria-label={t("插件通知")}
      onPointerEnter={() => (hovered.current = true)}
      onPointerLeave={() => (hovered.current = false)}
    >
      {notices.map((n) => {
        const button = noticeButton(n);
        return (
          <div key={n.id} className={`plugin-notice tone-${n.tone}`} role={n.tone === "danger" || n.tone === "warn" ? "alert" : "status"}>
            <span className="plugin-notice-icon">
              <Icon name={ICON[n.tone] ?? "info"} size={16} />
            </span>
            <div className="plugin-notice-text">
              <div className="plugin-notice-title">{n.title}</div>
              {n.body && <div className="plugin-notice-body">{n.body}</div>}
              <div className="plugin-notice-from">{pluginName(n.plugin) || n.plugin}</div>
            </div>
            <div className="plugin-notice-actions">
              <button className="icon-btn small" aria-label={t("关闭通知")} title={t("关闭通知")} onClick={() => dismissNotice(n.id)}>
                <Icon name="close" size={14} />
              </button>
              {button && (
                <button className="ghost plugin-notice-button" onClick={() => void runNotice(n.id)}>
                  {button}
                </button>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}

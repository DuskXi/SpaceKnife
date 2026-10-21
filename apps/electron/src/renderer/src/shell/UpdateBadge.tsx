// The title bar's update badge (updates.v1, state/updates.ts): there while a newer release is known,
// however far behind the app is; a running download fills it and shows its percentage. A click
// opens the update card under it; a click elsewhere or Esc closes it.

import { useEffect, useRef } from "react";

import { closeUpdate, toggleUpdateCard } from "@/state/actions";
import { useApp } from "@/state/store";
import { downloading, downloadOf, downloadPct, hasUpdate } from "@/state/updates";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { UpdateCard } from "./UpdateCard";

export function UpdateBadge() {
  const status = useApp((s) => s.update);
  const open = useApp((s) => s.updateOpen);
  const anchor = useRef<HTMLDivElement>(null);
  const badge = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!anchor.current?.contains(e.target as Node)) closeUpdate();
    };
    window.addEventListener("mousedown", away);
    return () => window.removeEventListener("mousedown", away);
  }, [open]);
  if (!hasUpdate(status)) return null;

  const version = status.latest.version;
  const d = downloadOf(status);
  const busy = downloading(d);
  const pct = downloadPct(d);
  const ready = d.state === "done";
  const label = busy ? (pct === null ? t("下载中") : `${pct}%`) : ready ? t("可以更新") : t("更新 {version}", { version });
  const tip = busy
    ? t("正在下载 SpaceKnife {version}", { version })
    : ready
      ? t("SpaceKnife {version} 已下载并校验，点开后可以重启并更新", { version })
      : d.state === "failed"
        ? t("更新下载失败：{error}", { error: d.error ?? "" })
        : t("SpaceKnife {version} 可用（你在用 {current}）", { version, current: status.current });
  return (
    <div
      ref={anchor}
      className="update-anchor"
      onKeyDown={(e) => {
        if (e.key !== "Escape" || !open) return;
        e.stopPropagation(); // not also the map's Escape (clear the selection)
        closeUpdate();
        badge.current?.focus();
      }}
    >
      <button
        ref={badge}
        className={`update-badge${open ? " on" : ""}${busy ? " busy" : ""}${ready ? " ready" : ""}`}
        aria-label={tip}
        title={tip}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={toggleUpdateCard}
      >
        {busy && <span className="update-fill" style={{ width: `${pct ?? 0}%` }} />}
        <Icon name={d.state === "done" ? "check" : d.state === "failed" ? "warning" : "up"} size={14} />
        <span className={busy ? "mono" : undefined}>{label}</span>
      </button>
      {open && <UpdateCard status={status} placement="anchored" />}
    </div>
  );
}

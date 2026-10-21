// The start page's card for 「本机磁盘」: the disks, their capacity by media and what is free, the
// fullest volume, the fastest link and how many findings there are; a click opens the page. It reads
// machine.disks with the start page (at most once a minute) and never pops up or animates.

import { useEffect } from "react";

import { openMachineTab, refreshMachine } from "@/state/actions";
import { useApp } from "@/state/store";
import { byMedia, fastest, findings, fullest, linkText, totals } from "@/state/machine";
import { fmtShare, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";
import { StackBar } from "@/ui/charts";

export function MachineCard() {
  const conn = useApp((s) => s.conn);
  const data = useApp((s) => s.machine);
  useEffect(() => {
    if (conn === "ready") void refreshMachine();
  }, [conn]);

  if (!data) {
    return (
      <button className="mcard" onClick={openMachineTab} aria-label={t("本机磁盘")}>
        <span className="mcard-head">
          <span className="mcard-icon">
            <Icon name="gauge" size={16} />
          </span>
          <span className="mcard-title">{t("本机磁盘")}</span>
          <Icon name="chevronRight" size={14} />
        </span>
        <span className="muted small">{t("正在读取磁盘…")}</span>
      </button>
    );
  }
  const sum = totals(data);
  const media = byMedia(data);
  const fast = fastest(data);
  const full = fullest(data);
  const found = findings(data);
  const warn = found.filter((f) => f.level === "warn").length;
  return (
    <button className="mcard" onClick={openMachineTab} title={t("每块盘的类型、接口与速率，以及盘上的卷")}>
      <span className="mcard-head">
        <span className="mcard-icon">
          <Icon name="gauge" size={16} />
        </span>
        <span className="mcard-title">{t("本机磁盘")}</span>
        {found.length > 0 && (
          <span className={`mcard-find${warn ? " warn" : ""}`}>
            <Icon name={warn ? "warning" : "info"} size={12} />
            {t("{n} 条发现", { n: found.length })}
          </span>
        )}
        <Icon name="chevronRight" size={14} />
      </span>
      <span className="mcard-kpis">
        <span className="mcard-kpi">
          <b>{sum.disks}</b>
          <span>{t("块盘")}</span>
        </span>
        <span className="mcard-kpi">
          <b>{fmtSize(sum.capacity, true)}</b>
          <span>{t("总容量")}</span>
        </span>
        <span className="mcard-kpi">
          <b>{fmtSize(sum.free, true)}</b>
          <span>{t("可用")}</span>
        </span>
      </span>
      {sum.capacity > 0 && (
        <StackBar
          height={6}
          title={t("容量构成")}
          segments={media.map((m) => ({ key: m.tier, share: m.bytes / sum.capacity, color: m.color, title: `${m.label} · ${fmtSize(m.bytes, true)}` }))}
        />
      )}
      <span className="mcard-legend">
        {media.map((m) => (
          <span key={m.tier}>
            <span className="chip-swatch" style={{ background: m.color }} />
            {m.label}
          </span>
        ))}
      </span>
      <span className="mcard-lines">
        {full && (
          <span className={full.share >= 0.9 ? "warn" : ""}>
            {t("最满")} <b>{full.v.volume.mount_point}</b> {fmtShare(full.v.volume.used, full.v.volume.total)}
          </span>
        )}
        {fast && (
          <span>
            {t("最快")} <b>{linkText(fast.disk.link)}</b>
          </span>
        )}
      </span>
    </button>
  );
}

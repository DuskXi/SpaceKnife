// The pure helpers of the scan progress band (shell/ScanProgress in the Electron renderer): split out
// of the component so state modules and tests can use them without a framework.

import type { IoStatus, ScanStatus } from "@/protocol/generated";
import type { LoadLevel } from "@/state/store";
import { fmtInt } from "@/lib/format";
import { t } from "@/lib/i18n";

export const isRunning = (s: ScanStatus | undefined) => s?.state === "scanning" || s?.state === "queued";

/** 「约 14 秒」「约 3 分钟」「约 1 小时 20 分」 (UI-W2-15): rounded first, then split. */
export function fmtRemaining(seconds: number): string {
  const s = Math.max(1, Math.round(seconds));
  if (s < 60) return t("约 {n} 秒", { n: s });
  const minutes = Math.round(s / 60);
  if (minutes < 60) return t("约 {n} 分钟", { n: minutes });
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return m ? t("约 {h} 小时 {m} 分", { h, m }) : t("约 {h} 小时", { h });
}

/** Progress as shown while scanning (UI-W2-10): rounded, at most 99% until the scan is done. */
export function scanPercent(progress: number): number {
  return Math.min(99, Math.round(progress * 100));
}

/** Items per second: hundreds for fast scans, exact for slow ones (UI-W2-14). */
export function fmtRate(rate: number): string {
  return fmtInt(rate >= 1000 ? Math.round(rate / 100) * 100 : Math.round(rate));
}

/** The load levels in their order, labels in the current language (read at render time, M7.5). */
export function levels(): { id: LoadLevel; label: string }[] {
  return [
    { id: "max", label: t("极速") },
    { id: "balanced", label: t("均衡 70%") },
    { id: "quiet", label: t("安静 35%") },
    { id: "idle", label: t("空闲") },
    { id: "custom", label: t("自定义") },
  ];
}

/** A load level's name without its percentage: 「均衡」. */
export function levelName(level: LoadLevel): string {
  return { max: t("极速"), balanced: t("均衡"), quiet: t("安静"), idle: t("空闲"), custom: t("自定义") }[level];
}

/** 「均衡 ≤70%」 for the status bar; 极速 and 空闲 carry no percentage (UI-W1-13). */
export function levelText(io: Pick<IoStatus, "level" | "limit">): string {
  const name = levelName(io.level);
  return io.level === "max" || io.level === "idle" ? name : `${name} ≤${Math.round(io.limit)}%`;
}

export function phaseText(phase: string): string | undefined {
  switch (phase) {
    case "helper":
      return t("正在等待管理员授权…");
    case "mft":
      return t("正在读取 MFT 主文件表…");
    case "receive":
      return t("正在接收扫描结果…");
    case "build":
      return t("正在建立目录树…");
    default:
      return undefined;
  }
}


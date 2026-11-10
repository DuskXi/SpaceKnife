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

/** The scan's target as the band names it: the volume, or the folder's last name. */
export function targetName(scan: ScanStatus, title: string): string {
  const root = scan.roots[0];
  if (!root) return title;
  if (root.whole_volume) return root.path.replace(/[\\/]+$/, "");
  return root.path.split(/[\\/]/).filter(Boolean).pop() ?? root.path;
}

/** UI-W2-43: a text summary of the 60-second load curve for assistive technology. */
export function curveSummary(io: IoStatus, start: number | null, paused: boolean): string {
  const before = io.history.filter((p) => start != null && p.t < start);
  const after = io.history.filter((p) => start == null || p.t >= start).slice(-10);
  const avg = (xs: number[]) => (xs.length ? Math.round(xs.reduce((a, b) => a + b, 0) / xs.length) : 0);
  const parts = [t("近 60 秒：")];
  if (before.length) parts.push(t("扫描开始前整盘读取占用约 {n}%；", { n: avg(before.map((p) => p.total_pct)) }));
  const self = avg(after.map((p) => p.self_pct));
  const total = avg(after.map((p) => p.total_pct));
  if (paused) parts.push(t("扫描已暂停，整盘约 {total}%", { total }));
  else parts.push(t("扫描开始后 SpaceKnife 占用约 {self}%，整盘约 {total}%", { self, total }));
  if (io.level !== "max" && io.level !== "idle") parts.push(self <= io.limit ? t("，均低于上限 {n}%", { n: Math.round(io.limit) }) : t("，上限 {n}%", { n: Math.round(io.limit) }));
  return parts.join("");
}

/** Why 「活动时间」 is shown but not limited (the load is measured by queue depth, see TODO / native.md). */
export const activeTip = (): string =>
  t("活动时间：任务管理器的口径，磁盘有请求在处理的时间占比。NVMe 固态盘上几个并发读就会到 100%，所以负载上限按请求队列折算的「占用」计算，活动时间只作参考");

/** A read latency: two decimals below 1 ms, one below 10 ms, whole milliseconds above. */
export const fmtMs = (ms: number): string => (ms < 1 ? ms.toFixed(2) : ms < 10 ? ms.toFixed(1) : String(Math.round(ms)));

const CURVE_W = 300;
const CURVE_H = 96;
const PAD_T = 6;
const PAD_B = 14;

/** UI-W2-38…43: the shapes of 「磁盘读取占用 · 近 60 秒」 in a 300 × 96 box — whole-disk area, own load
 * line, active time, the limit and where the scan started. */
export function loadCurve(io: IoStatus, startedAt: string | null) {
  const W = CURVE_W;
  const H = CURVE_H;
  const hist = io.history;
  const now = hist.length ? hist[hist.length - 1]!.t : Date.now() / 1000;
  const x = (t: number) => W * (1 - (now - t) / 60);
  const y = (pct: number) => PAD_T + (H - PAD_T - PAD_B) * (1 - Math.min(100, Math.max(0, pct)) / 100);
  const pts = hist.filter((p) => now - p.t <= 60);
  const total = pts.length ? `M ${x(pts[0]!.t)} ${y(0)} ${pts.map((p) => `L ${x(p.t).toFixed(1)} ${y(p.total_pct).toFixed(1)}`).join(" ")} L ${x(pts[pts.length - 1]!.t)} ${y(0)} Z` : "";
  const own = pts.map((p, i) => `${i ? "L" : "M"} ${x(p.t).toFixed(1)} ${y(p.self_pct).toFixed(1)}`).join(" ");
  const active = pts.map((p, i) => `${i ? "L" : "M"} ${x(p.t).toFixed(1)} ${y(p.active_pct).toFixed(1)}`).join(" ");
  const limited = io.level !== "max" && io.level !== "idle" && io.limit < 100;
  const start = startedAt ? Date.parse(startedAt) / 1000 : null;
  const showStart = start != null && now - start < 60 && now - start >= 0;
  const last = pts[pts.length - 1];
  return { W, H, PAD_T, PAD_B, x, y, total, own, active, limited, start, showStart, last };
}


// The status bar's texts (components/shell/StatusBar.vue): the live monitor, and the load level's limit.

import type { ScanStatus } from "@/protocol/generated";
import type { LoadLevel } from "@/state/store";
import { fmtInt } from "@/lib/format";
import { t } from "@/lib/i18n";

/** The limit of a load level before a scan reports its own (the start page's choice, UI-W1-13). */
export const LEVEL_CAP: Record<LoadLevel, number> = { max: 100, balanced: 70, quiet: 35, idle: 0, custom: 70 };

/** 「实时监控」 (UI-MAIN-49, UI-W1-12): the change hooks of the active scan; a click switches them. */
export function monitorText(scan: ScanStatus | undefined, on: boolean): { text: string; title: string; tone: string } {
  const m = scan?.monitor;
  const toggle = on ? t("点击关闭实时监控") : t("点击开启实时监控");
  if (!scan) return on ? { text: t("实时监控 · 扫描后自动开启"), title: toggle, tone: "muted" } : { text: t("实时监控已关闭"), title: toggle, tone: "muted" };
  if (scan.source === "snapshot") return { text: t("快照 · 不监控变化"), title: t("快照是过去的状态，不随磁盘变化"), tone: "muted" };
  if (!on || !m) return { text: t("实时监控已关闭"), title: toggle, tone: "muted" };
  const counts = m.pending ? t("{n} 处变化已更新，{pending} 项待处理", { n: fmtInt(m.changes), pending: fmtInt(m.pending) }) : t("{n} 处变化已更新", { n: fmtInt(m.changes) });
  if (m.state === "failed") return { text: t("实时监控 · 失败"), title: t("{a}。{b}", { a: m.detail || t("监视中断"), b: toggle }), tone: "warn-text" };
  if (m.state === "degraded") return { text: t("实时监控 · 部分轮询"), title: t("{a}。{b}", { a: m.detail, b: counts }), tone: "warn-text" };
  if (m.state === "starting") return { text: t("实时监控 · 启动中"), title: toggle, tone: "muted" };
  return { text: t("实时监控中"), title: t("{a}。{b}", { a: counts, b: toggle }), tone: "ok-text" };
}

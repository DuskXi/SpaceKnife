// The pure helpers of compare mode (shell/Compare in the Electron renderer): the direction choices,
// time spans, the labels of the two ends and the snapshot timeline.

import type { ChangeItem, CompareSide, CompareSourcesResult } from "@/protocol/generated";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import { NOW } from "@/state/compare";

/** How many of the largest growths and shrinks the side panel lists. */
export const LIST_LIMIT = 12;

export const KIND_ICON: Record<ChangeItem["kind"], IconName> = { file: "file", dir: "folder", link: "file", other: "file", group: "layers" };

export const DIRECTIONS = ["all", "grow", "shrink"] as const;
export type Direction = (typeof DIRECTIONS)[number];
export const directionLabel = (d: Direction): string => ({ all: t("全部"), grow: t("增长"), shrink: t("减少") })[d];

const pad = (n: number) => String(n).padStart(2, "0");
/** 「2025-03-04 09:05」 in local time. */
export function stamp(iso: string): string {
  const d = new Date(iso);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
/** 「03-04」 in local time. */
export const day = (iso: string) => {
  const d = new Date(iso);
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
};

/** "6 天 12 小时", "3 小时 20 分钟", "8 分钟" (UI-W6-24). */
export function fmtSpan(seconds: number): string {
  const s = Math.abs(seconds);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return h ? t("{d} 天 {h} 小时", { d, h }) : t("{d} 天", { d });
  if (h) return m ? t("{h} 小时 {m} 分钟", { h, m }) : t("{h} 小时", { h });
  return t("{m} 分钟", { m: Math.max(1, m) });
}

/** The label of one end in the picker and the timeline. */
export function endLabel(spec: string, sources: CompareSourcesResult | null): string {
  if (spec === NOW) return t("现在");
  const snap = sources?.snapshots.find((s) => s.path === spec);
  return snap ? t("{time} 快照", { time: stamp(snap.created_at) }) : (spec.split(/[\\/]/).pop() ?? spec);
}

export const sideName = (side: CompareSide) => (side.kind === "now" ? t("现在") : stamp(side.created_at));

export interface TimelinePoint {
  spec: string;
  at: string;
}

/** 快照时间轴 (UI-W6-33…35): the points in time order, their position in percent, the span between
 * base and target, and which points are labelled (labels of points closer than 12 % to a shown one
 * are left out; base and target first). */
export function timeline(sources: CompareSourcesResult | null, base: string, target: string, nowIso = new Date().toISOString()) {
  const snaps = sources?.snapshots ?? [];
  const nowAt = sources?.now?.created_at ?? nowIso;
  const points: TimelinePoint[] = [...snaps.map((s) => ({ spec: s.path, at: s.created_at })), ...(sources?.now ? [{ spec: NOW, at: nowAt }] : [])].sort((a, b) => a.at.localeCompare(b.at));
  const t0 = points.length ? Date.parse(points[0]!.at) : 0;
  const t1 = points.length ? Date.parse(points[points.length - 1]!.at) : 1;
  const pos = (at: string) => (t1 > t0 ? ((Date.parse(at) - t0) / (t1 - t0)) * 100 : 100);
  const at = (spec: string) => points.find((p) => p.spec === spec)?.at;
  const a = at(base);
  const b = at(target);
  const lo = a && b ? Math.min(pos(a), pos(b)) : 0;
  const hi = a && b ? Math.max(pos(a), pos(b)) : 0;
  const labelled = new Set<string>();
  const order = [...points].sort((p, q) => Number(q.spec === base || q.spec === target) - Number(p.spec === base || p.spec === target));
  for (const p of order) if ([...labelled].every((l) => Math.abs(pos(at(l)!) - pos(p.at)) >= 12)) labelled.add(p.spec);
  return { points, pos, lo, hi, labelled };
}

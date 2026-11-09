// Small helpers of 「本机磁盘」 (pages/MachinePage.vue and its parts): times, the full mark, class
// names and a volume's scan line.

import type { VolumeScan } from "@/protocol/generated";
import { fmtAgo, fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { get } from "@/state/store";

/** Unix seconds of an ISO time (0 when there is none). */
export const seconds = (iso: string | null | undefined): number => (iso ? Math.floor(new Date(iso).getTime() / 1000) || 0 : 0);

/** A volume this full is a finding (status colour, with its icon and words). */
export const FULL = 0.9;

/** A file class's name from the vocabulary (its id when unknown). */
export function className(id: string): string {
  return get().vocab?.classes.find((c) => c.id === id)?.name ?? id;
}

/** A row of the classes panel: a class, or the rest folded together. */
export function rowName(r: { classId: string | null; count: number }): string {
  return r.classId ? className(r.classId) : t("其余 {n} 类", { n: r.count });
}

/** What SpaceKnife knows of a volume's scan, in one line. */
export function scanSummary(s: VolumeScan): string {
  const files = t("{n} 个文件", { n: fmtInt(s.files) });
  if (s.running) return t("正在扫描 · {files}", { files });
  const when = s.at ? fmtAgo(seconds(s.at)) : "";
  const size = fmtSize(s.bytes_alloc, true);
  // an open scan, or what was kept of the last complete one: its own time; else the newest snapshot
  return s.source === "snapshot" ? t("快照 {when} · {files} · {size}", { when, files, size }) : t("扫描于 {when} · {files} · {size}", { when, files, size });
}

// The start page's words for volumes and scan options (pages/StartPage.vue, pages/MachinePage.vue):
// media and load names, a volume's name, whether the MFT fast scan can read it, what it will be
// scanned with, and the recent scans list.

import type { SnapshotInfo, VolumeInfo } from "@/protocol/generated";
import { fmtKeys, fmtShare } from "@/lib/format";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import type { LoadLevel, ScanMethod } from "@/state/store";

// the labels translate each time they are read (getters), so a language switch applies (M7.5)
export const MEDIA_ZH: Record<string, string> = {
  nvme_ssd: "NVMe SSD",
  sata_ssd: "SATA SSD",
  ssd: "SSD",
  get hdd() {
    return t("机械硬盘");
  },
  usb_hdd: "USB HDD",
  usb_ssd: "USB SSD",
  get usb_flash() {
    return t("U 盘");
  },
  get sd_card() {
    return t("存储卡");
  },
  get network() {
    return t("网络");
  },
  get virtual() {
    return t("虚拟磁盘");
  },
  get unknown() {
    return t("未知介质");
  },
};
export const LOAD_ZH: Record<LoadLevel, [string, string]> = {
  get max(): [string, string] {
    return [t("极速"), t("不限制磁盘读取")];
  },
  get balanced(): [string, string] {
    return [t("均衡 70%"), t("扫描最多占用 70% 的磁盘读取，给前台程序留出余量")];
  },
  get quiet(): [string, string] {
    return [t("安静 35%"), t("扫描最多占用 35% 的磁盘读取，几乎感觉不到")];
  },
  get idle(): [string, string] {
    return [t("空闲"), t("只在磁盘空闲时读取，并使用系统低 I/O 优先级")];
  },
  get custom(): [string, string] {
    return [t("自定义"), t("按设置里的「自定义上限」")];
  },
};
export const METHODS: ScanMethod[] = ["auto", "mft", "hard"];
export const LOADS: LoadLevel[] = ["max", "balanced", "quiet", "idle"];

export function methodText(m: ScanMethod): [string, string] {
  const table: Record<ScanMethod, [string, string]> = {
    auto: [t("自动"), t("整个 NTFS 卷用 MFT 快速扫描，其他位置逐目录硬扫描")],
    mft: [t("快速 MFT"), t("读取 NTFS 主文件表，整盘几秒完成（需要管理员）")],
    hard: [t("硬扫描"), t("逐目录遍历，耗时随文件数量增长")],
  };
  return table[m];
}
export const IS_WIN = () => window.sk.platform === "win32";

/** 「C: 系统」, or the folder name of a mount point on Linux / macOS. */
export function volumeName(v: VolumeInfo): string {
  const mp = v.mount_point.replace(/[\\/]+$/, "") || v.mount_point;
  return v.label && v.label !== mp && v.label !== mp.split("/").pop() ? `${mp} ${v.label}` : mp;
}

/** A whole local NTFS volume on Windows: the MFT fast scan can read it (UI-W1-18). */
export function mftCapable(v: VolumeInfo | null | undefined): boolean {
  return !!v && IS_WIN() && !v.reason && !v.is_remote && v.fs_type.toUpperCase() === "NTFS";
}

/** What this volume will actually be scanned with (UI-W1-18). */
export function methodBadge(v: VolumeInfo, method: ScanMethod, elevated: boolean): { icon: IconName; text: string } {
  if (v.reason) return { icon: "warning", text: v.reason };
  if (v.note) return { icon: "warning", text: t("不推荐 · 很慢") };
  if (method !== "hard" && mftCapable(v)) return { icon: "shield", text: elevated ? t("MFT 快速扫描") : t("MFT 快速扫描 · 需要管理员") };
  if (v.is_remote) return { icon: "clock", text: t("网络扫描 · 较慢") };
  if (v.media === "usb_hdd") return { icon: "gauge", text: t("并行硬扫描 · USB HDD 自动降速") };
  return { icon: "gauge", text: t("并行硬扫描") };
}

/** The same volume root (case-insensitive on Windows, trailing separators ignored). */
export function sameRoot(a: string, b: string): boolean {
  const norm = (x: string) => {
    const p = x.trim().replace(/[\\/]+$/, "");
    return IS_WIN() ? p.toLowerCase() : p || "/";
  };
  return norm(a) === norm(b);
}

/** The paths typed in the path box (`;`-separated). */
export const splitPaths = (text: string): string[] =>
  text
    .split(";")
    .map((x) => x.trim())
    .filter(Boolean);

/** The last folder name of a path. */
export const baseName = (path: string): string | undefined => path.split(/[\\/]/).filter(Boolean).pop();

/** A snapshot's date: today / yesterday with the time, else the date. */
export function fmtWhen(iso: string, now = new Date()): string {
  const d = new Date(iso);
  const hm = d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false });
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(now) - day(d)) / 86400000);
  if (diff === 0) return t("今天 {time}", { time: hm });
  if (diff === 1) return t("昨天 {time}", { time: hm });
  const pad = (n: number) => String(n).padStart(2, "0");
  if (d.getFullYear() === now.getFullYear()) return `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export interface RecentRecord {
  key: string;
  snap: SnapshotInfo;
  name: string;
  volume: boolean;
}

/** The recent scans (UI-W1-38): the newest per target unless `all`, named after the volume. */
export function recentRecords(snapshots: SnapshotInfo[], volumes: VolumeInfo[], all: boolean): RecentRecord[] {
  const seen = new Set<string>();
  const out: RecentRecord[] = [];
  for (const s of snapshots) {
    const key = s.roots.map((r) => r.toLowerCase()).join(";");
    if (!all && seen.has(key)) continue; // newest per target unless showing everything
    seen.add(key);
    const vol = s.roots.length === 1 ? volumes.find((v) => sameRoot(v.mount_point, s.roots[0]!)) : undefined;
    const name = vol ? volumeName(vol) : s.roots.length > 1 ? t("{n} 个位置", { n: s.roots.length }) : (baseName(s.roots[0] ?? "") ?? s.roots[0] ?? "");
    out.push({ key: s.path, snap: s, name, volume: !!vol });
  }
  return out;
}

export const fmtShareOf = fmtShare;
export const keysOf = fmtKeys;

// 「本机磁盘」 (machine.disks): the words and numbers of the page, kept out of the component so they
// are tested. Rates are line rates as the interfaces are sold (SATA 6 Gb/s, USB 5 Gb/s); PCIe links
// are named by generation and lanes, with what they carry after encoding (≈ GB/s).

import type { DiskLink, MachineDisks, PhysicalDisk, SnapshotPoint, VolumeOverview } from "@/protocol/generated";
import { fmtAgo, fmtSize } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";

/** MB/s one lane carries after encoding, per PCIe generation (1-6). */
const PCIE_LANE_MBPS = [0, 250, 500, 985, 1969, 3938, 7563];
/** MB/s a SATA link carries after encoding (8b/10b), per line rate. */
const SATA_MBPS: Record<number, number> = { 1500: 150, 3000: 300, 6000: 600 };

export function pcieName(gen: number | null | undefined, width: number | null | undefined): string {
  const g = gen ? `PCIe ${gen}.0` : "PCIe";
  return width ? `${g} ×${width}` : g;
}

/** MB/s of a PCIe link, null when the generation or the lanes are unknown. */
export function pcieMBps(gen: number | null | undefined, width: number | null | undefined): number | null {
  return gen && width && PCIE_LANE_MBPS[gen] ? PCIE_LANE_MBPS[gen]! * width : null;
}

/** "480 Mb/s", "5 Gb/s", "1.5 Gb/s". */
export function rateText(mbps: number): string {
  if (mbps < 1000) return `${mbps} Mb/s`;
  const g = mbps / 1000;
  return `${Number.isInteger(g) ? g : g.toFixed(1)} Gb/s`;
}

/** "≈ 7.9 GB/s", "≈ 500 MB/s". */
export function bandwidthText(mbytes: number): string {
  return mbytes >= 1000 ? `≈ ${(mbytes / 1000).toFixed(1)} GB/s` : `≈ ${mbytes} MB/s`;
}

/** The link in a few words: "PCIe 4.0 ×4", "SATA 6 Gb/s", "USB 480 Mb/s"; null when nothing is known. */
export function linkText(link: DiskLink | null | undefined): string | null {
  if (!link) return null;
  if (link.kind === "pcie") return link.pcie_gen || link.pcie_width ? pcieName(link.pcie_gen, link.pcie_width) : null;
  if (!link.rate_mbps) return null;
  return `${link.kind === "sata" ? "SATA" : "USB"} ${rateText(link.rate_mbps)}`;
}

/** What the link carries, for PCIe ("≈ 7.9 GB/s"); null otherwise. */
export function linkBandwidth(link: DiskLink | null | undefined): string | null {
  if (link?.kind !== "pcie") return null;
  const mb = pcieMBps(link.pcie_gen, link.pcie_width);
  return mb ? bandwidthText(mb) : null;
}

export interface LinkNote {
  text: string;
  /** a real limit on the disk's speed (a slow controller), not just headroom */
  warn: boolean;
}

/** A link that runs below what the disk supports, or a controller narrower than the disk's link. */
export function linkNotes(disk: PhysicalDisk): LinkNote[] {
  const out: LinkNote[] = [];
  const l = disk.link;
  if (l?.kind === "pcie" && l.max_pcie_gen && l.pcie_gen && (l.max_pcie_gen > l.pcie_gen || (l.max_pcie_width ?? 0) > (l.pcie_width ?? 0))) {
    out.push({
      text: t("这块盘支持 {max}，现在的插槽只给到 {now}", { max: pcieName(l.max_pcie_gen, l.max_pcie_width ?? l.pcie_width), now: pcieName(l.pcie_gen, l.pcie_width) }),
      warn: false,
    });
  }
  if (l?.kind === "usb" && l.rate_mbps && l.max_rate_mbps && l.max_rate_mbps > l.rate_mbps) {
    out.push({
      text: t("设备支持 {max}，现在只以 {now} 连接：可能插在 USB 2 接口、集线器或线材上", { max: rateText(l.max_rate_mbps), now: rateText(l.rate_mbps) }),
      warn: false,
    });
  }
  if (l?.kind === "sata" && l.rate_mbps && l.max_rate_mbps && l.max_rate_mbps > l.rate_mbps) {
    out.push({ text: t("这块盘支持 {max}，现在以 {now} 连接：接口或线材限制了速度", { max: rateText(l.max_rate_mbps), now: rateText(l.rate_mbps) }), warn: false });
  }
  const c = disk.controller_link;
  const cMB = c?.kind === "pcie" ? pcieMBps(c.pcie_gen, c.pcie_width) : null;
  if (c && cMB) {
    const diskMB = l?.kind === "sata" && l.rate_mbps ? (SATA_MBPS[l.rate_mbps] ?? null) : null;
    // a real limit only when the controller carries clearly less than the disk's link
    const slow = diskMB !== null && cMB < diskMB * 0.75;
    const narrower = diskMB !== null && cMB < diskMB;
    out.push({
      text: narrower
        ? t("经 {link} 控制器（{bw}），比这块盘的 SATA 链路慢：连续读写会受限", { link: pcieName(c.pcie_gen, c.pcie_width), bw: bandwidthText(cMB) })
        : t("经 {link} 控制器（{bw}）", { link: pcieName(c.pcie_gen, c.pcie_width), bw: bandwidthText(cMB) }),
      warn: slow,
    });
  }
  return out;
}

/** The interface in words. */
export function interfaceName(i: PhysicalDisk["interface"]): string {
  switch (i) {
    case "nvme":
      return "NVMe";
    case "sata":
      return "SATA";
    case "sas":
      return "SAS";
    case "scsi":
      return "SCSI";
    case "ata":
      return "ATA";
    case "usb":
      return "USB";
    case "sd":
      return t("存储卡");
    case "thunderbolt":
      return "Thunderbolt";
    case "virtual":
      return t("虚拟磁盘");
    case "raid":
      return "RAID";
    case "other":
      return t("其他接口");
    default:
      return t("未知接口");
  }
}

/** The disk's name: its model (with the vendor when the model doesn't start with it), else its id. */
export function diskName(d: PhysicalDisk): string {
  const model = d.model.trim();
  const vendor = d.vendor.trim();
  const name = vendor && model && !model.toLowerCase().startsWith(vendor.toLowerCase()) ? `${vendor} ${model}` : model || vendor;
  return name || diskLabel(d);
}

/** How the system numbers it: "磁盘 3" on Windows, else the device name. */
export function diskLabel(d: PhysicalDisk): string {
  return d.number != null ? t("磁盘 {n}", { n: d.number }) : d.id;
}

/** The page's summary line: how many disks of each kind and their total size. */
export function summary(data: MachineDisks): { counts: string; total: number } {
  const order: PhysicalDisk["interface"][] = ["nvme", "sata", "sas", "scsi", "ata", "usb", "thunderbolt", "sd", "raid", "virtual", "other", "unknown"];
  const by = new Map<string, number>();
  const present = data.disks.filter((d) => d.size != null); // an empty card slot is no disk
  for (const d of present) by.set(d.interface, (by.get(d.interface) ?? 0) + 1);
  const counts = order
    .filter((i) => by.has(i))
    .map((i) => t("{n} 块 {name}", { n: by.get(i)!, name: interfaceName(i) }))
    .join(byLang("、", ", "));
  return { counts, total: present.reduce((sum, d) => sum + (d.size ?? 0), 0) };
}

export interface MapSegment {
  mount: string;
  /** share of the disk (0-1) */
  share: number;
  /** share of the volume in use (0-1) */
  used: number;
}

/** The disk's capacity bar: its volumes by their size on it; the rest (other partitions, free) stays
 * empty. Volumes whose size on the disk is unknown (APFS, spanned) share what is left evenly. */
export function diskMap(disk: PhysicalDisk, volumes: VolumeOverview[]): MapSegment[] {
  const size = disk.size ?? 0;
  if (!size || !disk.parts.length) return [];
  const byMount = new Map(volumes.map((v) => [v.volume.mount_point, v.volume]));
  const known = disk.parts.reduce((sum, p) => sum + (p.bytes ?? 0), 0);
  const unknown = disk.parts.filter((p) => p.bytes == null).length;
  const each = unknown ? Math.max(0, size - known) / unknown : 0;
  return disk.parts.map((p) => {
    const v = byMount.get(p.mount_point);
    return {
      mount: p.mount_point,
      share: Math.min(1, (p.bytes ?? each) / size),
      used: v && v.total ? Math.min(1, v.used / v.total) : 0,
    };
  });
}

/** The disks in the order people look for them: the system disk, then by their first volume
 * (C:, D:, … or /, /home, …), then those without a volume; empty card slots last. */
export function orderedDisks(data: MachineDisks): PhysicalDisk[] {
  const rank = (d: PhysicalDisk): [number, string, number] => [
    d.size == null ? 3 : d.system ? 0 : d.parts.length ? 1 : 2,
    d.parts[0]?.mount_point.toLowerCase() ?? "",
    d.number ?? 0,
  ];
  return [...data.disks].sort((a, b) => {
    const [x, y] = [rank(a), rank(b)];
    return x[0] - y[0] || x[1].localeCompare(y[1], undefined, { numeric: true }) || x[2] - y[2] || a.id.localeCompare(b.id, undefined, { numeric: true });
  });
}

/** The capacity bar says something only when the disk holds more than one volume, or space
 * outside its volumes (other partitions, unallocated): one volume over the whole disk repeats
 * the volume's own bar. */
export function showMap(segments: MapSegment[]): boolean {
  if (segments.length > 1) return true;
  const covered = segments.reduce((sum, s) => sum + s.share, 0);
  return segments.length === 1 && covered < 0.95;
}

/** The volumes of the page: those on each disk (in the disk's order), then the ones on no known disk. */
export function volumesOf(data: MachineDisks, disk: PhysicalDisk): VolumeOverview[] {
  const by = new Map(data.volumes.map((v) => [v.volume.mount_point, v]));
  return disk.parts.map((p) => by.get(p.mount_point)).filter((v): v is VolumeOverview => !!v);
}

export function elsewhere(data: MachineDisks): VolumeOverview[] {
  const placed = new Set(data.disks.flatMap((d) => d.parts.map((p) => p.mount_point)));
  return data.volumes.filter((v) => !placed.has(v.volume.mount_point));
}

// ------------------------------------------------------------------ the dashboard's analysis

const seconds = (iso: string): number => Math.floor(new Date(iso).getTime() / 1000) || 0;

/** A volume that takes part in the totals: local, with a size, readable now. */
const local = (v: VolumeOverview): boolean => !v.volume.is_remote && v.volume.total > 0 && !v.volume.reason;

export interface Totals {
  /** disks with a medium */
  disks: number;
  /** their bytes */
  capacity: number;
  volumes: number;
  /** volumes SpaceKnife knows from a scan or a snapshot */
  known: number;
  /** used and free bytes of the local volumes on these disks */
  used: number;
  free: number;
}

export function totals(data: MachineDisks): Totals {
  const present = data.disks.filter((d) => d.size != null);
  const vols = data.volumes.filter(local);
  const onDisks = vols.filter((v) => v.disks.length > 0);
  return {
    disks: present.length,
    capacity: present.reduce((sum, d) => sum + (d.size ?? 0), 0),
    volumes: vols.length,
    known: vols.filter((v) => v.scan).length,
    used: onDisks.reduce((sum, v) => sum + v.volume.used, 0),
    free: onDisks.reduce((sum, v) => sum + v.volume.free, 0),
  };
}

export type MediaTier = "nvme" | "sata" | "hdd" | "external" | "other";

export interface MediaShare {
  tier: MediaTier;
  label: string;
  bytes: number;
  disks: number;
  /** a step of one hue, brighter for faster media (design tokens visual.branch.1.*) */
  color: string;
}

const TIER_OF: Record<PhysicalDisk["media"], MediaTier> = {
  nvme_ssd: "nvme",
  sata_ssd: "sata",
  ssd: "sata",
  hdd: "hdd",
  usb_hdd: "external",
  usb_ssd: "external",
  usb_flash: "external",
  sd_card: "external",
  network: "other",
  virtual: "other",
  unknown: "other",
};
const TIERS: MediaTier[] = ["nvme", "sata", "hdd", "external", "other"];
const TIER_COLOR: Record<MediaTier, string> = {
  nvme: "var(--sk-visual-branch-1-1)",
  sata: "var(--sk-visual-branch-1-2)",
  hdd: "var(--sk-visual-branch-1-3)",
  external: "var(--sk-visual-branch-1-4)",
  other: "var(--sk-visual-branch-neutral)",
};

export function tierName(tier: MediaTier): string {
  switch (tier) {
    case "nvme":
      return "NVMe SSD";
    case "sata":
      return t("SATA 及其他 SSD");
    case "hdd":
      return t("机械硬盘");
    case "external":
      return t("外接盘与存储卡");
    default:
      return t("其他");
  }
}

/** The disks' capacity by media, fastest first (the part-to-whole bar); empty tiers left out. */
export function byMedia(data: MachineDisks): MediaShare[] {
  const sums = new Map<MediaTier, { bytes: number; disks: number }>();
  for (const d of data.disks) {
    if (d.size == null) continue;
    const tier = TIER_OF[d.media] ?? "other";
    const s = sums.get(tier) ?? { bytes: 0, disks: 0 };
    sums.set(tier, { bytes: s.bytes + d.size, disks: s.disks + 1 });
  }
  return TIERS.filter((tier) => sums.has(tier)).map((tier) => ({ tier, label: tierName(tier), color: TIER_COLOR[tier], ...sums.get(tier)! }));
}

/** MB/s a link carries (PCIe after encoding, SATA after 8b/10b, USB about a tenth of its line rate);
 * `top`: the most the disk supports. */
export function linkMBps(link: DiskLink | null | undefined, top = false): number | null {
  if (!link) return null;
  if (link.kind === "pcie") return top ? pcieMBps(link.max_pcie_gen, link.max_pcie_width) : pcieMBps(link.pcie_gen, link.pcie_width);
  const rate = top ? link.max_rate_mbps : link.rate_mbps;
  if (!rate) return null;
  return link.kind === "sata" ? (SATA_MBPS[rate] ?? Math.round(rate / 10)) : Math.round(rate / 10);
}

/** How much of what the disk supports its link carries now (0-1); null when the most isn't known. */
export function linkShare(link: DiskLink | null | undefined): number | null {
  const now = linkMBps(link);
  const top = linkMBps(link, true);
  return now && top ? Math.min(1, now / top) : null;
}

/** The disk with the fastest link. */
export function fastest(data: MachineDisks): { disk: PhysicalDisk; mbps: number } | null {
  let best: { disk: PhysicalDisk; mbps: number } | null = null;
  for (const d of data.disks) {
    const mbps = d.size != null ? linkMBps(d.link) : null;
    if (mbps && (!best || mbps > best.mbps)) best = { disk: d, mbps };
  }
  return best;
}

/** The local volume with the least room left, by share used. */
export function fullest(data: MachineDisks): { v: VolumeOverview; share: number } | null {
  let best: { v: VolumeOverview; share: number } | null = null;
  for (const v of data.volumes.filter(local)) {
    const share = v.volume.used / v.volume.total;
    if (!best || share > best.share) best = { v, share };
  }
  return best;
}

/** Bytes in use on a disk: its volumes' used space (a volume over several disks counts by its
 * share here, when known); null when nothing on it is known. */
export function diskUsed(disk: PhysicalDisk, data: MachineDisks): number | null {
  const by = new Map(data.volumes.map((v) => [v.volume.mount_point, v]));
  let used = 0;
  let known = false;
  for (const p of disk.parts) {
    const v = by.get(p.mount_point);
    if (!v || !v.volume.total) continue;
    if (v.disks.length <= 1) used += v.volume.used;
    else if (p.bytes != null) used += (v.volume.used * p.bytes) / v.volume.total;
    else continue;
    known = true;
  }
  return known ? used : null;
}

/** Growth between the two newest complete snapshots of a volume. */
export function growth(history: SnapshotPoint[]): { delta: number; since: string } | null {
  if (history.length < 2) return null;
  return { delta: history[0]!.bytes_alloc - history[1]!.bytes_alloc, since: history[1]!.at };
}

export interface Finding {
  level: "warn" | "info";
  text: string;
}

const pct = (share: number): string => `${Math.round(share * 100)}%`;
const SSD: PhysicalDisk["media"][] = ["nvme_ssd", "sata_ssd", "ssd", "usb_ssd"];

/** What is worth knowing, the ones to act on first: nearly full volumes, links below what the disk
 * supports, a narrow controller, an SSD without TRIM, a volume that grew a lot since its last
 * snapshot. Nothing about volumes not scanned yet. */
export function findings(data: MachineDisks, now = Date.now() / 1000): Finding[] {
  const out: Finding[] = [];
  const sep = byLang("：", ": ");
  for (const v of data.volumes.filter(local)) {
    const share = v.volume.used / v.volume.total;
    const mount = v.volume.mount_point;
    if (share >= 0.9) out.push({ level: "warn", text: t("{mount} 已用 {pct}，只剩 {free}", { mount, pct: pct(share), free: fmtSize(v.volume.free, true) }) });
    else if (share >= 0.8) out.push({ level: "info", text: t("{mount} 已用 {pct}", { mount, pct: pct(share) }) });
    const g = growth(v.history);
    if (g && Math.abs(g.delta) >= Math.max(5 * 2 ** 30, v.volume.total * 0.02)) {
      const args = { mount, size: fmtSize(Math.abs(g.delta), true), when: fmtAgo(seconds(g.since), now) };
      out.push({ level: "info", text: g.delta > 0 ? t("{mount} 比 {when} 的快照多了 {size}", args) : t("{mount} 比 {when} 的快照少了 {size}", args) });
    }
  }
  for (const d of data.disks) {
    if (d.size == null) continue;
    for (const n of linkNotes(d)) out.push({ level: n.warn ? "warn" : "info", text: `${diskName(d)}${sep}${n.text}` });
    out.push(...healthFindings(d));
    if (d.trim === false && SSD.includes(d.media)) out.push({ level: "info", text: t("{disk} 不支持 TRIM：长期写入后可能变慢", { disk: diskName(d) }) });
  }
  return [...out.filter((f) => f.level === "warn"), ...out.filter((f) => f.level === "info")];
}

// ------------------------------------------------------------------ what disks record about themselves

/** Why a disk shows no record of its own (or only part of it); "" when there's nothing to say. */
export function healthNoteText(note: PhysicalDisk["health_note"]): string {
  switch (note) {
    case "usb":
      return t("USB 桥接没有把磁盘自己的记录传过来");
    case "unsupported":
      return t("这块盘没有回应健康记录的查询");
    case "needs_admin":
      return t("磁盘自己记录的累计读写要管理员权限才能读");
    case "unavailable":
      return t("这个系统读不到磁盘自己的健康记录");
    default:
      return "";
  }
}

/** Why a part of the disk's own record shows "—": the reason it has none, or that the disk
 * doesn't keep that part. */
export function recordGap(d: PhysicalDisk): string {
  return healthNoteText(d.health_note) || (d.health ? t("这块盘的记录里没有这一项") : t("这块盘没有提供自己的记录"));
}

/** Power-on time in words: hours under two days, else days. */
export function powerOnText(hours: number): string {
  return hours < 48 ? t("{n} 小时", { n: hours }) : t("{n} 天", { n: Math.floor(hours / 24).toLocaleString("en-US") });
}

/** What a disk's own record says that is worth knowing: a critical warning, media errors, wear,
 * spare blocks running out, heat. */
export function healthFindings(d: PhysicalDisk): Finding[] {
  const h = d.health;
  if (!h) return [];
  const out: Finding[] = [];
  const disk = diskName(d);
  if (h.critical_warning) out.push({ level: "warn", text: t("{disk} 报告了严重警告", { disk }) });
  if (h.media_errors) out.push({ level: "warn", text: t("{disk} 记录了 {n} 个介质错误", { disk, n: h.media_errors }) });
  if (h.percentage_used != null && h.percentage_used >= 70) {
    out.push({ level: h.percentage_used >= 90 ? "warn" : "info", text: t("{disk} 已用掉 {pct} 的写入寿命", { disk, pct: `${h.percentage_used}%` }) });
  }
  if (h.available_spare != null && h.spare_threshold && h.available_spare <= h.spare_threshold) {
    out.push({ level: "warn", text: t("{disk} 的备用块只剩 {pct}", { disk, pct: `${h.available_spare}%` }) });
  }
  if (h.temperature_c != null && h.temperature_c >= 70) out.push({ level: "info", text: t("{disk} 现在 {n} °C，偏热", { disk, n: h.temperature_c }) });
  return out;
}

// 「本机磁盘」 (machine.disks): the words and numbers of the page, kept out of the component so they
// are tested. Rates are line rates as the interfaces are sold (SATA 6 Gb/s, USB 5 Gb/s); PCIe links
// are named by generation and lanes, with what they carry after encoding (≈ GB/s).

import type { DiskLink, MachineDisks, PhysicalDisk, VolumeOverview } from "@/protocol/generated";
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

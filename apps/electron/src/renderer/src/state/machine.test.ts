import { describe, expect, it } from "vitest";

import type { DiskHealth, DiskLink, MachineDisks, PhysicalDisk, VolumeInfo, VolumeOverview } from "@/protocol/generated";

import { bandwidthText, byMedia, healthFindings, healthNoteText,
  recordGap, powerOnText, diskMap, diskName, diskUsed, elsewhere, fastest, findings, fullest, growth, linkBandwidth, linkMBps, linkNotes, linkShare, linkText, orderedDisks, pcieMBps, rateText, showMap, summary, totals, volumesOf } from "./machine";

const link = (patch: Partial<DiskLink>): DiskLink => ({
  kind: "pcie",
  rate_mbps: null,
  max_rate_mbps: null,
  pcie_gen: null,
  pcie_width: null,
  max_pcie_gen: null,
  max_pcie_width: null,
  ...patch,
});

const disk = (patch: Partial<PhysicalDisk> = {}): PhysicalDisk => ({
  id: "PhysicalDrive0",
  number: 0,
  model: "Disk",
  vendor: "",
  serial: "",
  firmware: "",
  size: 1000,
  media: "nvme_ssd",
  interface: "nvme",
  removable: false,
  trim: true,
  link: null,
  controller_link: null,
  system: false,
  parts: [],
  health: null,
  health_note: "",
  io: null,
  ...patch,
});

const volume = (mount: string, used = 50, total = 100): VolumeOverview => ({
  volume: {
    mount_point: mount,
    device: "",
    fs_type: "NTFS",
    label: "",
    total,
    free: total - used,
    used,
    cluster_size: 4096,
    is_remote: false,
    is_removable: false,
    serial: "",
    media: "nvme_ssd",
    is_system: false,
    remote_path: "",
    reason: "",
    note: "",
    reserved: 0,
  } as VolumeInfo,
  disks: [],
  scan: null,
  history: [],
});

describe("links", () => {
  it("names links and their rates", () => {
    expect(linkText(link({ pcie_gen: 4, pcie_width: 4 }))).toBe("PCIe 4.0 ×4");
    expect(linkText(link({ kind: "sata", rate_mbps: 6000 }))).toBe("SATA 6 Gb/s");
    expect(linkText(link({ kind: "usb", rate_mbps: 480 }))).toBe("USB 480 Mb/s");
    expect(linkText(link({ kind: "sata", rate_mbps: 1500 }))).toBe("SATA 1.5 Gb/s");
    expect(linkText(link({}))).toBeNull();
    expect(linkText(null)).toBeNull();
    expect(rateText(10000)).toBe("10 Gb/s");
  });

  it("gives what a PCIe link carries", () => {
    expect(pcieMBps(4, 4)).toBe(7876);
    expect(pcieMBps(2, 1)).toBe(500);
    expect(pcieMBps(null, 4)).toBeNull();
    expect(linkBandwidth(link({ pcie_gen: 4, pcie_width: 4 }))).toBe("≈ 7.9 GB/s");
    expect(linkBandwidth(link({ kind: "sata", rate_mbps: 6000 }))).toBeNull();
    expect(bandwidthText(500)).toBe("≈ 500 MB/s");
  });

  it("says when a link runs below what the disk supports", () => {
    const slot = linkNotes(disk({ link: link({ pcie_gen: 4, pcie_width: 4, max_pcie_gen: 5, max_pcie_width: 4 }) }));
    expect(slot).toHaveLength(1);
    expect(slot[0]!.text).toContain("PCIe 5.0 ×4");
    expect(slot[0]!.warn).toBe(false);
    expect(linkNotes(disk({ link: link({ pcie_gen: 4, pcie_width: 4, max_pcie_gen: 4, max_pcie_width: 4 }) }))).toEqual([]);
    const usb = linkNotes(disk({ interface: "usb", link: link({ kind: "usb", rate_mbps: 480, max_rate_mbps: 5000 }) }));
    expect(usb[0]!.text).toContain("5 Gb/s");
    expect(linkNotes(disk({ interface: "usb", link: link({ kind: "usb", rate_mbps: 5000, max_rate_mbps: 5000 }) }))).toEqual([]);
  });

  it("names the controller a SATA disk hangs on, and warns only for a clear bottleneck", () => {
    const sata = link({ kind: "sata", rate_mbps: 6000, max_rate_mbps: 6000 });
    // PCIe 2.0 ×1 (500 MB/s) under SATA 6 Gb/s (600 MB/s): slower, but not by much
    const narrow = linkNotes(disk({ interface: "sata", link: sata, controller_link: link({ pcie_gen: 2, pcie_width: 1 }) }));
    expect(narrow).toHaveLength(1);
    expect(narrow[0]!.warn).toBe(false);
    expect(narrow[0]!.text).toContain("PCIe 2.0 ×1");
    // PCIe 1.0 ×1 (250 MB/s): a real limit
    expect(linkNotes(disk({ interface: "sata", link: sata, controller_link: link({ pcie_gen: 1, pcie_width: 1 }) }))[0]!.warn).toBe(true);
    // a wide controller: named, no warning
    const wide = linkNotes(disk({ interface: "sata", link: sata, controller_link: link({ pcie_gen: 3, pcie_width: 2 }) }));
    expect(wide).toHaveLength(1);
    expect(wide[0]!.warn).toBe(false);
  });
});

describe("disks", () => {
  it("names a disk by its model, with the vendor when the model lacks it", () => {
    expect(diskName(disk({ model: "STORAGE DEVICE", vendor: "Generic" }))).toBe("Generic STORAGE DEVICE");
    expect(diskName(disk({ model: "WD_BLACK SN850X", vendor: "WD" }))).toBe("WD_BLACK SN850X");
    expect(diskName(disk({ model: "", vendor: "", number: 3 }))).toBe("磁盘 3");
    expect(diskName(disk({ model: "", vendor: "", number: null, id: "sdb" }))).toBe("sdb");
  });

  it("puts the system disk first, then by volume, empty slots last", () => {
    const data: MachineDisks = {
      collected_at: "2026-10-21T00:00:00Z",
      volumes: [],
      disks: [
        disk({ id: "PhysicalDrive0", number: 0, parts: [{ mount_point: "D:\\", bytes: 10 }] }),
        disk({ id: "PhysicalDrive9", number: 9, size: null, parts: [{ mount_point: "J:\\", bytes: null }] }),
        disk({ id: "PhysicalDrive5", number: 5 }),
        disk({ id: "PhysicalDrive3", number: 3, system: true, parts: [{ mount_point: "C:\\", bytes: 10 }] }),
        disk({ id: "PhysicalDrive1", number: 1, parts: [{ mount_point: "F:\\", bytes: 10 }] }),
      ],
    };
    expect(orderedDisks(data).map((d) => d.number)).toEqual([3, 0, 1, 5, 9]);
    const s = summary(data);
    expect(s.total).toBe(4000); // the empty slot has no size
    expect(s.counts).toBe("4 块 NVMe");
  });

  it("draws the capacity bar only when it says more than the volume's own bar", () => {
    const vols = [volume("C:\\", 30, 100), volume("D:\\", 10, 100)];
    const one = disk({ size: 1000, parts: [{ mount_point: "C:\\", bytes: 990 }] });
    expect(showMap(diskMap(one, vols))).toBe(false);
    const two = disk({ size: 1000, parts: [{ mount_point: "C:\\", bytes: 600 }, { mount_point: "D:\\", bytes: 300 }] });
    const map = diskMap(two, vols);
    expect(map.map((s) => [s.mount, s.share, s.used])).toEqual([
      ["C:\\", 0.6, 0.3],
      ["D:\\", 0.3, 0.1],
    ]);
    expect(showMap(map)).toBe(true);
    const half = disk({ size: 1000, parts: [{ mount_point: "C:\\", bytes: 500 }] }); // another partition beside it
    expect(showMap(diskMap(half, vols))).toBe(true);
    // APFS: the volumes share what is left
    const shared = diskMap(disk({ size: 1000, parts: [{ mount_point: "C:\\", bytes: null }, { mount_point: "D:\\", bytes: null }] }), vols);
    expect(shared.map((s) => s.share)).toEqual([0.5, 0.5]);
    expect(diskMap(disk({ size: null, parts: [{ mount_point: "C:\\", bytes: null }] }), vols)).toEqual([]);
  });

  it("splits the volumes into those on each disk and the others", () => {
    const d = disk({ parts: [{ mount_point: "D:\\", bytes: 1 }, { mount_point: "C:\\", bytes: 1 }] });
    const data: MachineDisks = { collected_at: "2026-10-21T00:00:00Z", disks: [d], volumes: [volume("C:\\"), volume("D:\\"), volume("Z:\\")] };
    expect(volumesOf(data, d).map((v) => v.volume.mount_point)).toEqual(["D:\\", "C:\\"]);
    expect(elsewhere(data).map((v) => v.volume.mount_point)).toEqual(["Z:\\"]);
  });
});

describe("the dashboard", () => {
  const nvme = disk({ id: "d3", number: 3, size: 4000, system: true, link: link({ pcie_gen: 4, pcie_width: 4, max_pcie_gen: 5, max_pcie_width: 4 }), parts: [{ mount_point: "C:\\", bytes: 4000 }] });
  const sata = disk({
    id: "d2",
    number: 2,
    size: 2000,
    media: "sata_ssd",
    interface: "sata",
    link: link({ kind: "sata", rate_mbps: 6000, max_rate_mbps: 6000 }),
    controller_link: link({ pcie_gen: 2, pcie_width: 1 }),
    parts: [{ mount_point: "H:\\", bytes: 2000 }],
  });
  const reader = disk({ id: "d8", number: 8, size: 500, media: "usb_flash", interface: "usb", trim: null, link: link({ kind: "usb", rate_mbps: 480, max_rate_mbps: 5000 }), parts: [{ mount_point: "E:\\", bytes: 500 }] });
  const slot = disk({ id: "d9", number: 9, size: null, media: "usb_flash", interface: "usb", parts: [] });
  const on = (v: VolumeOverview, ids: string[]): VolumeOverview => ({ ...v, disks: ids });
  const data: MachineDisks = {
    collected_at: "2026-10-21T00:00:00Z",
    disks: [nvme, sata, reader, slot],
    volumes: [on(volume("C:\\", 2700, 4000), ["d3"]), on(volume("H:\\", 1900, 2000), ["d2"]), on(volume("E:\\", 400, 500), ["d8"]), volume("Z:\\", 10, 100)],
  };

  it("sums disks, capacity and room", () => {
    expect(totals(data)).toEqual({ disks: 3, capacity: 6500, volumes: 4, known: 0, used: 5000, free: 1500 });
  });

  it("splits the capacity by media, fastest first, one hue brighter for faster", () => {
    const m = byMedia(data);
    expect(m.map((x) => [x.tier, x.bytes, x.disks])).toEqual([
      ["nvme", 4000, 1],
      ["sata", 2000, 1],
      ["external", 500, 1],
    ]);
    expect(m[0]!.color).toBe("var(--sk-visual-branch-1-1)");
  });

  it("finds the fastest link and the fullest volume", () => {
    expect(fastest(data)?.disk.id).toBe("d3");
    expect(fastest(data)?.mbps).toBe(7876);
    expect(fullest(data)?.v.volume.mount_point).toBe("H:\\");
  });

  it("measures links against what the disk supports", () => {
    expect(linkShare(nvme.link)).toBeCloseTo(7876 / 15752);
    expect(linkShare(sata.link)).toBe(1);
    expect(linkShare(reader.link)).toBeCloseTo(48 / 500);
    expect(linkShare(link({ pcie_gen: 4, pcie_width: 4 }))).toBeNull(); // the most is not known
    expect(linkMBps(link({ kind: "usb", rate_mbps: 5000 }))).toBe(500);
  });

  it("sums what is used on a disk", () => {
    expect(diskUsed(nvme, data)).toBe(2700);
    expect(diskUsed(slot, data)).toBeNull();
    const spread = disk({ parts: [{ mount_point: "S:\\", bytes: 500 }] });
    const both: MachineDisks = { ...data, volumes: [on(volume("S:\\", 600, 1000), ["a", "b"])] };
    expect(diskUsed(spread, both)).toBe(300); // half the volume is here
  });

  it("lists findings, the ones to act on first", () => {
    const f = findings(data);
    expect(f.map((x) => x.level)).toEqual(["warn", "info", "info", "info", "info"]);
    expect(f[0]!.text).toContain("H:\\");
    expect(f.some((x) => x.text.includes("PCIe 5.0 ×4"))).toBe(true);
    expect(f.some((x) => x.text.includes("480 Mb/s"))).toBe(true);
    expect(f.some((x) => x.text.includes("E:\\") && x.text.includes("80%"))).toBe(true);
    expect(f.some((x) => x.text.includes("PCIe 2.0 ×1"))).toBe(true);
  });

  it("tells growth between the two newest snapshots, and only a large one is a finding", () => {
    const history = [
      { at: "2026-10-20T00:00:00Z", bytes_alloc: 900 * 2 ** 30, files: 10 },
      { at: "2026-10-17T00:00:00Z", bytes_alloc: 880 * 2 ** 30, files: 9 },
    ];
    expect(growth(history)).toEqual({ delta: 20 * 2 ** 30, since: "2026-10-17T00:00:00Z" });
    expect(growth(history.slice(0, 1))).toBeNull();
    const big = { ...on(volume("D:\\", 500 * 2 ** 30, 1000 * 2 ** 30), ["d0"]), history };
    const small = { ...big, history: [history[0]!, { ...history[1]!, bytes_alloc: 899 * 2 ** 30 }] };
    const now = Date.parse("2026-10-21T00:00:00Z") / 1000;
    expect(findings({ ...data, disks: [], volumes: [big] }, now).map((x) => x.text)).toEqual([expect.stringContaining("20 GB")]);
    expect(findings({ ...data, disks: [], volumes: [small] }, now)).toEqual([]);
  });
});

describe("what a disk records about itself", () => {
  const record = (patch: Partial<DiskHealth> = {}): DiskHealth => ({
    source: "nvme",
    temperature_c: 47,
    power_on_hours: 8070,
    power_cycles: 686,
    read_bytes: 105_919_728_640_000,
    written_bytes: 252_478_183_936_000,
    percentage_used: 9,
    available_spare: 100,
    spare_threshold: 10,
    unsafe_shutdowns: 106,
    media_errors: 0,
    critical_warning: 0,
    read_at: null,
    ...patch,
  });

  it("finds nothing in a healthy record", () => {
    expect(healthFindings(disk({ health: record() }))).toEqual([]);
    expect(healthFindings(disk())).toEqual([]);
  });

  it("warns of a critical warning, media errors, worn-out endurance and spare running out", () => {
    const f = healthFindings(disk({ model: "X", health: record({ critical_warning: 2, media_errors: 3, percentage_used: 95, available_spare: 8, temperature_c: 72 }) }));
    expect(f.map((x) => x.level)).toEqual(["warn", "warn", "warn", "warn", "info"]);
    expect(f[1]!.text).toContain("3");
    expect(f[2]!.text).toContain("95%");
    expect(healthFindings(disk({ health: record({ percentage_used: 75 }) }))[0]!.level).toBe("info");
  });

  it("goes into the page's findings", () => {
    const data: MachineDisks = { collected_at: "2026-10-21T00:00:00Z", volumes: [], disks: [disk({ health: record({ media_errors: 1 }) })] };
    expect(findings(data).map((x) => x.level)).toEqual(["warn"]);
  });

  it("says why a part of the record shows a dash", () => {
    expect(recordGap(disk({ health_note: "usb" }))).toBe(healthNoteText("usb"));
    expect(recordGap(disk({ health: record() }))).not.toBe(recordGap(disk()));  // the record lacks it / there is none
    expect(recordGap(disk())).not.toBe("");
  });

  it("says why there is no record, and power-on time in hours or days", () => {
    expect(healthNoteText("")).toBe("");
    for (const note of ["usb", "unsupported", "needs_admin", "unavailable"] as const) expect(healthNoteText(note)).not.toBe("");
    expect(powerOnText(30)).toBe("30 小时");
    expect(powerOnText(8070)).toBe("336 天");
  });
});

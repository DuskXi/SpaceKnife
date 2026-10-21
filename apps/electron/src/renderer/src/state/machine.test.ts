import { describe, expect, it } from "vitest";

import type { DiskLink, MachineDisks, PhysicalDisk, VolumeInfo, VolumeOverview } from "@/protocol/generated";

import { bandwidthText, diskMap, diskName, elsewhere, linkBandwidth, linkNotes, linkText, orderedDisks, pcieMBps, rateText, showMap, summary, volumesOf } from "./machine";

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

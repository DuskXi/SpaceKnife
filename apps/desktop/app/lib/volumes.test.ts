// The start page's volume words: names, the MFT check, the method badge, roots and the recents list.

import { afterEach, describe, expect, it, vi } from "vitest";

import type { SnapshotInfo, VolumeInfo } from "@/protocol/generated";

import { fmtWhen, LOAD_ZH, MEDIA_ZH, methodBadge, mftCapable, recentRecords, sameRoot, splitPaths, volumeName } from "./volumes";

const platform = (p: string) => vi.stubGlobal("window", { sk: { platform: p } });
afterEach(() => vi.unstubAllGlobals());

function vol(p: Partial<VolumeInfo>): VolumeInfo {
  return {
    cluster_size: 4096,
    device: "",
    free: 0,
    fs_type: "NTFS",
    is_remote: false,
    is_removable: false,
    is_system: false,
    label: "",
    media: "nvme_ssd",
    mount_point: "C:\\",
    note: "",
    reason: "",
    remote_path: "",
    reserved: 0,
    serial: "",
    total: 100,
    used: 50,
    ...p,
  } as VolumeInfo;
}
const snap = (path: string, roots: string[]): SnapshotInfo => ({ path, roots, bytes_alloc: 1, created_at: "2026-01-01T00:00:00Z", file_bytes: 1, files: 1, method: "hard", node_count: 1 });

describe("volumes", () => {
  it("names a volume by its mount point and label", () => {
    expect(volumeName(vol({ mount_point: "C:\\", label: "系统" }))).toBe("C: 系统");
    expect(volumeName(vol({ mount_point: "/home/x/", label: "x" }))).toBe("/home/x");
    expect(volumeName(vol({ mount_point: "/" }))).toBe("/");
  });

  it("reads a whole local NTFS volume with the MFT only on Windows", () => {
    platform("win32");
    expect(mftCapable(vol({}))).toBe(true);
    expect(mftCapable(vol({ fs_type: "ReFS" }))).toBe(false);
    expect(mftCapable(vol({ is_remote: true }))).toBe(false);
    expect(mftCapable(vol({ reason: "locked" }))).toBe(false);
    expect(mftCapable(null)).toBe(false);
    platform("linux");
    expect(mftCapable(vol({}))).toBe(false);
  });

  it("says what a volume is scanned with", () => {
    platform("win32");
    expect(methodBadge(vol({ reason: "busy" }), "auto", false)).toEqual({ icon: "warning", text: "busy" });
    expect(methodBadge(vol({}), "auto", false).text).toBe("MFT 快速扫描 · 需要管理员");
    expect(methodBadge(vol({}), "auto", true).text).toBe("MFT 快速扫描");
    expect(methodBadge(vol({}), "hard", true).text).toBe("并行硬扫描");
    expect(methodBadge(vol({ fs_type: "exFAT", media: "usb_hdd" }), "auto", true).text).toBe("并行硬扫描 · USB HDD 自动降速");
  });

  it("matches roots without the trailing separator, ignoring case on Windows", () => {
    platform("win32");
    expect(sameRoot("C:\\", "c:")).toBe(true);
    platform("linux");
    expect(sameRoot("/", "/")).toBe(true);
    expect(sameRoot("/Data/", "/data")).toBe(false);
  });

  it("splits typed paths and translates the labels when read", () => {
    expect(splitPaths(" C:\\a ; ;D:\\b")).toEqual(["C:\\a", "D:\\b"]);
    expect(MEDIA_ZH.hdd).toBe("机械硬盘");
    expect(LOAD_ZH.balanced[0]).toBe("均衡 70%");
  });

  it("dates a snapshot relative to today", () => {
    const now = new Date(2026, 4, 10, 12, 0);
    expect(fmtWhen(new Date(2026, 4, 10, 9, 5).toISOString(), now)).toBe("今天 09:05");
    expect(fmtWhen(new Date(2026, 4, 9, 23, 0).toISOString(), now)).toBe("昨天 23:00");
    expect(fmtWhen(new Date(2026, 1, 3).toISOString(), now)).toBe("02-03");
    expect(fmtWhen(new Date(2025, 1, 3).toISOString(), now)).toBe("2025-02-03");
  });

  it("keeps the newest snapshot per target unless all are shown", () => {
    platform("win32");
    const snaps = [snap("a", ["C:\\"]), snap("b", ["c:\\"]), snap("c", ["D:\\x", "D:\\y"]), snap("d", ["E:\\Games\\"])];
    const volumes = [vol({ mount_point: "C:\\", label: "系统" })];
    expect(recentRecords(snaps, volumes, false).map((r) => [r.key, r.name, r.volume])).toEqual([
      ["a", "C: 系统", true],
      ["c", "2 个位置", false],
      ["d", "Games", false],
    ]);
    expect(recentRecords(snaps, volumes, true)).toHaveLength(4);
  });
});

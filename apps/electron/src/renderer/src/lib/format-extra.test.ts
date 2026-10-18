import { afterEach, describe, expect, it } from "vitest";

import { fmtRate, fmtRemaining, scanPercent } from "@/shell/ScanProgress";

import { fmtSizeAs, setUnitBase } from "./format";

const GiB = 1024 ** 3;
const TiB = 1024 ** 4;

describe("fmtSizeAs (one unit per volume card, UI-W1-17)", () => {
  afterEach(() => setUnitBase(1024));

  it("uses the unit of the reference size", () => {
    expect(fmtSizeAs(1.21 * TiB, 1.82 * TiB)).toBe("1.21 TB");
    expect(fmtSizeAs(0.61 * TiB, 1.82 * TiB)).toBe("0.61 TB"); // not "624.6 GB"
    expect(fmtSizeAs(182.4 * GiB, 961 * GiB)).toBe("182 GB");
    expect(fmtSizeAs(12.34 * GiB, 961 * GiB)).toBe("12.3 GB");
  });

  it("follows the unit base", () => {
    setUnitBase(1000);
    expect(fmtSizeAs(500e9, 2e12)).toBe("0.50 TB");
  });

  it("falls back to bytes for tiny references", () => {
    expect(fmtSizeAs(10, 512)).toBe("10 B");
  });
});

describe("W2 band figures", () => {
  it("rounds the percentage and keeps it below 100 while scanning (UI-W2-10)", () => {
    expect(scanPercent(412.6 / 778.6)).toBe(53);
    expect(scanPercent(0.996)).toBe(99);
    expect(scanPercent(0)).toBe(0);
  });

  it("rounds the remaining time before splitting it (UI-W2-15)", () => {
    expect(fmtRemaining(14.2)).toBe("约 14 秒");
    expect(fmtRemaining(0.2)).toBe("约 1 秒");
    expect(fmtRemaining(59.6)).toBe("约 1 分钟");
    expect(fmtRemaining(3590)).toBe("约 1 小时");
    expect(fmtRemaining(7190)).toBe("约 2 小时");
    expect(fmtRemaining(5400)).toBe("约 1 小时 30 分");
  });

  it("shows slow rates exactly and fast ones in hundreds (UI-W2-14)", () => {
    expect(fmtRate(41_234)).toBe("41,200");
    expect(fmtRate(40.4)).toBe("40");
    expect(fmtRate(999.6)).toBe("1,000");
  });
});

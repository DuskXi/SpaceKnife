import { describe, expect, it } from "vitest";

import type { IoStatus, ScanStatus } from "@/protocol/generated";

import { curveSummary, fmtMs, loadCurve, targetName } from "./scanProgress";

const io = (patch: Partial<IoStatus> = {}): IoStatus =>
  ({
    level: "balanced",
    limit: 70,
    history: [
      { t: 100, self_pct: 0, total_pct: 40, active_pct: 50 },
      { t: 130, self_pct: 30, total_pct: 60, active_pct: 90 },
      { t: 160, self_pct: 50, total_pct: 80, active_pct: 100 },
    ],
    ...patch,
  }) as IoStatus;

describe("scan progress helpers", () => {
  it("summarizes the load curve before and after the start", () => {
    expect(curveSummary(io(), 120, false)).toBe("近 60 秒：扫描开始前整盘读取占用约 40%；扫描开始后 SpaceKnife 占用约 40%，整盘约 70%，均低于上限 70%");
    expect(curveSummary(io({ level: "max" }), null, true)).toBe("近 60 秒：扫描已暂停，整盘约 60%");
    expect(curveSummary(io({ limit: 20 }), null, false)).toBe("近 60 秒：扫描开始后 SpaceKnife 占用约 27%，整盘约 60%，上限 20%");
  });

  it("draws the last 60 seconds in a 300 × 96 box", () => {
    const c = loadCurve(io(), new Date(130_000).toISOString());
    expect(c.x(160)).toBe(300);
    expect(c.x(100)).toBe(0);
    expect(c.y(100)).toBe(c.PAD_T);
    expect(c.y(0)).toBe(c.H - c.PAD_B);
    expect(c.y(150)).toBe(c.PAD_T); // clamped
    expect(c.own.startsWith("M 0.0 ")).toBe(true);
    expect(c.total.endsWith("Z")).toBe(true);
    expect(c.limited).toBe(true);
    expect(c.start).toBe(130);
    expect(c.showStart).toBe(true);
    expect(c.last?.t).toBe(160);
    expect(loadCurve(io({ level: "idle" }), null).limited).toBe(false);
    expect(loadCurve(io({ history: [] }), null).total).toBe("");
  });

  it("names the target and formats latencies", () => {
    const scan = (path: string, whole: boolean) => ({ roots: [{ path, whole_volume: whole }] }) as unknown as ScanStatus;
    expect(targetName(scan("C:\\", true), "x")).toBe("C:");
    expect(targetName(scan("C:\\Users\\me\\", false), "x")).toBe("me");
    expect(targetName({ roots: [] } as unknown as ScanStatus, "title")).toBe("title");
    expect([fmtMs(0.123), fmtMs(4.56), fmtMs(12.4)]).toEqual(["0.12", "4.6", "12"]);
  });
});

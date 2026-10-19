import { describe, expect, it } from "vitest";

import { Flag } from "./frame";
import { breathAlpha, breathingBlocks } from "./scanning";

function frameOf(rows: { flags: number; depth: number; w: number; h: number }[]) {
  return {
    count: rows.length,
    flags: Uint16Array.from(rows.map((r) => r.flags)),
    depth: Uint8Array.from(rows.map((r) => r.depth)),
    x: Int32Array.from(rows.map((_, i) => i * 10)),
    y: new Int32Array(rows.length),
    w: Int32Array.from(rows.map((r) => r.w)),
    h: Int32Array.from(rows.map((r) => r.h)),
  };
}

describe("folders being scanned", () => {
  it("keeps the marked blocks big enough for an outline", () => {
    const f = frameOf([
      { flags: Flag.SCANNING, depth: 1, w: 400, h: 300 },
      { flags: Flag.SCANNING | Flag.SELECTED, depth: 2, w: 100, h: 80 },
      { flags: Flag.SELECTED, depth: 1, w: 400, h: 300 }, // not being scanned
      { flags: Flag.SCANNING, depth: 3, w: 20, h: 8 }, // too thin at dpr 1
    ]);
    expect(breathingBlocks(f, 1).map((b) => b.depth)).toEqual([1, 2]);
    expect(breathingBlocks(f, 2).map((b) => b.depth)).toEqual([1, 2]);
  });

  it("breathes, outer levels brighter, and holds still under reduced motion", () => {
    const samples = Array.from({ length: 48 }, (_, i) => breathAlpha(1, i * 50, false));
    expect(Math.max(...samples) - Math.min(...samples)).toBeGreaterThan(0.5);
    expect(breathAlpha(1, 1000, true)).toBe(breathAlpha(1, 2000, true));
    expect(breathAlpha(1, 0, true)).toBeGreaterThan(breathAlpha(2, 0, true));
    expect(breathAlpha(2, 0, true)).toBeGreaterThan(breathAlpha(3, 0, true));
    for (const a of samples) expect(a).toBeGreaterThanOrEqual(0), expect(a).toBeLessThanOrEqual(1);
  });
});

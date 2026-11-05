import { describe, expect, it } from "vitest";

import { summaryInterval } from "./throttle";

describe("summaryInterval", () => {
  it("refreshes small subtrees at once, once a second while scanning", () => {
    expect(summaryInterval(0, false)).toBe(0);
    expect(summaryInterval(150_000, false)).toBe(0);
    expect(summaryInterval(150_000, true)).toBe(1000);
  });

  it("waits longer for larger subtrees, up to 10 s", () => {
    expect(summaryInterval(200_000, false)).toBe(1000);
    expect(summaryInterval(1_000_000, false)).toBe(1500);
    expect(summaryInterval(3_200_000, false)).toBe(4800);
    expect(summaryInterval(3_200_000, true)).toBe(4800);
    expect(summaryInterval(8_600_000, false)).toBe(10_000);
    expect(summaryInterval(100_000_000, true)).toBe(10_000);
  });

  it("grows with the size", () => {
    let last = -1;
    for (let n = 0; n <= 20_000_000; n += 250_000) {
      const ms = summaryInterval(n, false);
      expect(ms).toBeGreaterThanOrEqual(last);
      last = ms;
    }
  });

  it("treats an unknown size as small", () => {
    expect(summaryInterval(Number.NaN, false)).toBe(0);
  });
});

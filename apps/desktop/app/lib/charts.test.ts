import { describe, expect, it } from "vitest";

import { clampShare, ringArc, sparkPoints } from "./charts";

describe("charts", () => {
  it("keeps shares to 0-1", () => {
    expect(clampShare(-0.5)).toBe(0);
    expect(clampShare(0.25)).toBe(0.25);
    expect(clampShare(3)).toBe(1);
  });

  it("shortens the ring's arc for its round caps, unless it is whole", () => {
    const whole = ringArc(1, 44, 5);
    const c = 2 * Math.PI * whole.r;
    expect(whole.r).toBe(19.5);
    expect(whole.dash).toBe(`${c} ${c}`);
    const half = ringArc(0.5, 44, 5);
    expect(Number(half.dash.split(" ")[0])).toBeCloseTo(c / 2 - 3);
    // a sliver never goes negative
    expect(ringArc(0.001, 44, 5).dash.startsWith("0.01 ")).toBe(true);
  });

  it("scales a sparkline to its own range and marks the last point", () => {
    expect(sparkPoints([5], 96, 26)).toBeNull();
    const line = sparkPoints([0, 10], 96, 26)!;
    expect(line.points).toBe("3.0,23.0 93.0,3.0");
    expect(line.last).toEqual({ x: 93, y: 3 });
    // flat: the middle line
    expect(sparkPoints([4, 4, 4], 96, 26)!.points).toBe("3.0,13.0 48.0,13.0 93.0,13.0");
  });
});

import { describe, expect, it } from "vitest";

import type { CompareSourcesResult } from "@/protocol/generated";
import { NOW } from "@/state/compare";

import { directionLabel, DIRECTIONS, endLabel, fmtSpan, sideName, stamp, timeline } from "./compare";

const iso = (y: number, m: number, d: number, h = 0, min = 0) => new Date(y, m - 1, d, h, min).toISOString();

describe("compare helpers", () => {
  it("names the directions", () => {
    expect(DIRECTIONS.map(directionLabel)).toEqual(["全部", "增长", "减少"]);
  });

  it("says a span in its two largest units", () => {
    expect(fmtSpan(6 * 86400 + 12 * 3600 + 59)).toBe("6 天 12 小时");
    expect(fmtSpan(2 * 86400)).toBe("2 天");
    expect(fmtSpan(-(3 * 3600 + 20 * 60))).toBe("3 小时 20 分钟");
    expect(fmtSpan(3600)).toBe("1 小时");
    // never less than a minute
    expect(fmtSpan(5)).toBe("1 分钟");
  });

  it("labels the ends", () => {
    const at = iso(2025, 3, 4, 9, 5);
    const sources = { now: null, snapshots: [{ path: "C:\\snaps\\a.sksnap", created_at: at, bytes_alloc: 1 }] } as unknown as CompareSourcesResult;
    expect(stamp(at)).toBe("2025-03-04 09:05");
    expect(endLabel(NOW, sources)).toBe("现在");
    expect(endLabel("C:\\snaps\\a.sksnap", sources)).toBe("2025-03-04 09:05 快照");
    // a file the list does not know: its name
    expect(endLabel("D:/other/b.sksnap", sources)).toBe("b.sksnap");
    expect(sideName({ kind: "now" } as never)).toBe("现在");
  });

  it("lays the snapshots out on a time line and labels the ends first", () => {
    const sources = {
      now: { created_at: iso(2025, 1, 11), bytes_alloc: 1 },
      snapshots: [
        { path: "a", created_at: iso(2025, 1, 1), bytes_alloc: 1 },
        { path: "b", created_at: iso(2025, 1, 1, 12), bytes_alloc: 1 },
        { path: "c", created_at: iso(2025, 1, 6), bytes_alloc: 1 },
      ],
    } as unknown as CompareSourcesResult;
    const line = timeline(sources, "b", NOW);
    expect(line.points.map((p) => p.spec)).toEqual(["a", "b", "c", NOW]);
    expect(line.pos(iso(2025, 1, 1))).toBe(0);
    expect(line.pos(iso(2025, 1, 6))).toBe(50);
    expect(line.lo).toBe(5);
    expect(line.hi).toBe(100);
    // a is 5 % from the base b: unlabelled; the base keeps its label
    expect([...line.labelled].sort()).toEqual(["b", "c", NOW].sort());
  });

  it("has no span without both ends", () => {
    const line = timeline(null, "x", "y");
    expect(line.points).toEqual([]);
    expect(line.hi - line.lo).toBe(0);
  });
});

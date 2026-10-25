import { describe, expect, it } from "vitest";

import type { VisualInfo } from "@/protocol/generated";
import { frameInfo } from "@/visual/testing/fixtures";

import { COLOR_MODES, levelsThatFit, nextColor, nextVisual, offered, visualIcon } from "./visuals";

const visual = (id: string, order: number, patch: Partial<VisualInfo> = {}): VisualInfo => ({
  id,
  layout: { treemap: "drawlist.v1", flame: "flame.v1", flame_polar: "polar.v1" }[id] ?? `${id}.v1`,
  name: id,
  icon: "vizTreemap",
  order,
  compare: true,
  free_space: true,
  default_color: "class",
  available: true,
  reason: null,
  plugin: null,
  size_unit: "bytes",
  when: [],
  ...patch,
});

const BUILTIN = [visual("treemap", 0), visual("flame", 1), visual("flame_polar", 2)];
const ALL_LAYOUTS = ["drawlist.v1", "flame.v1", "polar.v1"];

describe("offered", () => {
  it("keeps the backend's order and enables what this renderer can draw", () => {
    const items = offered([BUILTIN[2]!, BUILTIN[0]!, BUILTIN[1]!], ALL_LAYOUTS, false);
    expect(items.map((o) => o.info.id)).toEqual(["treemap", "flame", "flame_polar"]);
    expect(items.every((o) => o.enabled && o.reason === null)).toBe(true);
  });

  it("disables a layout with no local component instead of hiding it", () => {
    const items = offered(BUILTIN, ["drawlist.v1", "flame.v1"], false);
    expect(items.map((o) => o.enabled)).toEqual([true, true, false]);
    expect(items[2]!.reason).toBe("这个界面画不了这种可视化");
  });

  it("gives the backend's reason for one the session doesn't have", () => {
    const list = [BUILTIN[0]!, visual("flame", 1, { available: false, reason: "后端说不行" }), visual("flame_polar", 2, { available: false })];
    const items = offered(list, ALL_LAYOUTS, false);
    expect(items.map((o) => o.reason)).toEqual([null, "后端说不行", "这个界面画不了这种可视化"]);
  });

  it("leaves out of a comparison what can't show changes", () => {
    const list = [BUILTIN[0]!, visual("flame", 1, { compare: false })];
    expect(offered(list, ALL_LAYOUTS, false).map((o) => o.enabled)).toEqual([true, true]);
    const cmp = offered(list, ALL_LAYOUTS, true);
    expect(cmp.map((o) => o.enabled)).toEqual([true, false]);
    expect(cmp[1]!.reason).toBe("对比视图里不能用这种可视化");
  });

  it("is empty when the backend offers nothing", () => {
    expect(offered([], ALL_LAYOUTS, false)).toEqual([]);
  });
});

describe("nextVisual", () => {
  it("steps forward and wraps around", () => {
    expect(nextVisual(BUILTIN, ALL_LAYOUTS, "treemap", 1, false)).toBe("flame");
    expect(nextVisual(BUILTIN, ALL_LAYOUTS, "flame", 1, false)).toBe("flame_polar");
    expect(nextVisual(BUILTIN, ALL_LAYOUTS, "flame_polar", 1, false)).toBe("treemap");
  });

  it("steps backward and wraps around", () => {
    expect(nextVisual(BUILTIN, ALL_LAYOUTS, "treemap", -1, false)).toBe("flame_polar");
    expect(nextVisual(BUILTIN, ALL_LAYOUTS, "flame_polar", -1, false)).toBe("flame");
    expect(nextVisual(BUILTIN, ALL_LAYOUTS, "flame", -1, false)).toBe("treemap");
  });

  it("skips disabled ones", () => {
    expect(nextVisual(BUILTIN, ["drawlist.v1", "polar.v1"], "treemap", 1, false)).toBe("flame_polar");
    expect(nextVisual(BUILTIN, ["drawlist.v1", "polar.v1"], "treemap", -1, false)).toBe("flame_polar");
    const list = [BUILTIN[0]!, visual("flame", 1, { compare: false }), BUILTIN[2]!];
    expect(nextVisual(list, ALL_LAYOUTS, "treemap", 1, true)).toBe("flame_polar");
    expect(nextVisual(list, ALL_LAYOUTS, "treemap", 1, false)).toBe("flame");
  });

  it("has nowhere to go with a single one", () => {
    expect(nextVisual([BUILTIN[0]!], ALL_LAYOUTS, "treemap", 1, false)).toBeNull();
    expect(nextVisual([BUILTIN[0]!], ALL_LAYOUTS, "treemap", -1, false)).toBeNull();
    expect(nextVisual(BUILTIN, ["drawlist.v1"], "treemap", 1, false)).toBeNull();
    expect(nextVisual([], ALL_LAYOUTS, "treemap", 1, false)).toBeNull();
  });

  it("leaves a disabled or unknown current one for an enabled one", () => {
    expect(nextVisual(BUILTIN, ["drawlist.v1"], "flame_polar", 1, false)).toBe("treemap");
    expect(nextVisual(BUILTIN, ALL_LAYOUTS, "gone", 1, false)).toBe("treemap");
    expect(nextVisual(BUILTIN, ALL_LAYOUTS, "gone", -1, false)).toBe("flame_polar");
  });
});

describe("nextColor", () => {
  it("cycles through the four colour modes", () => {
    expect(COLOR_MODES).toEqual(["class", "flat", "time", "branch"]);
    expect(nextColor("class")).toBe("flat");
    expect(nextColor("flat")).toBe("time");
    expect(nextColor("time")).toBe("branch");
    expect(nextColor("branch")).toBe("class");
  });
});

describe("levelsThatFit", () => {
  const flame = (rows: number) => frameInfo({ layout: "flame.v1", visual: "flame", geometry: { kind: "flame", origin: "bottom", row_h: 33, rows, gap: 2, root: { x: 0, y: 167, w: 400, h: 31 } } });
  const polar = (rings: number) => frameInfo({ layout: "polar.v1", visual: "flame_polar", geometry: { kind: "polar", cx: 200, cy: 200, radius: 192, hole: 46, ring_w: 29, rings, gap: 2, unit: 1 << 20 } });

  it("reports the levels a flame graph or polar flame graph has room for when fewer than asked", () => {
    expect(levelsThatFit(flame(3), 5)).toBe(3);
    expect(levelsThatFit(polar(4), 8)).toBe(4);
    expect(levelsThatFit(flame(0), 5)).toBe(0);
  });

  it("says nothing when every level fits or the layout has no limit", () => {
    expect(levelsThatFit(flame(5), 5)).toBeNull();
    expect(levelsThatFit(polar(9), 8)).toBeNull();
    expect(levelsThatFit(frameInfo(), 5)).toBeNull();
    expect(levelsThatFit(null, 5)).toBeNull();
  });
});

describe("visualIcon", () => {
  it("takes the camelCase icon keys and the kebab-case file names", () => {
    expect(visualIcon("vizTreemap")).toBe("vizTreemap");
    expect(visualIcon("vizFlamePolar")).toBe("vizFlamePolar");
    expect(visualIcon("viz-flame-polar")).toBe("vizFlamePolar");
    expect(visualIcon("viz-flame")).toBe("vizFlame");
  });

  it("falls back for an icon the set does not have", () => {
    expect(visualIcon("viz-nope")).toBe("layers");
    expect(visualIcon("")).toBe("layers");
  });
});

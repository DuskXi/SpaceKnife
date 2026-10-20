import { beforeEach, describe, expect, it, vi } from "vitest";

import { VISUAL_API, type VisualComponent } from "./types";

// a fresh registry per test: it is module state
let reg: typeof import("./registry");
beforeEach(async () => {
  vi.resetModules();
  reg = await import("./registry");
});

const component = (layout: string, capability: string | null, api: number = VISUAL_API): VisualComponent =>
  ({
    layout,
    api,
    capability,
    hoverPlacement: "beside",
    geometry: {} as VisualComponent["geometry"],
    create: () => {
      throw new Error("not in this test");
    },
  }) as VisualComponent;

describe("registerVisual", () => {
  it("finds components by layout, in registration order", () => {
    const tm = component("drawlist.v1", null);
    const fl = component("flame.v1", "layout.flame.v1");
    reg.registerVisual(tm);
    reg.registerVisual(fl);
    expect(reg.componentFor("drawlist.v1")).toBe(tm);
    expect(reg.componentFor("flame.v1")).toBe(fl);
    expect(reg.componentFor("polar.v1")).toBeNull();
    expect(reg.registeredLayouts()).toEqual(["drawlist.v1", "flame.v1"]);
  });

  it("refuses a second component for a layout", () => {
    reg.registerVisual(component("polar.v1", "layout.polar.v1"));
    expect(() => reg.registerVisual(component("polar.v1", "layout.polar.v1"))).toThrow(/already registered/);
  });

  it("refuses a component built for another interface version", () => {
    expect(() => reg.registerVisual(component("flame.v1", "layout.flame.v1", VISUAL_API + 1))).toThrow(/api/);
    expect(reg.registeredLayouts()).toEqual([]);
  });

  it("refuses a layout no decoder knows", () => {
    expect(() => reg.registerVisual(component("flame.v2", "layout.flame.v2"))).toThrow(/no decoder/);
  });
});

describe("visualCapabilities", () => {
  it("is visuals.v1 alone before any layout beyond the baseline", () => {
    expect(reg.visualCapabilities()).toEqual(["visuals.v1"]);
    reg.registerVisual(component("drawlist.v1", null));
    expect(reg.visualCapabilities()).toEqual(["visuals.v1"]);
  });

  it("announces each registered layout's capability", () => {
    reg.registerVisual(component("drawlist.v1", null));
    reg.registerVisual(component("flame.v1", "layout.flame.v1"));
    reg.registerVisual(component("polar.v1", "layout.polar.v1"));
    expect(reg.visualCapabilities()).toEqual(["visuals.v1", "layout.flame.v1", "layout.polar.v1"]);
  });
});

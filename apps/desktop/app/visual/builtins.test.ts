import { describe, expect, it } from "vitest";

import "./builtins";
import { flameComponent } from "./flame/component";
import { flamePolarComponent } from "./flamePolar/component";
import { componentFor, registeredLayouts, visualCapabilities } from "./registry";
import { treemapComponent } from "./treemap/component";

describe("the built-in visualizations", () => {
  it("register the treemap, the flame graph and the polar flame graph, in that order", () => {
    expect(registeredLayouts()).toEqual(["drawlist.v1", "flame.v1", "polar.v1"]);
    expect(componentFor("drawlist.v1")).toBe(treemapComponent);
    expect(componentFor("flame.v1")).toBe(flameComponent);
    expect(componentFor("polar.v1")).toBe(flamePolarComponent);
  });

  it("announce visuals.v1 and both partition layouts in hello (the backend's capability names)", () => {
    expect(visualCapabilities()).toEqual(["visuals.v1", "layout.flame.v1", "layout.polar.v1"]);
  });
});

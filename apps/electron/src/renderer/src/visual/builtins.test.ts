import { describe, expect, it } from "vitest";

import "./builtins";
import { flameComponent } from "./flame/component";
import { flamePolarComponent } from "./flamePolar/component";
import { componentFor, registeredLayouts, visualCapabilities } from "./registry";
import { shapesComponent } from "./shapes/component";
import { treemapComponent } from "./treemap/component";

describe("the built-in visualizations", () => {
  it("register the treemap, the flame graph, the polar flame graph and the plugins' shapes, in that order", () => {
    expect(registeredLayouts()).toEqual(["drawlist.v1", "flame.v1", "polar.v1", "shapes.v1"]);
    expect(componentFor("shapes.v1")).toBe(shapesComponent);
    expect(componentFor("drawlist.v1")).toBe(treemapComponent);
    expect(componentFor("flame.v1")).toBe(flameComponent);
    expect(componentFor("polar.v1")).toBe(flamePolarComponent);
  });

  it("announce visuals.v1, both partition layouts and the shapes in hello (the backend's capability names)", () => {
    expect(visualCapabilities()).toEqual(["visuals.v1", "layout.flame.v1", "layout.polar.v1", "layout.shapes.v1"]);
  });
});

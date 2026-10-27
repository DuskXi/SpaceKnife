// checkGeometry against plain reference geometries on the fixture scenes, and against broken ones.

import { describe, expect, it } from "vitest";

import { Kind } from "../frame";
import { type Box, HIT_NONE, HIT_ROOT, type PolarFrame, type RectFrame, type VisualComponent, type VisualGeometry } from "../types";
import { checkGeometry } from "./conformance";
import { polarScene, rectScene } from "./fixtures";

// ------------------------------------------------------------------ reference geometries (brute force)

const rootBar = (f: RectFrame): Box | null => (f.info.root && f.info.geometry?.kind === "flame" ? f.info.geometry.root : null);

const rects: VisualGeometry<RectFrame> = {
  hitTest(f, px, py) {
    for (let i = f.count - 1; i >= 0; i--) if (px >= f.x[i]! && py >= f.y[i]! && px < f.x[i]! + f.w[i]! && py < f.y[i]! + f.h[i]!) return i;
    const r = rootBar(f);
    return r && px >= r.x && py >= r.y && px < r.x + r.w && py < r.y + r.h ? HIT_ROOT : HIT_NONE;
  },
  bounds(f, hit) {
    if (hit === HIT_ROOT) return rootBar(f);
    return hit >= 0 && hit < f.count ? { x: f.x[hit]!, y: f.y[hit]!, w: f.w[hit]!, h: f.h[hit]! } : null;
  },
  anchor(f, hit, slot) {
    const b = this.bounds(f, hit);
    if (!b) return null;
    if (slot === "label") return { x: b.x + b.w / 2, y: b.y + b.h / 2 };
    if (slot === "tag") return b.w >= 22 && b.h >= 18 ? { x: b.x + b.w - 18, y: b.y + 4 } : null;
    return b.w >= 70 && b.h >= 40 ? { x: b.x + 6, y: b.y + b.h - 21 } : null;
  },
  box(f, hit) {
    const b = this.bounds(f, hit);
    return b ? { major: Math.max(b.w, b.h), minor: Math.min(b.w, b.h) } : { major: 0, minor: 0 };
  },
  path(f, hit, inset, out) {
    const b = this.bounds(f, hit);
    if (!b) return false;
    out.rect(b.x + inset, b.y + inset, b.w - 2 * inset, b.h - 2 * inset);
    return true;
  },
  filled: (f, row) => f.kind[row] !== Kind.FOLDER,
};

const TURN = 2 * Math.PI;
const polarOf = (f: PolarFrame) => (f.info.geometry?.kind === "polar" ? f.info.geometry : null);
/** Canvas point at a radius and a wire angle (units from 12 o'clock, clockwise). */
const at = (f: PolarFrame, r: number, a: number) => {
  const g = polarOf(f)!;
  const th = (a / g.unit) * TURN;
  return { x: g.cx + r * Math.sin(th), y: g.cy - r * Math.cos(th) };
};

const sectors: VisualGeometry<PolarFrame> = {
  hitTest(f, px, py) {
    const g = polarOf(f);
    if (!g) return HIT_NONE;
    const r = Math.hypot(px - g.cx, py - g.cy);
    if (r < g.hole) return f.info.root ? HIT_ROOT : HIT_NONE;
    let th = Math.atan2(px - g.cx, g.cy - py);
    if (th < 0) th += TURN;
    const u = (th / TURN) * g.unit;
    for (let i = f.count - 1; i >= 0; i--) if (r >= f.r0[i]! && r < f.r1[i]! && u >= f.a0[i]! && u < f.a1[i]!) return i;
    return HIT_NONE;
  },
  bounds(f, hit) {
    const g = polarOf(f);
    if (!g) return null;
    if (hit === HIT_ROOT) return f.info.root ? { x: g.cx - g.hole, y: g.cy - g.hole, w: 2 * g.hole, h: 2 * g.hole } : null;
    if (hit < 0 || hit >= f.count) return null;
    const [a0, a1, r0, r1] = [f.a0[hit]!, f.a1[hit]!, f.r0[hit]!, f.r1[hit]!];
    const pts = [at(f, r0, a0), at(f, r1, a0), at(f, r0, a1), at(f, r1, a1)];
    for (let q = 0; q <= 4; q++) if ((q * g.unit) / 4 > a0 && (q * g.unit) / 4 < a1) pts.push(at(f, r1, (q * g.unit) / 4));
    const xs = pts.map((p) => p.x);
    const ys = pts.map((p) => p.y);
    const [x, y] = [Math.min(...xs), Math.min(...ys)];
    return { x, y, w: Math.max(...xs) - x, h: Math.max(...ys) - y };
  },
  anchor(f, hit, slot) {
    const g = polarOf(f);
    if (!g || slot !== "label") return null;
    if (hit === HIT_ROOT) return f.info.root ? { x: g.cx, y: g.cy } : null;
    if (hit < 0 || hit >= f.count) return null;
    return at(f, (f.r0[hit]! + f.r1[hit]!) / 2, (f.a0[hit]! + f.a1[hit]!) / 2);
  },
  box(f, hit) {
    const g = polarOf(f);
    if (!g) return { major: 0, minor: 0 };
    if (hit === HIT_ROOT) return f.info.root ? { major: 2 * g.hole, minor: 2 * g.hole } : { major: 0, minor: 0 };
    if (hit < 0 || hit >= f.count) return { major: 0, minor: 0 };
    const arc = ((f.a1[hit]! - f.a0[hit]!) / g.unit) * TURN * ((f.r0[hit]! + f.r1[hit]!) / 2);
    return { major: arc, minor: f.r1[hit]! - f.r0[hit]! };
  },
  path(f, hit, inset, out) {
    const g = polarOf(f);
    if (!g) return false;
    if (hit === HIT_ROOT) {
      if (!f.info.root) return false;
      out.arc(g.cx, g.cy, g.hole - inset, 0, TURN);
      return true;
    }
    if (hit < 0 || hit >= f.count) return false;
    const phi = (a: number) => (a / g.unit) * TURN - Math.PI / 2;
    out.arc(g.cx, g.cy, f.r1[hit]! - inset, phi(f.a0[hit]!), phi(f.a1[hit]!));
    out.arc(g.cx, g.cy, f.r0[hit]! + inset, phi(f.a1[hit]!), phi(f.a0[hit]!), true);
    out.closePath();
    return true;
  },
  filled: () => true,
};

const rectComponent = (layout: string): Pick<VisualComponent<RectFrame>, "layout" | "geometry"> => ({ layout, geometry: rects });
const polarComponent: Pick<VisualComponent<PolarFrame>, "layout" | "geometry"> = { layout: "polar.v1", geometry: sectors };

// ------------------------------------------------------------------ tests

describe("checkGeometry", () => {
  it("passes reference geometries on every fixture scene", () => {
    expect(checkGeometry(rectComponent("drawlist.v1"), rectScene().frame)).toEqual([]);
    expect(checkGeometry(rectComponent("flame.v1"), rectScene({ layout: "flame.v1" }).frame)).toEqual([]);
    expect(checkGeometry(rectComponent("flame.v1"), rectScene({ layout: "flame.v1", origin: "top", dpr: 2, width: 800, height: 400 }).frame)).toEqual([]);
    expect(checkGeometry(polarComponent, polarScene().frame)).toEqual([]);
    expect(checkGeometry(polarComponent, polarScene({ dpr: 1.5, width: 900, height: 600, detail: 15 }).frame)).toEqual([]);
  });

  it("checks the root bar and the centre disc", () => {
    const noRoot: VisualGeometry<PolarFrame> = { ...sectors, hitTest: (f, x, y) => Math.max(HIT_NONE, sectors.hitTest(f, x, y)) };
    expect(checkGeometry({ layout: "polar.v1", geometry: noRoot }, polarScene().frame)).toEqual([expect.stringMatching(/^root: hitTest at its label anchor .* gives -1$/)]);
  });

  it("reports a component for another layout", () => {
    expect(checkGeometry(rectComponent("flame.v1"), rectScene().frame)).toContain("frame layout drawlist.v1, component layout flame.v1");
  });

  it("reports hits outside the canvas", () => {
    const greedy: VisualGeometry<RectFrame> = { ...rects, hitTest: (f, x, y) => Math.max(0, rects.hitTest(f, x, y)) };
    const bad = checkGeometry({ layout: "drawlist.v1", geometry: greedy }, rectScene().frame);
    expect(bad).toContain("hitTest (-1.0, -1.0) outside the canvas gives 0");
    expect(bad).toContain("hitTest on an empty frame gives row 0");
  });

  it("reports a label anchor that lands on another row or outside the bounds", () => {
    const shifted: VisualGeometry<RectFrame> = {
      ...rects,
      anchor: (f, hit, slot) => {
        const p = rects.anchor(f, hit, slot);
        return p && slot === "label" && hit === 1 ? { x: p.x + 60, y: p.y } : p;
      },
    };
    const errors = checkGeometry({ layout: "drawlist.v1", geometry: shifted }, rectScene().frame);
    expect(errors).toEqual(["row 1: hitTest at its label anchor (112.0, 59.0) gives 3", "row 1: label anchor (112.0, 59.0) outside its bounds"]);
  });

  it("reports missing anchors, bounds and outlines on rows big enough for them", () => {
    const blind: VisualGeometry<RectFrame> = { ...rects, anchor: () => null, path: () => false };
    const errors = checkGeometry({ layout: "drawlist.v1", geometry: blind }, rectScene().frame);
    expect(errors).toContain("row 0: no label anchor (box 200 × 100)");
    expect(errors).toContain("row 0: no outline");
    const sloppy: VisualGeometry<RectFrame> = { ...rects, path: (f, hit, inset, out) => (rects.path(f, hit, inset - 3, out), true) };
    expect(checkGeometry({ layout: "drawlist.v1", geometry: sloppy }, rectScene().frame)).toContain("row 0: outline point (-3.0, -3.0) outside its bounds");
  });

  it("reports shapes for HIT_NONE and geometry that throws", () => {
    const loose: VisualGeometry<RectFrame> = {
      ...rects,
      bounds: (f, hit) => ({ x: f.x[hit]!, y: f.y[hit]!, w: f.w[hit]!, h: f.h[hit]! }),
    };
    expect(checkGeometry({ layout: "drawlist.v1", geometry: loose }, rectScene().frame)).toContain("HIT_NONE has bounds");
    const fragile: VisualGeometry<RectFrame> = {
      ...rects,
      hitTest: (f, x, y) => {
        if (!f.count) throw new Error("no rows");
        return rects.hitTest(f, x, y);
      },
    };
    expect(checkGeometry({ layout: "drawlist.v1", geometry: fragile }, rectScene().frame)).toEqual(["empty frame: threw Error: no rows"]);
  });
});

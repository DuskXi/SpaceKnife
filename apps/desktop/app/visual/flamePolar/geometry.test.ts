// polar.v1 geometry: conformance on the fixture scenes, hits by cell (the hole, ring gaps, seams,
// the 12 o'clock wrap, the blank after a folder's children), outlines, and the sector distance the
// shader draws with.

import { describe, expect, it } from "vitest";

import type { PolarGeometry } from "@/protocol/generated";

import { Kind } from "../frame";
import { checkGeometry } from "../testing/conformance";
import { PathRecorder, polarScene, type SceneNode } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type PolarFrame } from "../types";
import { flamePolarComponent } from "./component";
import { angleAt, discRadius, IDENTITY, pointAt, polarGeometry, ringAt, seamInset, sectorDistance, tangentWidth, TURN, zoomTransform } from "./geometry";

const geo = (f: PolarFrame) => f.info.geometry as PolarGeometry;
const G = polarGeometry;

/** Lower edge of ring k's cell. */
const cellLo = (g: PolarGeometry, k: number) => g.hole + (k - 1) * g.ring_w;

/** Deterministic pseudo-random numbers in [0, 1). */
function random(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A random tree below a zoom root; the root keeps a random share of its own after its children. */
function randomTree(rand: () => number): SceneNode {
  let key = 1;
  const grow = (depth: number): SceneNode => {
    const k = key++;
    const n = depth < 5 && rand() < 0.7 ? Math.floor(rand() * 7) : 0;
    const children = Array.from({ length: n }, () => grow(depth + 1));
    const sum = children.reduce((s, c) => s + c.size, 0n);
    const size = n ? sum + BigInt(Math.floor(rand() < 0.5 ? 0 : Number(sum) * rand() * 0.3)) : BigInt(1 + Math.floor(rand() ** 3 * 5000));
    return { key: k, node: k, kind: n ? Kind.FOLDER : Kind.FILE, size, name: `n${k}`, children: n ? children : undefined };
  };
  return grow(0);
}

/** The cell under a point by plain search: no index, no bisection. */
function bruteHit(f: PolarFrame, px: number, py: number): number {
  const g = geo(f);
  if (px < 0 || py < 0 || px >= f.info.width || py >= f.info.height) return HIT_NONE;
  const r = Math.hypot(px - g.cx, py - g.cy);
  if (r < g.hole) return f.info.root ? HIT_ROOT : HIT_NONE;
  let th = Math.atan2(px - g.cx, g.cy - py);
  if (th < 0) th += TURN;
  const u = (th / TURN) * g.unit;
  for (let i = 0; i < f.count; i++) {
    const k = Math.floor((f.r0[i]! - g.hole) / g.ring_w) + 1;
    if (r >= cellLo(g, k) && r < cellLo(g, k + 1) && u >= f.a0[i]! && u < f.a1[i]!) return i;
  }
  return HIT_NONE;
}

/** Two files filling the whole turn, 60 / 40, with a folder of three below the second. */
const FULL: SceneNode = {
  key: 1,
  node: 1,
  kind: Kind.FOLDER,
  size: 100n,
  name: "full",
  children: [
    { key: 2, node: 2, kind: Kind.FILE, size: 60n, name: "a" },
    {
      key: 3,
      node: 3,
      kind: Kind.FOLDER,
      size: 40n,
      name: "b",
      children: [
        { key: 4, node: 4, kind: Kind.FILE, size: 20n, name: "c" },
        { key: 5, node: 5, kind: Kind.FILE, size: 20n, name: "d" },
      ],
    },
  ],
};

describe("conformance", () => {
  it("passes checkGeometry on every scene", () => {
    expect(flamePolarComponent).toMatchObject({ layout: "polar.v1", capability: "layout.polar.v1", hoverPlacement: "pointer", api: 1 });
    expect(checkGeometry(flamePolarComponent, polarScene().frame)).toEqual([]);
    expect(checkGeometry(flamePolarComponent, polarScene({ dpr: 1.5, width: 900, height: 600, detail: 15 }).frame)).toEqual([]);
    expect(checkGeometry(flamePolarComponent, polarScene({ dpr: 2, width: 800, height: 800, tree: FULL }).frame)).toEqual([]);
    expect(checkGeometry(flamePolarComponent, polarScene({ width: 60, height: 60 }).frame)).toEqual([]);
    const rand = random(7);
    for (let n = 0; n < 20; n++) {
      const scene = polarScene({ tree: randomTree(rand), width: 300 + Math.floor(rand() * 900), height: 300 + Math.floor(rand() * 600), dpr: [1, 1.25, 1.5, 2][n % 4]!, detail: 2 + (n % 9) });
      expect(checkGeometry(flamePolarComponent, scene.frame)).toEqual([]);
    }
  });

  it("has no shapes without the polar geometry", () => {
    const { frame } = polarScene();
    const bare = { ...frame, info: { ...frame.info, geometry: null } };
    expect(G.hitTest(bare, 200, 200)).toBe(HIT_NONE);
    expect(G.bounds(bare, 0)).toBeNull();
    expect(G.anchor(bare, HIT_ROOT, "label")).toBeNull();
    expect(G.box(bare, 0)).toEqual({ major: 0, minor: 0 });
    expect(G.path(bare, 0, 0, new PathRecorder())).toBe(false);
  });
});

describe("hitTest", () => {
  const { frame: f } = polarScene();
  const g = geo(f);
  const mid = (k: number) => cellLo(g, k) + g.ring_w / 2;
  const at = (r: number, a: number) => pointAt(g, r, a);

  it("gives the zoom root inside the hole, gaps around the disc included", () => {
    expect(G.hitTest(f, g.cx, g.cy)).toBe(HIT_ROOT);
    const edge = at(g.hole - 0.01, g.unit / 3);
    expect(G.hitTest(f, edge.x, edge.y)).toBe(HIT_ROOT);
    // the disc is drawn smaller than its cell: the gap before ring 1 still hits the root
    expect(discRadius(g)).toBe(g.hole - 1);
    const noRoot = { ...f, info: { ...f.info, root: null } };
    expect(G.hitTest(noRoot, g.cx, g.cy)).toBe(HIT_NONE);
  });

  it("gives nothing beyond the last ring and outside the canvas", () => {
    // three rings: x.pdf in the last one
    const { frame: s } = polarScene({ detail: 3 });
    const sg = geo(s);
    const pdf = s.rowOf(6);
    expect([sg.rings, ringAt(sg, s.r0[pdf]!)]).toEqual([3, 3]);
    const u = (s.a0[pdf]! + s.a1[pdf]!) / 2;
    const last = pointAt(sg, sg.radius - 0.5, u);
    const out = pointAt(sg, sg.radius + 0.5, u);
    expect([G.hitTest(s, last.x, last.y), G.hitTest(s, out.x, out.y)]).toEqual([pdf, HIT_NONE]);
    expect(G.hitTest(f, -1, g.cy)).toBe(HIT_NONE);
    expect(G.hitTest(f, f.info.width, g.cy)).toBe(HIT_NONE);
  });

  it("counts the gaps between rings to the ring whose cell holds them", () => {
    const a = f.rowOf(3); // sub, ring 1, with children in ring 2
    const u = (f.a0[a]! + f.a1[a]!) / 2;
    expect(ringAt(g, f.r0[a]!)).toBe(1);
    for (const r of [cellLo(g, 1) + 0.01, cellLo(g, 1) + 0.5, f.r1[a]! + 0.5, cellLo(g, 2) - 0.01]) {
      const p = at(r, u);
      expect(G.hitTest(f, p.x, p.y)).toBe(a);
    }
    const p = at(cellLo(g, 2) + 0.01, u);
    expect(f.parent[G.hitTest(f, p.x, p.y)]).toBe(a);
  });

  it("counts each half of a seam to the sector beside it", () => {
    const [a, b] = [f.rowOf(2), f.rowOf(3)];
    expect(f.a1[a]).toBe(f.a0[b]);
    // the seam is drawn gap / 2 = 1 px into each sector; half a px either side of the edge line
    const r = mid(1);
    const off = (Math.asin(0.5 / r) / TURN) * g.unit;
    const [before, after] = [at(r, f.a1[a]! - off), at(r, f.a1[a]! + off)];
    expect(G.hitTest(f, before.x, before.y)).toBe(a);
    expect(G.hitTest(f, after.x, after.y)).toBe(b);
    const s = { cx: g.cx, cy: g.cy, t0: f.a0[a]! / g.unit, t1: f.a1[a]! / g.unit, r0: f.r0[a]!, r1: f.r1[a]! };
    expect(sectorDistance(before.x, before.y, s, g.gap)).toBeGreaterThan(0); // in the drawn gap
  });

  it("leaves what a folder keeps for itself to nobody", () => {
    // the root's own 10 % after its children
    const blank = at(mid(1), 0.95 * g.unit);
    expect(G.hitTest(f, blank.x, blank.y)).toBe(HIT_NONE);
    // a folder of 40 with one file of 20: the second half of its span in ring 2 is blank
    const tree: SceneNode = {
      key: 1,
      node: 1,
      kind: Kind.FOLDER,
      size: 100n,
      name: "r",
      children: [
        { key: 2, node: 2, kind: Kind.FILE, size: 60n, name: "a" },
        { key: 3, node: 3, kind: Kind.FOLDER, size: 40n, name: "b", children: [{ key: 4, node: 4, kind: Kind.FILE, size: 20n, name: "c" }] },
      ],
    };
    const { frame: h } = polarScene({ tree });
    const hg = geo(h);
    const r2 = cellLo(hg, 2) + hg.ring_w / 2;
    const c = pointAt(hg, r2, 0.7 * hg.unit);
    const own = pointAt(hg, r2, 0.9 * hg.unit);
    const over = pointAt(hg, cellLo(hg, 1) + 3, 0.9 * hg.unit);
    expect([G.hitTest(h, c.x, c.y), G.hitTest(h, own.x, own.y), G.hitTest(h, over.x, over.y)]).toEqual([h.rowOf(4), HIT_NONE, h.rowOf(3)]);
  });

  it("wraps at 12 o'clock", () => {
    const { frame: w } = polarScene({ tree: FULL });
    const wg = geo(w);
    const [a, b] = [w.rowOf(2), w.rowOf(3)];
    expect([w.a0[a], w.a1[b]]).toEqual([0, wg.unit]);
    const r = cellLo(wg, 1) + wg.ring_w / 2;
    expect(G.hitTest(w, wg.cx + 0.01, wg.cy - r)).toBe(a);
    expect(G.hitTest(w, wg.cx - 0.01, wg.cy - r)).toBe(b);
    expect(G.hitTest(w, wg.cx, wg.cy - r)).toBe(a);
    expect(angleAt(wg, wg.cx - 1e-12, wg.cy - r)).toBeLessThan(wg.unit);
    // ring 2 under b: the last sector also ends at the full turn
    const r2 = cellLo(wg, 2) + wg.ring_w / 2;
    expect(G.hitTest(w, wg.cx - 0.01, wg.cy - r2)).toBe(w.rowOf(5));
    expect(G.hitTest(w, wg.cx + 0.01, wg.cy - r2)).toBe(HIT_NONE);
  });

  it("equals a plain search on random frames", () => {
    const rand = random(42);
    for (let n = 0; n < 30; n++) {
      const [width, height] = [200 + Math.floor(rand() * 1200), 200 + Math.floor(rand() * 800)];
      const { frame } = polarScene({ tree: randomTree(rand), width, height, dpr: [1, 1.25, 1.5, 2][n % 4]!, detail: 1 + (n % 12) });
      for (let k = 0; k < 400; k++) {
        const [px, py] = [rand() * (width + 20) - 10, rand() * (height + 20) - 10];
        expect(G.hitTest(frame, px, py)).toBe(bruteHit(frame, px, py));
      }
    }
  });

  it("finds rows of a ring the backend sent out of order", () => {
    const { frame } = polarScene({ tree: FULL });
    // ring 1 reversed: [b, a, c, d] would break parents-first, so swap a / b's angles instead
    const a0 = Int32Array.from(frame.a0);
    const a1 = Int32Array.from(frame.a1);
    const [a, b] = [frame.rowOf(2), frame.rowOf(3)];
    [a0[a], a1[a], a0[b], a1[b]] = [frame.a0[b]!, frame.a1[b]!, frame.a0[a]!, frame.a1[a]!];
    const swapped = { ...frame, a0, a1, geometry: { ...frame.geometry, a0, a1 } };
    const g2 = geo(swapped);
    const p = pointAt(g2, cellLo(g2, 1) + 5, 0.2 * g2.unit);
    expect(G.hitTest(swapped, p.x, p.y)).toBe(b);
  });
});

describe("bounds, path, box, anchors", () => {
  const scenes = [polarScene().frame, polarScene({ tree: FULL, dpr: 1.5, width: 700, height: 500 }).frame, polarScene({ tree: randomTree(random(3)), detail: 8, width: 1000, height: 900 }).frame];

  it("bounds hold the outline at any inset", () => {
    for (const f of scenes)
      for (let i = 0; i < f.count; i++)
        for (const inset of [0, 1, 3]) {
          const b = G.bounds(f, i)!;
          const rec = new PathRecorder();
          if (!G.path(f, i, inset, rec)) continue;
          for (const p of rec.points(64)) {
            expect(p.x).toBeGreaterThanOrEqual(b.x - 1e-6);
            expect(p.y).toBeGreaterThanOrEqual(b.y - 1e-6);
            expect(p.x).toBeLessThanOrEqual(b.x + b.w + 1e-6);
            expect(p.y).toBeLessThanOrEqual(b.y + b.h + 1e-6);
          }
        }
  });

  it("bounds a sector by its corners and the outer arc's extremes", () => {
    const { frame: f } = polarScene({ tree: FULL });
    const g = geo(f);
    // a: [0, 0.6) of ring 1 crosses 12, 3 and 6 o'clock
    const a = f.rowOf(2);
    const r1 = f.r1[a]!;
    const b = G.bounds(f, a)!;
    expect(b.x).toBeCloseTo(g.cx + r1 * Math.sin((f.a1[a]! / g.unit) * TURN), 9);
    expect(b.x + b.w).toBeCloseTo(g.cx + r1, 9);
    expect([b.y, b.y + b.h]).toEqual([g.cy - r1, g.cy + r1]);
    expect(G.bounds(f, HIT_ROOT)).toEqual({ x: g.cx - discRadius(g), y: g.cy - discRadius(g), w: 2 * discRadius(g), h: 2 * discRadius(g) });
  });

  it("outlines a full ring as two circles and the disc as one", () => {
    const tree: SceneNode = { key: 1, node: 1, kind: Kind.FOLDER, size: 10n, name: "one", children: [{ key: 2, node: 2, kind: Kind.FILE, size: 10n, name: "only" }] };
    const { frame: f } = polarScene({ tree });
    const g = geo(f);
    const rec = new PathRecorder();
    expect(G.path(f, 0, 0, rec)).toBe(true);
    const arcs = rec.ops.filter((o) => o.op === "arc");
    expect(arcs.map((o) => o.args[2])).toEqual([f.r1[0], f.r0[0]]);
    expect(rec.ops.map((o) => o.op)).toEqual(["moveTo", "arc", "closePath", "moveTo", "arc", "closePath"]);
    const disc = new PathRecorder();
    expect(G.path(f, HIT_ROOT, 2, disc)).toBe(true);
    expect(disc.ops.find((o) => o.op === "arc")!.args.slice(0, 5)).toEqual([g.cx, g.cy, discRadius(g) - 2, 0, TURN]);
    expect(G.box(f, 0).major).toBeCloseTo(Math.PI * (f.r0[0]! + f.r1[0]!), 9);
  });

  it("moves the radial edges in by the seam rule", () => {
    const { frame: f } = polarScene();
    const g = geo(f);
    const i = f.rowOf(2);
    const rec = new PathRecorder();
    G.path(f, i, 0, rec);
    // every outline vertex keeps gap / 2 from both edge lines
    const s = { cx: g.cx, cy: g.cy, t0: f.a0[i]! / g.unit, t1: f.a1[i]! / g.unit, r0: f.r0[i]!, r1: f.r1[i]! };
    for (const p of rec.points(16)) expect(sectorDistance(p.x, p.y, s, g.gap)).toBeLessThanOrEqual(1e-6);
    const corner = rec.ops[0]!.args;
    expect(Math.abs(sectorDistance(corner[0]!, corner[1]!, s, g.gap))).toBeLessThan(1e-6);
    expect(G.path(f, i, 20, new PathRecorder())).toBe(false); // nothing left at that inset
  });

  it("gives box major / minor and anchors only where they fit", () => {
    const { frame: f } = polarScene({ width: 800, height: 800, detail: 3 });
    const g = geo(f);
    const i = f.rowOf(2);
    const T = f.r1[i]! - f.r0[i]!;
    const arc = ((f.a1[i]! - f.a0[i]!) / g.unit) * TURN * ((f.r0[i]! + f.r1[i]!) / 2);
    expect(G.box(f, i)).toEqual({ major: Math.max(arc, T), minor: Math.min(arc, T) });
    const label = G.anchor(f, i, "label")!;
    expect(Math.hypot(label.x - g.cx, label.y - g.cy)).toBeCloseTo((f.r0[i]! + f.r1[i]!) / 2, 9);
    const tag = G.anchor(f, i, "tag")!;
    expect(Math.hypot(tag.x - g.cx, tag.y - g.cy)).toBeCloseTo(f.r1[i]! - 10, 9);
    const badge = G.anchor(f, i, "badge")!;
    expect(Math.hypot(badge.x - g.cx, badge.y - g.cy)).toBeCloseTo(f.r0[i]! + 12, 9);
    expect(G.anchor(f, HIT_NONE, "label")).toBeNull();
    expect(G.filled(f, i)).toBe(true);
    // a 1 % sliver holds a label anchor but neither a tag marker nor a badge
    const tree: SceneNode = {
      key: 1,
      node: 1,
      kind: Kind.FOLDER,
      size: 100n,
      name: "r",
      children: [
        { key: 2, node: 2, kind: Kind.FILE, size: 99n, name: "big" },
        { key: 3, node: 3, kind: Kind.FILE, size: 1n, name: "tiny" },
      ],
    };
    const { frame: s } = polarScene({ tree });
    const tiny = s.rowOf(3);
    expect(G.anchor(s, tiny, "label")).not.toBeNull();
    expect(G.anchor(s, tiny, "tag")).toBeNull();
    expect(G.anchor(s, tiny, "badge")).toBeNull();
    expect(G.box(s, tiny).minor).toBeLessThan(5);
  });
});

describe("sectorDistance", () => {
  const s = { cx: 0, cy: 0, t0: 0.1, t1: 0.3, r0: 100, r1: 140 };
  const at = (r: number, t: number) => ({ x: r * Math.sin(t * TURN), y: -r * Math.cos(t * TURN) });

  it("is negative inside, positive outside", () => {
    const c = at(120, 0.2);
    expect(sectorDistance(c.x, c.y, s, 2)).toBeCloseTo(-20, 9);
    const inside = at(139, 0.2);
    expect(sectorDistance(inside.x, inside.y, s, 2)).toBeCloseTo(-1, 9);
    for (const [r, t] of [
      [150, 0.2],
      [90, 0.2],
      [120, 0.05],
      [120, 0.35],
      [120, 0.7],
    ] as const) {
      const p = at(r, t);
      expect(sectorDistance(p.x, p.y, s, 2)).toBeGreaterThan(0);
    }
    expect(sectorDistance(0, 0, s, 2)).toBeGreaterThan(0);
  });

  it("leaves a gap as wide as the frame's between neighbours", () => {
    const next = { ...s, t0: 0.3, t1: 0.45 };
    const e = at(120, 0.3);
    expect(sectorDistance(e.x, e.y, s, 2)).toBeCloseTo(1, 9);
    expect(sectorDistance(e.x, e.y, next, 2)).toBeCloseTo(1, 9);
    // one px across the edge line, perpendicular to it, each side reaches its border
    const n = { x: Math.cos(0.3 * TURN), y: Math.sin(0.3 * TURN) }; // into `next`
    expect(sectorDistance(e.x + n.x, e.y + n.y, next, 2)).toBeCloseTo(0, 9);
    expect(sectorDistance(e.x - n.x, e.y - n.y, s, 2)).toBeCloseTo(0, 9);
    // the gap is the same width at every radius (not a wedge)
    const out = at(139, 0.3);
    expect(sectorDistance(out.x - n.x, out.y - n.y, s, 2)).toBeCloseTo(0, 9);
  });

  it("has no radial edges on a full turn", () => {
    const ring = { ...s, t0: 0, t1: 1 };
    for (const t of [0, 0.25, 0.5, 0.999]) {
      const p = at(120, t);
      expect(sectorDistance(p.x, p.y, ring, 2)).toBeCloseTo(-20, 9);
    }
    expect(seamInset(TURN, 100, 2)).toBe(0);
  });

  it("covers a sector wider than half a turn", () => {
    const wide = { ...s, t0: 0.05, t1: 0.9 };
    for (const t of [0.1, 0.5, 0.6, 0.85]) {
      const p = at(120, t);
      expect(sectorDistance(p.x, p.y, wide, 2)).toBeLessThan(0);
    }
    for (const t of [0.0, 0.95, 0.99]) {
      const p = at(120, t);
      expect(sectorDistance(p.x, p.y, wide, 2)).toBeGreaterThan(0);
    }
  });

  it("keeps a thin spike at least 1 px wide at its inner radius", () => {
    for (const across of [1, 1.2, 1.6, 2, 2.5, 3, 4, 6]) {
      const r0 = 80;
      const delta = across / r0; // arc at the inner radius, px
      const spike = { cx: 0, cy: 0, t0: 0.3, t1: 0.3 + delta / TURN, r0, r1: 120 };
      // walk across the bisector just outside the inner radius and measure what is inside
      const mid = ((spike.t0 + spike.t1) / 2) * TURN;
      const [ux, uy] = [Math.sin(mid), -Math.cos(mid)];
      const [nx, ny] = [Math.cos(mid), Math.sin(mid)];
      let width = 0;
      const step = 0.001;
      for (let d = -5; d <= 5; d += step) {
        const [x, y] = [(r0 + 0.01) * ux + d * nx, (r0 + 0.01) * uy + d * ny];
        if (sectorDistance(x, y, spike, 2) <= 0) width += step;
      }
      expect(width).toBeGreaterThanOrEqual(Math.min(across, 1) - 0.01);
      expect(width).toBeCloseTo(Math.max(Math.min(across, 1), across - 2), 1);
    }
  });
});

describe("tangentWidth and zoomTransform", () => {
  it("takes the smaller of the mid arc and the chord inside the outer edge", () => {
    // a wide sector: the chord at r_mid = 110 inside r1 − 2 = 118 decides
    expect(tangentWidth(Math.PI, 100, 120, 2, 1)).toBeCloseTo(2 * Math.sqrt(118 ** 2 - 110 ** 2), 9);
    // a narrow one: the arc less a gap
    expect(tangentWidth(0.2, 100, 120, 2, 1)).toBeCloseTo(0.2 * 110 - 2, 9);
    expect(tangentWidth(0.2, 100, 101, 2, 1)).toBe(0);
  });

  it("opens the zoomed row to the whole turn in the disc's place", () => {
    const { frame: f } = polarScene();
    const g = geo(f);
    const sub = f.rowOf(3);
    expect(zoomTransform(f, sub, 0)).toEqual(IDENTITY);
    const [st, tt, sr, tr] = zoomTransform(f, sub, 1);
    expect((f.a0[sub]! / g.unit) * st + tt).toBeCloseTo(0, 12);
    expect((f.a1[sub]! / g.unit) * st + tt).toBeCloseTo(1, 12);
    expect(sr).toBe(1);
    expect(f.r1[sub]! + tr).toBeLessThan(g.hole);
    // its children move to ring 1
    const kid = Array.from({ length: f.count }, (_, i) => i).find((i) => f.parent[i] === sub)!;
    expect(ringAt(g, f.r0[kid]! + tr)).toBe(1);
    expect(zoomTransform(f, HIT_ROOT, 0.5)).toEqual(IDENTITY);
    const [h0] = zoomTransform(f, sub, 0.5);
    expect(h0).toBeCloseTo(1 + (g.unit / (f.a1[sub]! - f.a0[sub]!) - 1) / 2, 12);
  });
});

// flame.v1 geometry: the conformance checks on the fixture scenes, the band cells with their gaps,
// the binary search against a brute-force reference on random frames, and the two origins.

import { describe, expect, it } from "vitest";

import type { FlameGeometry } from "@/protocol/generated";

import { Kind, Label } from "../frame";
import { checkGeometry } from "../testing/conformance";
import { encodeFrame, PathRecorder, rectScene, type SceneNode } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type Hit, type RectFrame } from "../types";
import { bandAt, flameGeometry, flameOf, rootBar } from "./geometry";

const flame = { layout: "flame.v1", geometry: flameGeometry };
const geo = (f: RectFrame) => f.info.geometry as FlameGeometry;
const hit = (f: RectFrame, x: number, y: number) => flameGeometry.hitTest(f, x, y);

/** Deterministic PRNG (mulberry32). */
function prng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A random folder tree: up to 6 children per folder, sizes skewed small, some own share left blank. */
function randomTree(rng: () => number): SceneNode {
  let next = 2;
  const grow = (depth: number): SceneNode[] =>
    Array.from({ length: Math.floor(rng() * 7) }, () => {
      const key = next++;
      if (depth < 6 && rng() < 0.45) {
        const children = grow(depth + 1);
        const size = children.reduce((s, c) => s + c.size, BigInt(Math.floor(rng() * 60)));
        return { key, node: key, kind: Kind.FOLDER, size: size > 0n ? size : 1n, name: `d${key}`, children };
      }
      return { key, node: key, kind: Kind.FILE, size: BigInt(1 + Math.floor(rng() ** 3 * 2000)), name: `f${key}` };
    });
  const children = grow(1);
  return { key: 1, node: 1, kind: Kind.FOLDER, size: children.reduce((s, c) => s + c.size, BigInt(Math.floor(rng() * 200))), name: "root", children };
}

/** Reference hit test: every row's cell (bar plus the gap on its right and on its far side from the root). */
function bruteHit(f: RectFrame, px: number, py: number): Hit {
  const g = geo(f);
  if (px < 0 || py < 0 || px >= f.info.width || py >= f.info.height) return HIT_NONE;
  const cell = (y: number, h: number) => (g.origin === "top" ? [y, y + h + g.gap] : [y - g.gap, y + h]);
  for (let i = 0; i < f.count; i++) {
    const [top, bottom] = cell(f.y[i]!, f.h[i]!);
    if (py >= top! && py < bottom! && px >= f.x[i]! && px < f.x[i]! + f.w[i]! + g.gap) return i;
  }
  const [top, bottom] = cell(g.root.y, g.root.h);
  return f.info.root && py >= top! && py < bottom! && px >= g.root.x && px < g.root.x + g.root.w ? HIT_ROOT : HIT_NONE;
}

describe("flameGeometry conformance", () => {
  it("passes checkGeometry on every fixture scene", () => {
    const scenes = [
      rectScene({ layout: "flame.v1" }),
      rectScene({ layout: "flame.v1", origin: "top" }),
      rectScene({ layout: "flame.v1", dpr: 2, width: 800, height: 400 }),
      rectScene({ layout: "flame.v1", origin: "top", dpr: 1.5, width: 900, height: 700, detail: 15 }),
      rectScene({ layout: "flame.v1", detail: 2 }),
      rectScene({ layout: "flame.v1", width: 60, height: 30 }),
      rectScene({ layout: "flame.v1", tree: randomTree(prng(7)), width: 1200, height: 500, detail: 8 }),
    ];
    for (const s of scenes) expect(checkGeometry(flame, s.frame)).toEqual([]);
  });

  it("passes on a zoom root that is a file: no rows, only the root bar", () => {
    const s = rectScene({ layout: "flame.v1", tree: { key: 9, node: 9, kind: Kind.FILE, size: 500n, name: "big.iso" } });
    expect(s.frame.count).toBe(0);
    expect(checkGeometry(flame, s.frame)).toEqual([]);
    const r = rootBar(s.frame)!;
    expect(hit(s.frame, 100, r.y + 5)).toBe(HIT_ROOT);
    expect(hit(s.frame, 100, r.y - 20)).toBe(HIT_NONE);
  });

  it("knows only flame frames", () => {
    const treemap = rectScene().frame;
    expect(flameOf(treemap)).toBeNull();
    expect(hit(treemap, 10, 50)).toBe(HIT_NONE);
    expect(flameGeometry.bounds(treemap, HIT_ROOT)).toBeNull();
  });
});

describe("flameGeometry hit test", () => {
  const { frame: f } = rectScene({ layout: "flame.v1" });
  const g = geo(f);
  const band1 = (key: number) => f.rowOf(key);

  it("hits the root bar in band 0, including the gap above it", () => {
    // 400 × 200, root at the bottom: root bar y 169..200, its gap 167..169
    expect(g.root).toEqual({ x: 0, y: 169, w: 400, h: 31 });
    expect(hit(f, 0, 199.5)).toBe(HIT_ROOT);
    expect(hit(f, 399.5, 170)).toBe(HIT_ROOT);
    expect(hit(f, 200, 167)).toBe(HIT_ROOT);
    expect(hit(f, 200, 166.9)).not.toBe(HIT_ROOT);
    const noRoot = rectScene({ layout: "flame.v1", info: { root: null } }).frame;
    expect(hit(noRoot, 200, 180)).toBe(HIT_NONE);
    expect(flameGeometry.bounds(noRoot, HIT_ROOT)).toBeNull();
    expect(flameGeometry.path(noRoot, HIT_ROOT, 0, new PathRecorder())).toBe(false);
  });

  it("gives the gap between two bands to the one nearer the root", () => {
    const a = band1(2); // a.mp4, band 1: y 136..167
    expect([f.y[a], f.h[a]]).toEqual([136, 31]);
    expect(hit(f, 10, 166.5)).toBe(a);
    expect(hit(f, 10, 167)).toBe(HIT_ROOT); // the gap between band 1 and the root bar
    expect(hit(f, 10, 134)).toBe(a); // the gap between band 2 and band 1
    expect(hit(f, 10, 133.9)).toBe(HIT_NONE); // band 2 has nothing above a file
  });

  it("gives the gap right of a bar to that bar, and the blank after the last child to nobody", () => {
    const a = band1(2);
    const sub = band1(3);
    const right = f.x[a]! + f.w[a]!;
    expect(f.x[sub]).toBe(right + g.gap); // siblings share the boundary
    expect(hit(f, right + 0.5, 150)).toBe(a);
    expect(hit(f, right + g.gap, 150)).toBe(sub);
    // the zoom root's own 10 % after its children is background
    const last = band1(0xc0000001); // free space, the last child
    const end = f.x[last]! + f.w[last]! + g.gap;
    expect(hit(f, end - 0.5, 150)).toBe(last);
    expect(hit(f, end + 1, 150)).toBe(HIT_NONE);
    expect(hit(f, 399, 150)).toBe(HIT_NONE);
  });

  it("hits the rightmost bar up to the canvas edge and nothing beyond", () => {
    const tree: SceneNode = {
      key: 1,
      node: 1,
      kind: Kind.FOLDER,
      size: 300n,
      name: "full",
      children: [
        { key: 2, node: 2, kind: Kind.FILE, size: 100n, name: "a" },
        { key: 3, node: 3, kind: Kind.FILE, size: 200n, name: "b" },
      ],
    };
    const s = rectScene({ layout: "flame.v1", tree }).frame;
    const b = s.rowOf(3);
    expect(s.x[b]! + s.w[b]!).toBe(400);
    expect(hit(s, 399.9, s.y[b]! + 1)).toBe(b);
    expect(hit(s, 400, s.y[b]! + 1)).toBe(HIT_NONE);
    expect(hit(s, -0.1, s.y[b]! + 1)).toBe(HIT_NONE);
  });

  it("finds nothing past the last band or outside the canvas", () => {
    // 5 bands of 33 px above the root bar: band 5's cell starts at y 2
    expect(bandAt(g, 2)).toBe(5);
    expect(bandAt(g, 1.9)).toBe(-1);
    expect(hit(f, 300, 1)).toBe(HIT_NONE);
    for (const [x, y] of [
      [-1, 150],
      [200, -1],
      [200, 200],
      [400, 180],
      [Number.NaN, 150],
    ] as const)
      expect(hit(f, x, y)).toBe(HIT_NONE);
  });

  it("equals a brute-force search on random frames", () => {
    for (let seed = 1; seed <= 12; seed++) {
      const rng = prng(seed);
      const origin = seed % 2 ? "bottom" : "top";
      const [width, height] = [300 + Math.floor(rng() * 1500), 120 + Math.floor(rng() * 600)];
      const s = rectScene({ layout: "flame.v1", tree: randomTree(rng), origin, width, height, dpr: [1, 1.25, 2][seed % 3]!, detail: 3 + (seed % 10) });
      for (let k = 0; k < 1500; k++) {
        const px = rng() * (width + 20) - 10;
        const py = rng() * (height + 20) - 10;
        expect(hit(s.frame, px, py)).toBe(bruteHit(s.frame, px, py));
      }
      // on every bar's corners and edges too
      for (let i = 0; i < s.frame.count; i++) {
        const [x, y, w, h] = [s.frame.x[i]!, s.frame.y[i]!, s.frame.w[i]!, s.frame.h[i]!];
        for (const [px, py] of [
          [x, y],
          [x + w - 0.01, y + h - 0.01],
          [x + w, y],
          [x - 0.01, y + h / 2],
        ] as const)
          expect(hit(s.frame, px, py)).toBe(bruteHit(s.frame, px, py));
      }
    }
  });

  it("does not rely on rows listed in x order", () => {
    const rows = [
      { key: 2, node: 2, parent: -1, kind: Kind.FILE, depth: 1, geo: [210, 136, 190, 31] as const, label: Label.NAME, size: 2n, name: "b" },
      { key: 3, node: 3, parent: -1, kind: Kind.FILE, depth: 1, geo: [0, 136, 208, 31] as const, label: Label.NAME, size: 3n, name: "a" },
    ];
    const s = encodeFrame<RectFrame>("flame.v1", rows, { width: 400, height: 200, geometry: g, root: rectScene({ layout: "flame.v1" }).info.root });
    expect(hit(s.frame, 100, 150)).toBe(1);
    expect(hit(s.frame, 300, 150)).toBe(0);
    expect(hit(s.frame, 209, 150)).toBe(1);
  });
});

describe("flameGeometry origins", () => {
  it("mirrors the bands: the root at the top hits what the root at the bottom hits upside down", () => {
    const tree = randomTree(prng(42));
    const bottom = rectScene({ layout: "flame.v1", tree, width: 640, height: 360, detail: 6 }).frame;
    const top = rectScene({ layout: "flame.v1", tree, width: 640, height: 360, detail: 6, origin: "top" }).frame;
    expect(geo(top).root.y).toBe(0);
    expect(geo(bottom).root.y + geo(bottom).root.h).toBe(360);
    // half-pixel points never sit on a cell boundary in either frame
    for (let y = 0; y < 360; y++) for (let x = 0; x < 640; x += 7) expect(hit(top, x + 0.5, 359.5 - y)).toBe(hit(bottom, x + 0.5, y + 0.5));
  });

  it("puts band k at k·row_h from the root's side", () => {
    const top = rectScene({ layout: "flame.v1", origin: "top" });
    const g = geo(top.frame);
    for (let k = 0; k <= g.rows; k++) {
      expect(bandAt(g, k * g.row_h)).toBe(k);
      expect(bandAt(g, (k + 1) * g.row_h - 0.01)).toBe(k);
    }
    expect(bandAt(g, -0.01)).toBe(-1);
    const bottom = geo(rectScene({ layout: "flame.v1" }).frame);
    for (let k = 0; k <= bottom.rows; k++) {
      expect(bandAt(bottom, 200 - k * bottom.row_h - 0.01)).toBe(k);
      expect(bandAt(bottom, 200 - (k + 1) * bottom.row_h)).toBe(k);
    }
  });
});

describe("flameGeometry shapes", () => {
  it("bounds a bar by its rectangle and the root by geometry.root", () => {
    const { frame: f } = rectScene({ layout: "flame.v1" });
    const i = f.rowOf(3);
    expect(flameGeometry.bounds(f, i)).toEqual({ x: f.x[i], y: f.y[i], w: f.w[i], h: f.h[i] });
    expect(flameGeometry.bounds(f, HIT_ROOT)).toEqual(geo(f).root);
    expect(flameGeometry.bounds(f, HIT_ROOT)).not.toBe(geo(f).root); // a copy: callers cannot move the root
    expect(flameGeometry.bounds(f, f.count)).toBeNull();
    expect(flameGeometry.box(f, i)).toEqual({ major: f.w[i], minor: f.h[i] });
    expect(flameGeometry.box(f, HIT_NONE)).toEqual({ major: 0, minor: 0 });
  });

  it("outlines a bar as its rectangle moved inward", () => {
    const { frame: f } = rectScene({ layout: "flame.v1" });
    const i = f.rowOf(2);
    const rec = new PathRecorder();
    expect(flameGeometry.path(f, i, 1.5, rec)).toBe(true);
    expect(rec.ops).toEqual([{ op: "rect", args: [f.x[i]! + 1.5, f.y[i]! + 1.5, f.w[i]! - 3, f.h[i]! - 3] }]);
    expect(flameGeometry.path(f, i, f.h[i]! / 2, new PathRecorder())).toBe(false);
    const out = new PathRecorder();
    expect(flameGeometry.path(f, HIT_ROOT, -2, out)).toBe(true);
    expect(out.ops[0]!.args).toEqual([-2, 167, 404, 35]);
  });

  it("places the tag marker on bars of 22 × 18 CSS px and the badge on bars of 70 × 40", () => {
    // 400 × 600: bars are 46 px high (row_max 48 − gap)
    const tall = rectScene({ layout: "flame.v1", width: 400, height: 600 }).frame;
    const a = tall.rowOf(2);
    expect(tall.h[a]).toBe(46);
    expect(flameGeometry.anchor(tall, a, "label")).toEqual({ x: tall.x[a]! + tall.w[a]! / 2, y: tall.y[a]! + 23 });
    expect(flameGeometry.anchor(tall, a, "tag")).toEqual({ x: tall.x[a]! + tall.w[a]! - 18, y: tall.y[a]! + 4 });
    expect(flameGeometry.anchor(tall, a, "badge")).toEqual({ x: tall.x[a]! + 21, y: tall.y[a]! + 46 - 13.5 });
    const short = rectScene({ layout: "flame.v1" }).frame; // 31 px bars: no badge
    expect(flameGeometry.anchor(short, short.rowOf(2), "badge")).toBeNull();
    expect(flameGeometry.anchor(short, short.rowOf(2), "tag")).not.toBeNull();
    // at dpr 2 the same CSS sizes count
    const hidpi = rectScene({ layout: "flame.v1", width: 800, height: 1200, dpr: 2 }).frame;
    const b = hidpi.rowOf(2);
    expect(flameGeometry.anchor(hidpi, b, "tag")).toEqual({ x: hidpi.x[b]! + hidpi.w[b]! - 36, y: hidpi.y[b]! + 8 });
    // a narrow bar has no room for a marker
    const narrow = rectScene({ layout: "flame.v1", width: 40, height: 200 }).frame;
    for (let i = 0; i < narrow.count; i++) if (narrow.w[i]! < 22) expect(flameGeometry.anchor(narrow, i, "tag")).toBeNull();
  });

  it("fills every bar", () => {
    const { frame: f } = rectScene({ layout: "flame.v1" });
    for (let i = 0; i < f.count; i++) expect(flameGeometry.filled(f, i)).toBe(true);
    expect(f.kind[f.rowOf(3)]).toBe(Kind.FOLDER);
  });
});

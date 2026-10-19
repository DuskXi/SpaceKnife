// flame.v1 paint: instances per row and overlay, the colour rules, stripes and dots, labels only
// where the backend set label bits, the root bar, the selection and the halo, the zoom transform.

import { tokens } from "@design/tokens";
import { describe, expect, it } from "vitest";

import { fillOf, Pattern, type RGBA, rgba, STRIDE } from "@/treemap/paint";
import type { PreparedText } from "@/treemap/text";

import { branchFill, CENTER, HALO_OVERLAY, ringFill } from "../colors";
import { DStat, Flag, Kind, Label } from "../frame";
import { FREE_KEY, rectScene, SCENE_TREE, type SceneNode, SMALL_KEY } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type PaintSettings, type RectFrame } from "../types";
import { flameGeometry } from "./geometry";
import { fillFor, haloOf, Instances, moveTags, moveTexts, overlayLimit, paintFlame, paintFlameOverlay, SELECTION_INSTANCES, selectedOf, zoomXform } from "./paint";

const T = tokens.treemap;

const settings = (patch: Partial<PaintSettings> = {}): PaintSettings => ({
  colorMode: "branch",
  classIds: ["other", "video", "doc", "archive"],
  classColors: [],
  scanning: false,
  formatSize: (b) => `${b} B`,
  formatInt: (n) => `#${n}`,
  formatDelta: (d) => `${d >= 0 ? "+" : "−"}${Math.abs(d)} B`,
  haloLevels: 3,
  hoverHighlight: true,
  selectionShadow: true,
  fontScale: 1,
  levelContrast: 1,
  borderContrast: 1,
  reduced: false,
  diffTiers: null,
  ...patch,
});

const slot = (p: { instances: Float32Array }, k: number) => p.instances.subarray(k * STRIDE, (k + 1) * STRIDE);
/** The main picture's instance of a row: the root bar comes first. */
const bar = (p: { instances: Float32Array }, row: number) => slot(p, row + 1);

const expectRGBA = (actual: ArrayLike<number>, expected: RGBA) => {
  expect(actual).toHaveLength(4);
  for (let k = 0; k < 4; k++) expect(actual[k]).toBeCloseTo(expected[k]!, 5);
};
const lighten = (c: RGBA): RGBA => {
  const a = HALO_OVERLAY[3];
  return [0, 1, 2].map((i) => HALO_OVERLAY[i]! * a + c[i]! * (1 - a)).concat(a + c[3] * (1 - a)) as RGBA;
};

/** A scene tree with the zoom root's children replaced. */
const treeOf = (children: SceneNode[], root: Partial<SceneNode> = {}): SceneNode => ({ ...SCENE_TREE, ...root, children });

describe("paintFlame instances", () => {
  it("draws the root bar and one bar per row; the overlay stays within its limit", () => {
    for (const haloLevels of [0, 1, 3, 10, Infinity]) {
      const s = settings({ haloLevels });
      for (const scene of [rectScene({ layout: "flame.v1" }), rectScene({ layout: "flame.v1", origin: "top", detail: 2 })]) {
        const f = scene.frame;
        const main = paintFlame(f, s);
        expect(main.count).toBe(f.count + 1);
        for (const hover of [HIT_NONE, HIT_ROOT, ...Array.from({ length: f.count }, (_, i) => i)]) {
          const over = paintFlameOverlay(f, s, hover);
          expect(over.count).toBeLessThanOrEqual(overlayLimit(haloLevels));
          expect(main.count + over.count).toBeLessThanOrEqual(f.count + 1 + overlayLimit(haloLevels));
          expect(over.texts).toEqual([]);
        }
      }
    }
  });

  it("writes each bar's rectangle, rounded at the file radius", () => {
    const { frame: f } = rectScene({ layout: "flame.v1" });
    const p = paintFlame(f, settings());
    expect(Array.from(slot(p, 0).subarray(0, 4))).toEqual([0, 169, 400, 31]);
    for (let i = 0; i < f.count; i++) {
      expect(Array.from(bar(p, i).subarray(0, 4))).toEqual([f.x[i], f.y[i], f.w[i], f.h[i]]);
      expect(bar(p, i)[16]).toBe(T.geometry.fileRadius);
    }
  });

  it("reuses its buffer and only grows it", () => {
    const { frame: f } = rectScene({ layout: "flame.v1" });
    const buf = new Instances(4);
    const first = paintFlame(f, settings(), buf).instances;
    expect(paintFlame(f, settings({ colorMode: "class" }), buf).instances).toBe(first);
    const over = new Instances();
    const data = paintFlameOverlay(f, settings(), 0, over).instances;
    for (let i = 0; i < f.count; i++) expect(paintFlameOverlay(f, settings(), i, over).instances).toBe(data);
    const big = rectScene({ layout: "flame.v1", tree: treeOf(Array.from({ length: 80 }, (_, k) => ({ key: 100 + k, node: 100 + k, kind: Kind.FILE, size: 10n, name: `f${k}` })), { size: 800n }), width: 2000 }).frame;
    expect(paintFlame(big, settings(), buf).count).toBe(big.count + 1);
    expect(buf.data.length).toBeGreaterThanOrEqual((big.count + 1) * STRIDE);
  });

  it("draws nothing but the root bar on an empty frame, and nothing at all without a root", () => {
    const tree: SceneNode = { key: 9, node: 9, kind: Kind.FILE, size: 500n, name: "big.iso" };
    const file = rectScene({ layout: "flame.v1", tree }).frame;
    const p = paintFlame(file, settings());
    expect(file.count).toBe(0);
    expect(p.count).toBe(1);
    expect(p.texts.map((t) => [t.text, t.sub?.text])).toEqual([["big.iso", "500 B"]]);
    expect(paintFlameOverlay(file, settings(), HIT_ROOT).count).toBe(1); // the hover inset
    const none = rectScene({ layout: "flame.v1", tree, info: { root: null } }).frame;
    expect(paintFlame(none, settings())).toMatchObject({ count: 0, texts: [], tags: [] });
    expect(paintFlameOverlay(none, settings(), HIT_ROOT).count).toBe(0);
  });
});

describe("paintFlame colours", () => {
  const { frame: f } = rectScene({ layout: "flame.v1" });
  const solid = Array.from({ length: f.count }, (_, i) => i).filter((i) => f.kind[i]! < Kind.FREE && f.kind[i] !== Kind.PENDING);

  it("colours every solid bar by its branch slot and depth under 「顶层分支」", () => {
    const p = paintFlame(f, settings());
    expect(solid.length).toBeGreaterThan(4);
    for (const i of solid) expectRGBA(bar(p, i).subarray(4, 8), branchFill(f.branch![i]!, f.depth[i]!));
    // the folder bar and its file share the branch's hue, a shade apart
    expect(f.branch![f.rowOf(3)]).toBe(f.branch![f.rowOf(4)]);
    expect(Array.from(bar(p, f.rowOf(3)).subarray(4, 8))).not.toEqual(Array.from(bar(p, f.rowOf(4)).subarray(4, 8)));
  });

  it("is neutral without branch slots (an old server's frame)", () => {
    const old = { ...f, branch: null } as RectFrame;
    expectRGBA(fillFor(old, old.rowOf(2), settings()), branchFill(0, 1));
  });

  it("puts folder bars on the ring tones and files on the colour mode otherwise", () => {
    for (const colorMode of ["class", "flat", "time"] as const) {
      const s = settings({ colorMode });
      const p = paintFlame(f, s);
      for (const i of solid) {
        const kind = f.kind[i]!;
        const want = kind === Kind.FOLDER || kind === Kind.COLLAPSED ? ringFill(f.depth[i]!) : fillOf(f, i, colorMode, s.classIds, undefined, s.classColors);
        expectRGBA(bar(p, i).subarray(4, 8), want);
      }
    }
    expectRGBA(bar(paintFlame(f, settings({ colorMode: "class" })), f.rowOf(2)).subarray(4, 8), rgba(T.class.video));
    expectRGBA(bar(paintFlame(f, settings({ colorMode: "class", classColors: ["", "#123456"] })), f.rowOf(2)).subarray(4, 8), rgba("#123456"));
  });

  it("colours folder bars too by the compare scale", () => {
    const tree = treeOf([
      { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "a", delta: 5_000_000n, dstat: DStat.CHANGED },
      { key: 3, node: 3, kind: Kind.FOLDER, size: 350n, name: "sub", delta: -2_000n, dstat: DStat.CHANGED, children: [{ key: 4, node: 4, kind: Kind.FILE, size: 350n, name: "b", delta: 0n }] },
    ]);
    const c = rectScene({ layout: "flame.v1", tree }).frame;
    const s = settings({ diffTiers: [1_000, 1_000_000, 1_000_000_000] });
    const p = paintFlame(c, s);
    for (const key of [2, 3, 4]) {
      const i = c.rowOf(key);
      expectRGBA(bar(p, i).subarray(4, 8), fillOf(c, i, "class", [], s.diffTiers!));
    }
    expectRGBA(bar(p, c.rowOf(4)).subarray(4, 8), rgba(T.collapsed)); // unchanged
    // the size line becomes the change
    const sized = rectScene({ layout: "flame.v1", tree: treeOf(tree.children!.map((k) => ({ ...k, label: Label.NAME | Label.SIZE }))) }).frame;
    expect(paintFlame(sized, s).texts.map((t) => t.text)).toContain("+5000000 B");
  });

  it("stripes free and unknown space and folders waiting for the scan, dashing all but free space", () => {
    const tree = treeOf([...SCENE_TREE.children!, { key: 0xc0000002, node: -1, kind: Kind.UNKNOWN, size: 50n }]);
    const g = rectScene({ layout: "flame.v1", tree }).frame;
    const p = paintFlame(g, settings());
    const [free, pending, unknown] = [g.rowOf(FREE_KEY), g.rowOf(7), g.rowOf(0xc0000002)].map((i) => bar(p, i));
    expect([free![19], free![21]]).toEqual([Pattern.STRIPES_135, 0]);
    expectRGBA(free!.subarray(4, 8), rgba(T.free.stripeA));
    expectRGBA(free!.subarray(8, 12), rgba(T.free.border));
    expect([pending![19], pending![21], pending![20]]).toEqual([Pattern.STRIPES_135, 1, 8]);
    expect([unknown![19], unknown![21], unknown![20]]).toEqual([Pattern.STRIPES_45, 1, 3]);
    expect(p.texts.find((t) => t.text === "可用空间")!.color).toBe(T.free.text);
  });

  it("dots aggregates and dims what the filter dims (free space less)", () => {
    const tree = treeOf([
      { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "a", flags: Flag.DIM },
      { key: SMALL_KEY, node: 1, kind: Kind.SMALL, size: 100n, count: 9, flags: Flag.DIM },
      { key: 3, node: 3, kind: Kind.FOLDER, size: 300n, name: "sub", flags: Flag.DIM, children: [{ key: 4, node: 4, kind: Kind.FILE, size: 300n, name: "b" }] },
      { key: FREE_KEY, node: -1, kind: Kind.FREE, size: 100n, flags: Flag.DIM },
    ]);
    const d = rectScene({ layout: "flame.v1", tree }).frame;
    const p = paintFlame(d, settings());
    for (const key of [2, SMALL_KEY, 3]) expect(bar(p, d.rowOf(key))[22]).toBeCloseTo(T.dimOpacity, 5);
    expect(bar(p, d.rowOf(4))[22]).toBe(1);
    expect(bar(p, d.rowOf(FREE_KEY))[22]).toBeCloseTo(0.55, 5);
    expect(bar(p, d.rowOf(SMALL_KEY))[19]).toBe(Pattern.DOTS);
    expect(bar(p, d.rowOf(2))[19]).toBe(Pattern.NONE);
    for (const t of p.texts.filter((t) => t.text === "a")) expect(t.alpha).toBeCloseTo(T.dimOpacity, 5);
  });
});

describe("paintFlame labels", () => {
  it("writes labels only where the backend set the name bit", () => {
    const tree = treeOf([
      { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "named", label: Label.NAME },
      { key: 3, node: 3, kind: Kind.FILE, size: 300n, name: "unnamed", label: 0 },
      { key: SMALL_KEY, node: 1, kind: Kind.SMALL, size: 100n, count: 12, label: 0 },
      { key: SMALL_KEY + 1, node: 1, kind: Kind.MORE, size: 100n, count: 7, label: Label.NAME },
    ]);
    const f = rectScene({ layout: "flame.v1", tree }).frame;
    const texts = paintFlame(f, settings()).texts.map((t) => t.text);
    expect(texts).toEqual(["root", "named", "+#7 项"]);
  });

  it("writes the size line only with the size bit, and one centred line for short bars", () => {
    const tree = treeOf([
      { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "two", label: Label.NAME | Label.SIZE },
      { key: 3, node: 3, kind: Kind.FILE, size: 300n, name: "short", label: Label.NAME | Label.SHORT },
      { key: 7, node: 7, kind: Kind.PENDING, size: 200n, name: "cache", label: Label.NAME | Label.SIZE },
    ]);
    const f = rectScene({ layout: "flame.v1", tree }).frame;
    const p = paintFlame(f, settings());
    const at = (text: string) => p.texts.find((t) => t.text === text)!;
    const two = f.rowOf(2);
    expect(at("two").x).toBe(f.x[two]! + 6);
    expect(at("400 B").y).toBeGreaterThan(at("two").y);
    expect(at("400 B").font).toMatch(/^400 10\.5px .*Mono/);
    const short = f.rowOf(3);
    expect(at("short").y).toBeCloseTo(f.y[short]! + f.h[short]! / 2 + 10.5 * 0.36, 5);
    expect(p.texts.map((t) => t.text)).not.toContain("300 B");
    expect(at("等待扫描…").y).toBeGreaterThan(at("cache").y);
    expect(at("cache").font.startsWith("600 ")).toBe(true);
  });

  it("bolds folders the filter hit, in the bar's contrasting colour", () => {
    const tree = treeOf([
      { key: 3, node: 3, kind: Kind.FOLDER, size: 500n, name: "hit", flags: Flag.FOLDER_HIT, children: [{ key: 4, node: 4, kind: Kind.FILE, size: 500n, name: "x" }] },
      { key: 5, node: 5, kind: Kind.FOLDER, size: 400n, name: "plain", children: [{ key: 6, node: 6, kind: Kind.FILE, size: 400n, name: "y" }] },
    ]);
    const f = rectScene({ layout: "flame.v1", tree }).frame;
    const texts = paintFlame(f, settings()).texts;
    expect(texts.find((t) => t.text === "hit")!.font.startsWith("700 ")).toBe(true);
    expect(texts.find((t) => t.text === "plain")!.font.startsWith("600 ")).toBe(true);
    expect(texts.find((t) => t.text === "x")!.font.startsWith("500 ")).toBe(true);
  });

  it("names the root bar by the last crumb, in bold, with its size", () => {
    const { frame: f } = rectScene({ layout: "flame.v1", info: { breadcrumb: [{ key: 0, label: "C:\\" }, { key: 1, label: "C:\\Users\\lenny" }] } });
    const root = paintFlame(f, settings()).texts[0]!;
    expect([root.text, root.sub?.text, root.color, root.sub?.color]).toEqual(["lenny", "1000 B", CENTER.text, CENTER.sub]);
    expect(root.font.startsWith("700 ")).toBe(true);
    expect(root.y).toBeCloseTo(169 + 31 / 2 + 11 * 0.36, 5);
    const small = rectScene({ layout: "flame.v1", info: { breadcrumb: [{ key: 1, label: "C:\\" }, { key: SMALL_KEY, label: "small:42" }] } }).frame;
    expect(paintFlame(small, settings()).texts[0]!.text).toBe("#42 个小项");
  });

  it("places tag markers and compare badges at the geometry's anchors", () => {
    const tree = treeOf([
      { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "a", tag: 2, dstat: DStat.NEW, delta: 400n },
      { key: 3, node: 3, kind: Kind.FILE, size: 400n, name: "b", dstat: DStat.DELETED, delta: -400n },
    ]);
    const tall = rectScene({ layout: "flame.v1", tree, width: 400, height: 600 }).frame;
    const p = paintFlame(tall, settings({ diffTiers: [1, 10, 100] }));
    const a = tall.rowOf(2);
    const b = tall.rowOf(3);
    expect(p.tags).toEqual([
      { ...flameGeometry.anchor(tall, a, "tag")!, tag: 2, alpha: 1 },
      { x: tall.x[a]! + 6, y: tall.y[a]! + tall.h[a]! - 21, tag: 0, alpha: 1, badge: "new" },
      { x: tall.x[b]! + 6, y: tall.y[b]! + tall.h[b]! - 21, tag: 0, alpha: 1, badge: "deleted" },
    ]);
    // 31 px bars carry the tag but no badge
    const short = rectScene({ layout: "flame.v1", tree }).frame;
    expect(paintFlame(short, settings()).tags.map((t) => t.badge ?? t.tag)).toEqual([2]);
  });

  it("scales type with the font scale and the device pixel ratio", () => {
    const { frame: f } = rectScene({ layout: "flame.v1", dpr: 2, width: 800, height: 400 });
    const p = paintFlame(f, settings({ fontScale: 1.25 }));
    const name = p.texts.find((t) => t.text === "a.mp4")!;
    expect(name.font).toMatch(/^500 27\.5px /);
    expect(name.x).toBe(f.x[f.rowOf(2)]! + 12);
  });
});

describe("selection, halo and hover", () => {
  const { frame: f } = rectScene({ layout: "flame.v1" });
  const sel = f.rowOf(2); // a.mp4, depth 1, selected
  const deep = f.rowOf(6); // x.pdf under sub / docs

  it("finds the selection by FrameInfo.selected_row", () => {
    expect(selectedOf(f)).toBe(sel);
    expect(selectedOf(rectScene({ layout: "flame.v1", info: { selected_row: null } }).frame)).toBe(-1);
    expect(selectedOf(rectScene({ layout: "flame.v1", info: { selected_row: 3 } }).frame)).toBe(-1); // not flagged
  });

  it("lights ancestors up to the level limit, the root bar as the last one", () => {
    expect(haloOf(f, deep, 3)).toEqual({ rows: [f.rowOf(5), f.rowOf(3)], root: true });
    expect(haloOf(f, deep, 2)).toEqual({ rows: [f.rowOf(5), f.rowOf(3)], root: false });
    expect(haloOf(f, deep, 1)).toEqual({ rows: [f.rowOf(5)], root: false });
    expect(haloOf(f, deep, 0)).toEqual({ rows: [], root: false });
    expect(haloOf(f, sel, Infinity)).toEqual({ rows: [], root: true });
    expect(haloOf(f, HIT_ROOT, 3)).toEqual({ rows: [], root: false });
    expect(haloOf(f, HIT_NONE, 3)).toEqual({ rows: [], root: false });
  });

  it("lights the selection's ancestors in the picture: here only the root bar", () => {
    expectRGBA(slot(paintFlame(f, settings()), 0).subarray(4, 8), lighten(CENTER.bg));
    expectRGBA(slot(paintFlame(f, settings({ haloLevels: 0 })), 0).subarray(4, 8), CENTER.bg);
    const under = rectScene({ layout: "flame.v1", tree: treeOf(SCENE_TREE.children!.map((c) => ({ ...c, flags: c.key === 2 ? 0 : c.flags }))), info: { selected_row: 4 } });
    under.frame.flags[4]! |= Flag.SELECTED;
    const p = paintFlame(under.frame, settings());
    expect(under.frame.key[4]).toBe(6);
    expectRGBA(bar(p, under.frame.rowOf(5)).subarray(4, 8), lighten(branchFill(under.frame.branch![under.frame.rowOf(5)]!, 2)));
    expectRGBA(bar(p, under.frame.rowOf(3)).subarray(4, 8), lighten(branchFill(under.frame.branch![under.frame.rowOf(3)]!, 1)));
  });

  it("overlays the hovered row's halo, its inset, and the selection on top", () => {
    const s = settings();
    const p = paintFlameOverlay(f, s, deep);
    // docs and sub halos (the root is lit by the selection already), shadow, the bar again, hover inset, two rings
    expect(p.count).toBe(2 + SELECTION_INSTANCES + 1);
    const docs = f.rowOf(5);
    expect(Array.from(slot(p, 0).subarray(0, 4))).toEqual([f.x[docs], f.y[docs], f.w[docs], f.h[docs]]);
    expectRGBA(slot(p, 0).subarray(4, 8), HALO_OVERLAY);
    expect(slot(p, 2)[19]).toBe(Pattern.SHADOW);
    expect(Array.from(slot(p, 3))).toEqual(Array.from(bar(paintFlame(f, s), sel)));
    const inset = slot(p, 4);
    expect(Array.from(inset.subarray(0, 4))).toEqual([f.x[deep], f.y[deep], f.w[deep], f.h[deep]]);
    expectRGBA(inset.subarray(8, 12), rgba(T.state.hoverInset));
    const r = T.geometry.selectRingWidth;
    const ring = slot(p, p.count - 1);
    expect(Array.from(ring.subarray(0, 4))).toEqual([f.x[sel]! - r, f.y[sel]! - r, f.w[sel]! + 2 * r, f.h[sel]! + 2 * r]);
    expectRGBA(ring.subarray(8, 12), rgba(T.state.selectRing));
    expectRGBA(slot(p, p.count - 2).subarray(8, 12), rgba(T.state.selectOuter));
  });

  it("lights the root bar for a hover when the selection does not", () => {
    const quiet = rectScene({ layout: "flame.v1", info: { selected_row: null } }).frame;
    const p = paintFlameOverlay(quiet, settings(), quiet.rowOf(4)); // 中文.txt under sub: sub and the root
    expect(p.count).toBe(3);
    expect(Array.from(slot(p, 1).subarray(0, 4))).toEqual([0, 169, 400, 31]);
    expectRGBA(slot(p, 1).subarray(4, 8), HALO_OVERLAY);
  });

  it("insets the root bar on hover, and nothing with hover highlight off", () => {
    const quiet = rectScene({ layout: "flame.v1", info: { selected_row: null } }).frame;
    const p = paintFlameOverlay(quiet, settings(), HIT_ROOT);
    expect(p.count).toBe(1);
    expect(Array.from(slot(p, 0).subarray(0, 4))).toEqual([0, 169, 400, 31]);
    expect(paintFlameOverlay(quiet, settings({ hoverHighlight: false }), HIT_ROOT).count).toBe(0);
    expect(paintFlameOverlay(quiet, settings(), HIT_NONE).count).toBe(0);
  });

  it("drops the shadow and the repeated bar without the selection shadow", () => {
    const p = paintFlameOverlay(f, settings({ selectionShadow: false }), HIT_NONE);
    expect(p.count).toBe(2);
    expect(slot(p, 0)[19]).not.toBe(Pattern.SHADOW);
  });
});

describe("zoom transform", () => {
  const { frame: f } = rectScene({ layout: "flame.v1" });
  const row = f.rowOf(3); // sub
  const apply = (m: { sx: number; sy: number; tx: number; ty: number }, x: number, y: number) => [x * m.sx + m.tx, y * m.sy + m.ty];

  it("lays the zoomed bar over the root bar at the end, from identity at the start", () => {
    const end = zoomXform(f, { row, t: 1 }, false)!;
    expect(end.sy).toBe(1);
    expect(apply(end, f.x[row]!, f.y[row]!)).toEqual([0, 169]);
    expect(apply(end, f.x[row]! + f.w[row]!, f.y[row]! + f.h[row]!)[0]).toBeCloseTo(400, 9);
    // its children land in band 1
    const kid = f.rowOf(4);
    expect(apply(end, 0, f.y[kid]!)[1]).toBe(136);
    const start = zoomXform(f, { row, t: 0 }, false)!;
    for (const [x, y] of [
      [0, 0],
      [123, 45],
    ] as const) {
      const [px, py] = apply(start, x, y);
      expect(px).toBeCloseTo(x, 9);
      expect(py).toBeCloseTo(y, 9);
    }
    const half = zoomXform(f, { row, t: 0.5 }, false)!;
    expect(half.sx).toBeCloseTo(1 + (end.sx - 1) / 2, 9);
    expect(half.ty).toBeCloseTo(end.ty / 2, 9);
  });

  it("is null without a zoom, under reduced motion, and for rows the frame does not have", () => {
    expect(zoomXform(f, null, false)).toBeNull();
    expect(zoomXform(f, { row, t: 0.5 }, true)).toBeNull();
    expect(zoomXform(f, { row: f.count, t: 0.5 }, false)).toBeNull();
    expect(zoomXform(f, { row: HIT_ROOT, t: 0.5 }, false)).toBeNull();
  });

  it("moves label anchors with the bars but keeps the type's size", () => {
    const m = zoomXform(f, { row, t: 1 }, false)!;
    const item: PreparedText = { x: 10, y: 20, text: "n", font: "500 11px x", color: "#fff", maxWidth: 50, alpha: 1, shown: "n", subShown: "", subX: 0 };
    const header: PreparedText = { ...item, sub: { text: "1 B", font: "400 10px y", color: "#ccc" }, subShown: "1 B", subX: 30 };
    const [a, h] = moveTexts([item, header], m);
    expect([a!.x, a!.y]).toEqual(apply(m, 10, 20));
    expect([a!.font, a!.maxWidth]).toEqual(["500 11px x", 50]);
    expect(h!.subX - h!.x).toBe(20);
    expect(moveTags([{ x: 5, y: 6, tag: 1, alpha: 1 }], m)).toEqual([{ x: apply(m, 5, 6)[0], y: apply(m, 5, 6)[1], tag: 1, alpha: 1 }]);
  });
});

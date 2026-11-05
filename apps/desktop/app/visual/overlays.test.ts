import { describe, expect, it } from "vitest";

import { Flag, Kind } from "./frame";
import { badgeAt, breathAlpha, breathingRows, CLASS_FADE_MS, classFade, classMarks, drawBreath, drawClassMark, drawFlash, drawTrail, flashOf, glowOf, trailMark } from "./overlays";
import { encodeFrame, PathRecorder, polarScene, rectScene } from "./testing/fixtures";
import { treemapGeometry } from "./treemap/geometry";
import { type Box, HIT_ROOT, type RectFrame, type VisualGeometry } from "./types";

const recorder = () => new PathRecorder() as unknown as Path2D;
const opsOf = (p: Path2D) => (p as unknown as PathRecorder).ops;

/** A 2D context that keeps what was stroked and filled, with the alpha at that moment. */
function canvas() {
  const calls: { op: string; args: unknown[]; alpha: number; lineWidth: number }[] = [];
  const ctx = {
    globalAlpha: 1,
    lineWidth: 1,
    strokeStyle: "",
    fillStyle: "",
    shadowColor: "",
    shadowBlur: 0,
    font: "",
    textBaseline: "alphabetic",
    save() {},
    restore() {},
    stroke(path: Path2D) {
      calls.push({ op: "stroke", args: [path], alpha: ctx.globalAlpha, lineWidth: ctx.lineWidth });
    },
    fillRect(...args: number[]) {
      calls.push({ op: "fillRect", args, alpha: ctx.globalAlpha, lineWidth: ctx.lineWidth });
    },
    fillText(...args: unknown[]) {
      calls.push({ op: "fillText", args, alpha: ctx.globalAlpha, lineWidth: ctx.lineWidth });
    },
    measureText: (text: string) => ({ width: text.length * 6 }),
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, calls };
}

/** A frame of blocks in a row, 10 px apart (x is the index × 10). */
function blocks(rows: { flags: number; depth: number; w: number; h: number }[]): RectFrame {
  return encodeFrame<RectFrame>(
    "drawlist.v1",
    rows.map((r, i) => ({ key: i + 1, node: i + 1, parent: -1, kind: Kind.FILE, depth: r.depth, geo: [i * 10, 0, r.w, r.h] as const, size: 1n, flags: r.flags })),
    { width: 4000, height: 400 },
  ).frame;
}

describe("folders being scanned", () => {
  it("keeps the marked blocks big enough for an outline", () => {
    const f = blocks([
      { flags: Flag.SCANNING, depth: 1, w: 400, h: 300 },
      { flags: Flag.SCANNING | Flag.SELECTED, depth: 2, w: 100, h: 80 },
      { flags: Flag.SELECTED, depth: 1, w: 400, h: 300 }, // not being scanned
      { flags: Flag.SCANNING, depth: 3, w: 20, h: 8 }, // too thin at dpr 1
    ]);
    expect(breathingRows(treemapGeometry, f, 1, recorder).map((b) => b.depth)).toEqual([1, 2]);
    expect(breathingRows(treemapGeometry, f, 2, recorder).map((b) => b.depth)).toEqual([1, 2]);
    expect(breathingRows(treemapGeometry, f, 7, recorder).map((b) => b.depth)).toEqual([1]); // 80 < 12 × 7
  });

  it("outlines each block half its line width inside, through the geometry's path", () => {
    const f = blocks([
      { flags: Flag.SCANNING, depth: 1, w: 400, h: 300 },
      { flags: Flag.SCANNING, depth: 2, w: 100, h: 80 },
    ]);
    const [outer, inner] = breathingRows(treemapGeometry, f, 1, recorder);
    expect(outer!.lineWidth).toBe(2);
    expect(opsOf(outer!.path)).toEqual([{ op: "rect", args: [1, 1, 398, 298] }]);
    expect(inner!.lineWidth).toBe(1.5);
    expect(opsOf(inner!.path)).toEqual([{ op: "rect", args: [10.75, 0.75, 98.5, 78.5] }]);
  });

  it("keeps at most 300 blocks, the largest first", () => {
    const f = blocks(Array.from({ length: 320 }, (_, i) => ({ flags: Flag.SCANNING, depth: 1, w: 20 + i, h: 20 })));
    const rows = breathingRows(treemapGeometry, f, 1, recorder);
    expect(rows).toHaveLength(300);
    expect(rows[0]!.hit).toBe(319);
    expect(rows[299]!.hit).toBe(20);
    // fewer than the limit: frame order, untouched
    const few = breathingRows(treemapGeometry, blocks([20, 90, 40].map((w) => ({ flags: Flag.SCANNING, depth: 1, w, h: 20 }))), 1, recorder);
    expect(few.map((b) => b.hit)).toEqual([0, 1, 2]);
  });

  it("lets the zoom root breathe when its own flags say it is being scanned", () => {
    const geometry: VisualGeometry<RectFrame> = {
      ...treemapGeometry,
      box: (f, hit) => (hit === HIT_ROOT ? { major: 400, minor: 30 } : treemapGeometry.box(f, hit)),
      path: (f, hit, inset, out) => (hit === HIT_ROOT ? (out.rect(inset, inset, 400 - 2 * inset, 30 - 2 * inset), true) : treemapGeometry.path(f, hit, inset, out)),
    };
    const scanning = rectScene({ layout: "flame.v1" }).frame;
    const marked = { ...scanning, info: { ...scanning.info, root: { ...scanning.info.root!, flags: Flag.SCANNING } } };
    expect(breathingRows(geometry, marked, 1, recorder)[0]!.hit).toBe(HIT_ROOT);
    expect(breathingRows(geometry, scanning, 1, recorder).some((b) => b.hit === HIT_ROOT)).toBe(false);
  });

  it("breathes, outer levels brighter, and holds still under reduced motion", () => {
    const samples = Array.from({ length: 48 }, (_, i) => breathAlpha(1, i * 50, false));
    expect(Math.max(...samples) - Math.min(...samples)).toBeGreaterThan(0.5);
    expect(breathAlpha(1, 1000, true)).toBe(breathAlpha(1, 2000, true));
    expect(breathAlpha(1, 0, true)).toBeGreaterThan(breathAlpha(2, 0, true));
    expect(breathAlpha(2, 0, true)).toBeGreaterThan(breathAlpha(3, 0, true));
    for (const a of samples) expect(a).toBeGreaterThanOrEqual(0), expect(a).toBeLessThanOrEqual(1);
  });

  it("strokes every outline it was given, the same under reduced motion at any time", () => {
    const f = blocks([{ flags: Flag.SCANNING, depth: 1, w: 400, h: 300 }]);
    const rows = breathingRows(treemapGeometry, f, 1, recorder);
    const a = canvas();
    drawBreath(a.ctx, rows, 1000, true, 1);
    const b = canvas();
    drawBreath(b.ctx, rows, 5000, true, 1);
    expect(a.calls).toHaveLength(1);
    expect(a.calls[0]!.args[0]).toBe(rows[0]!.path);
    expect(a.calls[0]!.alpha).toBe(b.calls[0]!.alpha);
    expect(a.calls[0]!.lineWidth).toBe(2);
  });

  it("outlines sectors of the polar flame graph too", () => {
    const p = polarScene();
    const geometry: VisualGeometry<typeof p.frame> = {
      hitTest: () => -1,
      bounds: () => null,
      anchor: () => null,
      box: () => ({ major: 100, minor: 20 }),
      path: (_f, _hit, _inset, out) => (out.arc(200, 200, 80, 0, 1), true),
      filled: () => true,
    };
    const marked = Array.from({ length: p.frame.count }, (_, i) => i).filter((i) => p.frame.flags[i]! & Flag.SCANNING);
    expect(breathingRows(geometry, p.frame, 1, recorder).map((b) => b.hit)).toEqual(marked);
  });
});

describe("file changes", () => {
  const f = rectScene().frame;

  it("outlines each changed row one device pixel inside; row -1 is the zoom root", () => {
    const flash = flashOf(treemapGeometry, f, [{ row: 1, delta: 4096 }, { row: -1, delta: 1 }], 100, recorder)!;
    // the treemap has no zoom root to outline: only the block
    expect(flash.marks).toHaveLength(1);
    expect(opsOf(flash.marks[0]!.path)).toEqual([{ op: "rect", args: [3, 21, 98, 76] }]);
    expect(flash.marks[0]!.badge).toEqual({ x: 102, y: 98 });
    expect(flash.start).toBe(100);
    expect(flashOf(treemapGeometry, f, [], 0, recorder)).toBeNull();
  });

  it("pulses twice and fades out, then stops", () => {
    const flash = flashOf(treemapGeometry, f, [{ row: 1, delta: 4096 }], 0, recorder)!;
    const c = canvas();
    expect(drawFlash(c.ctx, flash, 100, false, 1)).toBe(true);
    expect(c.calls.map((x) => x.op)).toEqual(["stroke"]);
    expect(drawFlash(canvas().ctx, flash, 60_000, false, 1)).toBe(false);
  });

  it("shows the change as a static badge under reduced motion, in the block's bottom-right corner", () => {
    const flash = flashOf(treemapGeometry, f, [{ row: 1, delta: -2048 }], 0, recorder)!;
    const c = canvas();
    expect(drawFlash(c.ctx, flash, 100, true, 1)).toBe(true);
    const rect = c.calls.find((x) => x.op === "fillRect")!;
    const [x, y, w, h] = rect.args as number[];
    // the block is 2,20 100 × 78: the badge ends 2 px inside its right and bottom edges
    expect(x! + w!).toBe(100);
    expect(y! + h!).toBe(96);
    expect(String(c.calls.find((x) => x.op === "fillText")!.args[0]).startsWith("−")).toBe(true);
    expect(drawFlash(canvas().ctx, flash, 20_000, true, 1)).toBe(false);
  });

  it("leaves the static badge out where it does not fit", () => {
    const tiny = encodeFrame<RectFrame>("drawlist.v1", [{ key: 1, node: 1, parent: -1, kind: Kind.FILE, depth: 1, geo: [0, 0, 30, 30], size: 1n }]).frame;
    const flash = flashOf(treemapGeometry, tiny, [{ row: 0, delta: 1 << 30 }], 0, recorder)!;
    const c = canvas();
    expect(drawFlash(c.ctx, flash, 100, true, 1)).toBe(true);
    expect(c.calls).toEqual([]);
  });

  it("centres a badge on its anchor, pulled back inside the bounds", () => {
    const b: Box = { x: 0, y: 0, w: 100, h: 50 };
    expect(badgeAt({ x: 100, y: 50 }, b, 40, 16, 2)).toEqual({ x: 58, y: 32 }); // a corner anchor
    expect(badgeAt({ x: 26, y: 37 }, b, 40, 16, 2)).toEqual({ x: 6, y: 29 }); // an anchor whose badge fits
    expect(badgeAt({ x: -20, y: -20 }, b, 40, 16, 2)).toEqual({ x: 2, y: 2 });
    expect(badgeAt({ x: 0, y: 0 }, b, 97, 16, 2)).toBeNull();
    expect(badgeAt({ x: 0, y: 0 }, b, 40, 47, 2)).toBeNull();
  });
});

describe("the pointer trail", () => {
  const f = rectScene({ dpr: 2 }).frame;

  it("outlines the row left half a device pixel inside, and fades it out", () => {
    const mark = trailMark(treemapGeometry, f, 1, 0, recorder)!;
    expect(opsOf(mark.path)).toEqual([{ op: "rect", args: [3, 21, 98, 76] }]);
    const c = canvas();
    expect(drawTrail(c.ctx, [mark], 100, 2)).toEqual([mark]);
    expect(c.calls[0]!.alpha).toBeCloseTo(0.6 * 0.8);
    expect(c.calls[0]!.lineWidth).toBe(2);
    expect(drawTrail(canvas().ctx, [mark], 600, 2)).toEqual([]);
    expect(trailMark(treemapGeometry, f, HIT_ROOT, 0, recorder)).toBeNull();
  });
});

describe("the class lit from 「类别统计」 (UI-MAIN-65)", () => {
  const V = 0; // the class hovered
  const NONE = 255; // the backend's "in no one class"
  type R = { kind: number; parent: number; cls: number; geo: readonly [number, number, number, number] };
  const frameOf = (layout: string, rows: R[]) =>
    encodeFrame<RectFrame>(
      layout,
      rows.map((r, i) => ({ key: i + 1, node: i + 1, parent: r.parent, kind: r.kind, depth: 1, geo: r.geo, cls: r.cls, size: 1n })),
      { width: 600, height: 200 },
    ).frame;
  //   0 a folder the class swallows (a game) ├ 1 its file
  //   2 a folder in no class ├ 3 file · 4 aggregate of the class · 5 mixed aggregate · 6 "+N" · 8 tiny file
  //   7 free space
  const ROWS: R[] = [
    { kind: Kind.FOLDER, parent: -1, cls: V, geo: [0, 0, 200, 100] },
    { kind: Kind.FILE, parent: 0, cls: V, geo: [2, 20, 90, 70] },
    { kind: Kind.FOLDER, parent: -1, cls: NONE, geo: [200, 0, 200, 100] },
    { kind: Kind.FILE, parent: 2, cls: V, geo: [202, 20, 90, 70] },
    { kind: Kind.SMALL, parent: 2, cls: V, geo: [300, 20, 90, 30] },
    { kind: Kind.SMALL, parent: 2, cls: NONE, geo: [300, 60, 90, 30] },
    { kind: Kind.MORE, parent: 2, cls: V, geo: [300, 92, 90, 6] },
    { kind: Kind.FREE, parent: -1, cls: V, geo: [400, 0, 60, 100] },
    { kind: Kind.FILE, parent: 2, cls: V, geo: [202, 92, 3, 3] },
  ];
  const rects = (p: Path2D) => opsOf(p).map((o) => o.args);

  it("lights the class's files, aggregates and swallowed folders; a lit folder holds its blocks", () => {
    const m = classMarks(treemapGeometry, frameOf("drawlist.v1", ROWS), V, -1, 1, recorder);
    // row 1 lies inside the lit row 0; mixed aggregates, "+N" and free space never light
    expect(rects(m.cut)).toEqual([
      [0, 0, 200, 100],
      [202, 20, 90, 70],
      [300, 20, 90, 30],
      [202, 92, 3, 3],
    ]);
    expect(m.rows).toBe(4);
    expect(m.outlined).toBe(3); // the 3 px block is too small for an outline
    expect(rects(m.outline)[0]).toEqual([0.75, 0.75, 198.5, 98.5]);
    expect(classMarks(treemapGeometry, frameOf("drawlist.v1", ROWS), 3, -1, 1, recorder).rows).toBe(0);
  });

  it("stays within the folder whose make-up the panel shows", () => {
    const m = classMarks(treemapGeometry, frameOf("drawlist.v1", ROWS), V, 2, 1, recorder);
    expect(rects(m.cut)).toEqual([
      [202, 20, 90, 70],
      [300, 20, 90, 30],
      [202, 92, 3, 3],
    ]);
    // a collapsed folder shows nothing of what is in it: it lights itself
    const collapsed = frameOf("drawlist.v1", [{ kind: Kind.COLLAPSED, parent: -1, cls: NONE, geo: [0, 0, 50, 50] }, ROWS[2]!]);
    expect(rects(classMarks(treemapGeometry, collapsed, V, 0, 1, recorder).cut)).toEqual([[0, 0, 50, 50]]);
    expect(classMarks(treemapGeometry, collapsed, V, 1, 1, recorder).rows).toBe(0);
  });

  it("lights every row of the class in the flame graphs, through the geometry's path", () => {
    const hits: number[] = [];
    const geometry: VisualGeometry<RectFrame> = {
      hitTest: () => -1,
      bounds: () => null,
      anchor: () => null,
      box: () => ({ major: 100, minor: 20 }),
      path: (_f, hit, inset, out) => (inset === 0 && hits.push(hit as number), out.arc(200, 200, 80, 0, 1), true),
      filled: () => true,
    };
    const m = classMarks(geometry, frameOf("flame.v1", ROWS), V, -1, 1, recorder);
    expect(hits).toEqual([0, 1, 3, 4, 8]); // the bars of a folder's contents lie beyond it
    expect(m.outlined).toBe(5);
  });

  it("veils the picture, cuts the class out of it, and outlines it with the class's glow", () => {
    const calls: { op: string; alpha: number; comp: string }[] = [];
    const ctx = {
      globalAlpha: 1,
      globalCompositeOperation: "source-over",
      lineWidth: 1,
      strokeStyle: "",
      fillStyle: "",
      shadowColor: "",
      shadowBlur: 0,
      save() {},
      restore() {},
      fillRect() {
        calls.push({ op: "veil", alpha: ctx.globalAlpha, comp: ctx.globalCompositeOperation });
      },
      fill() {
        calls.push({ op: "cut", alpha: ctx.globalAlpha, comp: ctx.globalCompositeOperation });
      },
      stroke() {
        calls.push({ op: "outline", alpha: ctx.globalAlpha, comp: ctx.globalCompositeOperation });
      },
    };
    const m = classMarks(treemapGeometry, frameOf("drawlist.v1", ROWS), V, -1, 2, recorder);
    drawClassMark(ctx as unknown as CanvasRenderingContext2D, m, "#E07A1F", 0.5, 600, 200, 2);
    expect(calls).toEqual([
      { op: "veil", alpha: 0.5, comp: "source-over" },
      { op: "cut", alpha: 1, comp: "destination-out" },
      { op: "outline", alpha: 0.5, comp: "source-over" },
    ]);
    expect(ctx.shadowColor).toBe("#E07A1F");
    expect(ctx.lineWidth).toBe(2);
    calls.length = 0;
    drawClassMark(ctx as unknown as CanvasRenderingContext2D, m, "#E07A1F", 0, 600, 200, 2);
    expect(calls).toEqual([]);
  });

  it("glows in the class colour, in the accent for a class too dark to glow", () => {
    expect(glowOf("#E07A1F")).toBe("#E07A1F");
    expect(glowOf("#2B2F38")).not.toBe("#2B2F38");
    expect(glowOf("not a colour")).toBe(glowOf("#2B2F38"));
  });

  it("fades in and out in CLASS_FADE_MS, at once under reduced motion", () => {
    expect(classFade(0, true, CLASS_FADE_MS / 2, false)).toBeCloseTo(0.5);
    expect(classFade(0.5, true, CLASS_FADE_MS, false)).toBe(1);
    expect(classFade(1, false, CLASS_FADE_MS / 4, false)).toBeCloseTo(0.75);
    expect(classFade(0.2, false, CLASS_FADE_MS, false)).toBe(0);
    expect(classFade(0, true, 0, true)).toBe(1);
    expect(classFade(1, false, 0, true)).toBe(0);
  });
});

// shapes.v1: decoding (the points blob), the geometry against the conformance checks and by hand,
// the paint (instances, labels, overlay) and the component against a recording renderer.

import { describe, expect, it } from "vitest";

import { decodeVisualFrame, Flag, isNested, isShapesFrame, Label } from "../frame";
import { checkGeometry } from "../testing/conformance";
import { encodeBinary, frameInfo, PathRecorder, rowColumns, shapesScene } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type PaintSettings, type VisualContext } from "../types";
import { shapesComponent } from "./component";
import { bandWidth, contains, insetPolygon, outline, shapeOf, shapesGeometry } from "./geometry";
import { type ShapePicture, type ShapeRenderer, SHAPE_STRIDE, SLOT } from "./gl";
import { labelOf, paintShapes, paintShapesOverlay, SELECTION_INSTANCES, zoomXform } from "./paint";
import { decodeFrame } from "@/protocol/rpc";

const settings = (patch: Partial<PaintSettings> = {}): PaintSettings => ({
  colorMode: "class",
  classIds: ["other", "video", "text", "doc"],
  classColors: [],
  scanning: false,
  formatSize: (b) => `${b} B`,
  formatInt: (n) => `${n}`,
  formatDelta: (d) => `${d} B`,
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

describe("decoding", () => {
  it("reads the polygons' points and where each row's start", () => {
    const f = shapesScene().frame;
    expect(isShapesFrame(f)).toBe(true);
    expect(Array.from(f.poly)).toEqual([0, 0, 6, 0, 0]);
    expect(Array.from(f.pointStart)).toEqual([0, 0, 0, 6, 6, 6]);
    expect(f.points.length).toBe(12);
    expect(isNested(f)).toBe(true);
    expect(isNested(shapesScene({ nested: false }).frame)).toBe(false);
  });

  it("refuses a frame whose points do not add up", () => {
    const { columns, names } = rowColumns("shapes.v1", [{ key: 1, node: 1, parent: -1, kind: 0, depth: 1, geo: [0, 0, 10, 10, 0, 5], size: 1n }]);
    const info = frameInfo({ layout: "shapes.v1", rows: 1 });
    const bin = decodeFrame(encodeBinary("shapes.v1", columns, { names, points: new Uint8Array(8) }));
    expect(decodeVisualFrame(bin, info)).toBeNull();
  });
});

describe("geometry", () => {
  it("conforms on the scene, with and without a root shape, at dpr 1 and 2", () => {
    expect(checkGeometry(shapesComponent, shapesScene().frame)).toEqual([]);
    expect(checkGeometry(shapesComponent, shapesScene({ root: false }).frame)).toEqual([]);
    expect(checkGeometry(shapesComponent, shapesScene({ dpr: 2, nested: false, root: false }).frame)).toEqual([]);
  });

  it("hits the last shape painted over the point, then the root, then nothing", () => {
    const f = shapesScene().frame;
    expect(shapesGeometry.hitTest(f, 110, 150)).toBe(1); // a.mp4 inside sub
    expect(shapesGeometry.hitTest(f, 140, 220)).toBe(0); // sub, below its files
    expect(shapesGeometry.hitTest(f, 180, 150)).toBe(2); // the hexagon
    expect(shapesGeometry.hitTest(f, 250, 170)).toBe(HIT_ROOT); // between the shapes: the root circle
    expect(shapesGeometry.hitTest(f, 62, 12)).toBe(HIT_NONE); // the root's box corner is not the circle
    expect(shapesGeometry.hitTest(shapesScene({ root: false }).frame, 250, 170)).toBe(HIT_NONE);
  });

  it("tests circles, rounded corners and polygons exactly", () => {
    const circle = { x: 0, y: 0, w: 40, h: 40, r: 20, pts: null };
    expect(contains(circle, 20, 20)).toBe(true);
    expect(contains(circle, 2, 2)).toBe(false);
    const tri = { x: 0, y: 0, w: 40, h: 40, r: 0, pts: [0, 0, 40, 0, 0, 40] };
    expect(contains(tri, 5, 5)).toBe(true);
    expect(contains(tri, 35, 35)).toBe(false);
  });

  it("insets polygons along their edges and rounded rectangles with their radius", () => {
    const square = insetPolygon([0, 0, 10, 0, 10, 10, 0, 10], 2)!;
    expect(square.map((v) => Math.round(v * 1e6) / 1e6)).toEqual([2, 2, 8, 2, 8, 8, 2, 8]);
    expect(insetPolygon([0, 0, 10, 0, 10, 10, 0, 10], 6)).toBeNull(); // shrunk past a point
    const grown = insetPolygon([0, 0, 0, 10, 10, 10, 10, 0], -1)!; // the other orientation, grown
    expect(Math.min(...grown)).toBeCloseTo(-1);
    const rec = new PathRecorder();
    expect(outline({ x: 0, y: 0, w: 40, h: 40, r: 20, pts: null }, 2, rec)).toBe(true);
    for (const p of rec.points()) expect(Math.hypot(p.x - 20, p.y - 20)).toBeLessThanOrEqual(18.01);
  });

  it("measures a shape's width across a band as the backend does", () => {
    const circle = { x: 0, y: 0, w: 100, h: 100, r: 50, pts: null };
    expect(bandWidth(circle, 10, 20)).toBeCloseTo(60, 0);
    expect(bandWidth(circle, 0, 10)).toBe(0);
    expect(bandWidth({ ...circle, r: 0 }, 0, 10)).toBe(100);
  });
});

describe("paint", () => {
  it("draws the root's shape, then one instance per row, polygons with their points", () => {
    const f = shapesScene().frame;
    const p = paintShapes(f, settings());
    expect(p.count).toBe(1 + f.count);
    const at = (k: number, slot: number) => p.instances[k * SHAPE_STRIDE + slot]!;
    expect([at(0, SLOT.rect), at(0, SLOT.rect + 2), at(0, SLOT.shape)]).toEqual([60, 280, 140]); // the root circle
    expect(at(3, SLOT.extra + 3)).toBe(6); // the hexagon: six points, around its box's centre
    const hex = shapeOf(f, 2)!;
    expect(at(3, SLOT.points)).toBeCloseTo(hex.pts![0]! - (hex.x + hex.w / 2));
  });

  it("centres the labels of round shapes and polygons, and names the root at its top", () => {
    const f = shapesScene().frame;
    const p = paintShapes(f, settings());
    const a = p.texts.find((t) => t.text === "a.mp4")!;
    expect(a.align).toBe("center");
    expect(a.x).toBe(110);
    expect(p.texts.find((t) => t.text === "400 B")).toBeTruthy(); // its size line
    expect(p.texts.find((t) => t.text === "x.pdf")?.align).toBe("center");
    const cache = p.texts.find((t) => t.text === "cache")!; // a rectangle: as in the flame graph
    expect(cache.align).toBeUndefined();
    expect(cache.x).toBe(246);
    const root = p.texts.find((t) => t.text === "root")!;
    expect(root.align).toBe("center");
    expect(root.y).toBeLessThan(40);
    expect(p.tags).toContainEqual(expect.objectContaining({ tag: 1 })); // a.mp4's tag
  });

  it("lays a top label out in the band at the top", () => {
    const sh = { x: 0, y: 0, w: 200, h: 200, r: 100, pts: null };
    const [item] = labelOf(sh, Label.NAME | Label.TOP, { line1: "folder", line2: null }, "1 B", { text: "#fff", sub: "#ccc", alpha: 1, lit: false, bold: true }, 1, 1);
    expect(item!.align).toBe("center");
    expect(item!.x).toBe(100);
    expect(item!.maxWidth).toBeLessThan(120); // the chord near the top of a circle
  });

  it("lights the hovered row's folder and rings the selection", () => {
    const f = shapesScene().frame;
    const o = paintShapesOverlay(f, settings(), 1);
    // the selected row (1) is hovered too: no extra halo; the selection's instances and the hover inset
    expect(o.count).toBe(SELECTION_INSTANCES + 1);
    // a top-level row lights only the root, which the selection lights already: the two rings
    const plain = paintShapesOverlay(f, settings({ selectionShadow: false, hoverHighlight: false }), 4);
    expect(plain.count).toBe(2);
  });

  it("selects an open folder of a nested layout with its rings only, so its contents stay in sight", () => {
    const pick = (f: ReturnType<typeof shapesScene>["frame"], row: number) => {
      for (let i = 0; i < f.count; i++) f.flags[i]! &= ~Flag.SELECTED;
      f.flags[row]! |= Flag.SELECTED;
      f.info.selected_row = row;
      return f;
    };
    const nested = pick(shapesScene().frame, 0); // sub: a folder whose circles lie in it
    const rings = paintShapesOverlay(nested, settings({ hoverHighlight: false }), HIT_NONE);
    expect(rings.count).toBe(2);
    // neither instance has a fill: nothing is laid over the folder's circles
    for (let k = 0; k < rings.count; k++) expect(rings.instances[k * SHAPE_STRIDE + SLOT.fill + 3]).toBe(0);
    // a file in it is lifted as before: its shadow, itself again, the rings
    const file = pick(shapesScene().frame, 1);
    expect(paintShapesOverlay(file, settings({ hoverHighlight: false }), HIT_NONE).count).toBe(SELECTION_INSTANCES);
    // a folder of a layout that puts its blocks elsewhere (an icicle) holds nothing: lifted too
    const flat = pick(shapesScene({ nested: false }).frame, 0);
    expect(paintShapesOverlay(flat, settings({ hoverHighlight: false }), HIT_NONE).count).toBe(SELECTION_INSTANCES);
  });

  it("draws a line inside every shape's edge in the canvas colour, as wide as the frame says", () => {
    const f = shapesScene({ stroke: 2 }).frame;
    const p = paintShapes(f, settings());
    const at = (k: number, slot: number) => p.instances[k * SHAPE_STRIDE + slot]!;
    const width = (k: number) => at(k, SLOT.shape + 1);
    expect(width(0)).toBe(0); // the root's shape: on the canvas already
    expect([width(1), width(2), width(3), width(5)]).toEqual([2, 2, 2, 2]); // sub, a.mp4, the hexagon, the small items
    // laid over the shape's own fill: opaque (not a see-through ring), and not the fill itself
    expect(at(2, SLOT.border + 3)).toBe(1);
    const fill = [0, 1, 2].map((c) => at(2, SLOT.fill + c));
    const line = [0, 1, 2].map((c) => at(2, SLOT.border + c));
    expect(line.some((v, c) => v !== fill[c])).toBe(true);
    // the folder waiting for the scan keeps its own dashed border
    expect(at(4, SLOT.extra)).toBe(1);
    expect(width(4)).toBe(1);
    // none when the plugin asked for none, when 「边框对比」 is 0, or on a shape too small to keep a fill
    const none = paintShapes(shapesScene({ stroke: 0 }).frame, settings());
    expect(none.instances[2 * SHAPE_STRIDE + SLOT.shape + 1]).toBe(0);
    const soft = paintShapes(f, settings({ borderContrast: 0 }));
    expect(soft.instances[2 * SHAPE_STRIDE + SLOT.shape + 1]).toBe(0);
    const thick = paintShapes(shapesScene({ stroke: 12 }).frame, settings());
    expect(thick.instances[5 * SHAPE_STRIDE + SLOT.shape + 1]).toBe(0); // the small items' 40 px circle
    expect(thick.instances[1 * SHAPE_STRIDE + SLOT.shape + 1]).toBe(12); // sub's 160 px one
  });

  it("names the root by the scanned folder's own name, not its whole path", () => {
    const f = shapesScene({ info: { breadcrumb: [{ key: 1, label: "C:\\Users\\lenny\\Downloads\\" }] } }).frame;
    const p = paintShapes(f, settings());
    expect(p.texts.map((t) => t.text)).toContain("Downloads");
    expect(p.texts.some((t) => t.text.includes("Users"))).toBe(false);
  });

  it("zooms a row's box onto the root's shape, keeping it round", () => {
    const f = shapesScene().frame;
    expect(zoomXform(f, { row: 0, t: 0 }, false)).toEqual({ sx: 1, sy: 1, tx: 0, ty: 0 });
    const m = zoomXform(f, { row: 0, t: 1 }, false)!;
    expect(m.sx).toBeCloseTo(280 / 160);
    expect(m.sx).toBe(m.sy);
    expect(60 * m.sx + m.tx).toBeCloseTo(60); // sub's box left edge lands on the root's
    expect(zoomXform(f, { row: 0, t: 1 }, true)).toBeNull();
  });

  it("dims what the filter leaves out", () => {
    const f = shapesScene({ info: {} }).frame;
    f.flags[1] = Flag.DIM;
    const p = paintShapes(f, settings());
    expect(p.instances[2 * SHAPE_STRIDE + SLOT.extra + 1]).toBeLessThan(1);
  });
});

/** A ShapeRenderer that keeps what it was asked to do. */
function fakeRenderer() {
  let made = 0;
  const draws: { picture: ShapePicture; xform: number[]; alpha: number }[] = [];
  const uploads: { count: number; reused: boolean }[] = [];
  const gl = {
    upload(_data: Float32Array, count: number, previous?: ShapePicture | null): ShapePicture {
      const picture = previous ?? ({ vao: { id: ++made }, buffer: {}, count } as unknown as ShapePicture);
      picture.count = count;
      uploads.push({ count, reused: !!previous });
      return picture;
    },
    use() {},
    draw(picture: ShapePicture, sx = 1, sy = 1, tx = 0, ty = 0, alpha = 1) {
      draws.push({ picture, xform: [sx, sy, tx, ty], alpha });
    },
    dispose() {},
  };
  return { gl: gl as unknown as ShapeRenderer, draws, uploads };
}

function fakeContext(renderer: ShapeRenderer) {
  const texts: string[] = [];
  const text = {
    font: "",
    fillStyle: "",
    globalAlpha: 1,
    textBaseline: "alphabetic",
    measureText: (s: string) => ({ width: s.length * 6 }),
    fillText: (s: string) => texts.push(s),
    beginPath() {},
    roundRect() {},
    fill() {},
    moveTo() {},
    lineTo() {},
    closePath() {},
    arc() {},
    stroke() {},
  };
  const ctx = { canvas: {}, gl: {}, text, shared: () => renderer } as unknown as VisualContext;
  return { ctx, texts };
}

describe("component", () => {
  it("uploads the picture and the overlay, repaints only the overlay on a hover, says why when the plugin failed", () => {
    const r = fakeRenderer();
    const { ctx, texts } = fakeContext(r.gl);
    const inst = shapesComponent.create(ctx);
    const f = shapesScene().frame;
    inst.setFrame(f, settings());
    expect(r.uploads.map((u) => u.count)).toEqual([1 + f.count, SELECTION_INSTANCES]); // nothing hovered yet
    inst.setHover(2);
    expect(r.uploads).toHaveLength(3);
    expect(r.uploads[2]!.reused).toBe(true);
    inst.draw({ now: 0, alpha: 1, zoom: null });
    expect(r.draws).toHaveLength(2);
    expect(texts).toContain("a.mp4");

    const failed = shapesScene({ info: { geometry: { kind: "shapes", root: null, nested: true, message: "插件没有在 5 秒内排好布局", stroke: 1 } } }).frame; // i18n-ignore
    const empty = { ...failed, count: 0 } as typeof failed;
    texts.length = 0;
    inst.setFrame(empty, settings());
    inst.draw({ now: 0, alpha: 1, zoom: null });
    expect(texts).toContain("插件没有在 5 秒内排好布局"); // i18n-ignore
  });
});

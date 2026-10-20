// The flame component's instance against a recording renderer and label canvas (GL does not run
// under node): what gets uploaded on a frame and on a hover, the zoom transform it draws with, and
// the GL objects it gives back.

import { describe, expect, it } from "vitest";

import type { GlRenderer, Picture } from "@/treemap/gl";

import { componentFor, registerVisual, visualCapabilities } from "../registry";
import { rectScene } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type PaintSettings, type VisualContext, VISUAL_API } from "../types";
import { flameComponent } from "./component";
import { flameGeometry } from "./geometry";
import { zoomXform } from "./paint";

const settings = (patch: Partial<PaintSettings> = {}): PaintSettings => ({
  colorMode: "branch",
  classIds: ["other", "video"],
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

/** A GlRenderer that keeps what it was asked to do. */
function fakeRenderer() {
  let made = 0;
  const log: string[] = [];
  const draws: { picture: Picture; xform: number[]; alpha: number }[] = [];
  const uploads: { picture: Picture; count: number; reused: boolean }[] = [];
  const disposed: (Picture | null)[] = [];
  const gl = {
    upload(_data: Float32Array, count: number, previous?: Picture | null): Picture {
      const picture = previous ?? ({ vao: { id: ++made }, buffer: {}, count } as unknown as Picture);
      picture.count = count;
      uploads.push({ picture, count, reused: !!previous });
      return picture;
    },
    use(w: number, h: number) {
      log.push(`use ${w}×${h}`);
    },
    draw(picture: Picture, sx = 1, sy = 1, tx = 0, ty = 0, alpha = 1) {
      draws.push({ picture, xform: [sx, sy, tx, ty], alpha });
    },
    dispose(p: Picture | null) {
      disposed.push(p);
    },
  };
  return { gl: gl as unknown as GlRenderer, log, draws, uploads, disposed };
}

/** A label canvas: fixed-width glyphs, the texts drawn kept. */
function fakeText() {
  const drawn: { text: string; x: number; y: number; alpha: number }[] = [];
  const ctx = {
    font: "",
    fillStyle: "",
    strokeStyle: "",
    lineWidth: 1,
    globalAlpha: 1,
    textBaseline: "alphabetic",
    measureText: (s: string) => ({ width: s.length * 6 }),
    fillText(text: string, x: number, y: number) {
      drawn.push({ text, x, y, alpha: ctx.globalAlpha });
    },
    beginPath() {},
    roundRect() {},
    fill() {},
    stroke() {},
    arc() {},
    moveTo() {},
    lineTo() {},
    closePath() {},
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, drawn };
}

function setup() {
  const r = fakeRenderer();
  const t = fakeText();
  const keys: string[] = [];
  const ctx: VisualContext = {
    canvas: {} as HTMLCanvasElement,
    gl: {} as WebGL2RenderingContext,
    text: t.ctx,
    shared: <T,>(key: string, _make: () => T): T => {
      keys.push(key);
      return r.gl as unknown as T;
    },
  };
  return { ...r, ...t, keys, ctx, instance: flameComponent.create(ctx) };
}

describe("flameComponent", () => {
  it("describes flame.v1 and registers", () => {
    expect(flameComponent).toMatchObject({ layout: "flame.v1", api: VISUAL_API, capability: "layout.flame.v1", hoverPlacement: "beside" });
    expect(flameComponent.geometry).toBe(flameGeometry);
    registerVisual(flameComponent);
    expect(componentFor("flame.v1")).toBe(flameComponent);
    expect(visualCapabilities()).toContain("layout.flame.v1");
  });

  it("shares the context's rectangle renderer", () => {
    const { keys } = setup();
    expect(keys).toEqual(["rect"]);
  });

  it("uploads the picture and the overlay for a frame, then only the overlay for a hover", () => {
    const { instance, uploads } = setup();
    const { frame: f } = rectScene({ layout: "flame.v1" });
    instance.setFrame(f, settings());
    expect(uploads.map((u) => [u.count, u.reused])).toEqual([
      [f.count + 1, false],
      [4, false], // the selection: shadow, bar, two rings
    ]);
    const main = uploads[0]!.picture;
    const over = uploads[1]!.picture;
    instance.setHover(f.rowOf(6));
    expect(uploads).toHaveLength(3);
    expect(uploads[2]!.picture).toBe(over);
    expect(uploads[2]!.reused).toBe(true);
    instance.setHover(f.rowOf(6)); // the same row: nothing to do
    expect(uploads).toHaveLength(3);
    // a new frame reuses both pictures
    instance.setFrame(rectScene({ layout: "flame.v1", info: { seq: 2 } }).frame, settings({ selectionShadow: false }));
    expect(uploads.slice(3).map((u) => [u.picture, u.reused])).toEqual([
      [main, true],
      [over, true],
    ]);
    expect(uploads[4]!.count).toBe(2); // hover dropped with the old frame's rows; the rings stay
  });

  it("keeps the hover across a repaint of the same frame", () => {
    const { instance, uploads } = setup();
    const { frame: f } = rectScene({ layout: "flame.v1", info: { selected_row: null } });
    instance.setFrame(f, settings());
    instance.setHover(HIT_ROOT);
    expect(uploads.at(-1)!.count).toBe(1);
    instance.setFrame(f, settings({ colorMode: "class" }));
    expect(uploads.at(-1)!.count).toBe(1);
  });

  it("draws both pictures and the labels, under the zoom transform while zooming", () => {
    const { instance, draws, log, drawn } = setup();
    const { frame: f } = rectScene({ layout: "flame.v1" });
    instance.setFrame(f, settings());
    expect(instance.draw({ now: 0, alpha: 1, zoom: null })).toBe(false);
    expect(log).toEqual(["use 400×200"]);
    expect(draws.map((d) => [d.xform, d.alpha])).toEqual([
      [[1, 1, 0, 0], 1],
      [[1, 1, 0, 0], 1],
    ]);
    const label = drawn.find((d) => d.text === "a.mp4")!;
    expect(label.x).toBe(f.x[f.rowOf(2)]! + 6);

    draws.length = 0;
    drawn.length = 0;
    const zoom = { row: f.rowOf(3), t: 0.5 };
    instance.draw({ now: 16, alpha: 0.4, zoom });
    const m = zoomXform(f, zoom, false)!;
    expect(draws.map((d) => [d.xform, d.alpha])).toEqual([
      [[m.sx, m.sy, m.tx, m.ty], 0.4],
      [[m.sx, m.sy, m.tx, m.ty], 0.4],
    ]);
    const moved = drawn.find((d) => d.text === "a.mp4")!;
    expect(moved.x).toBeCloseTo((f.x[f.rowOf(2)]! + 6) * m.sx + m.tx, 9);
    expect(moved.alpha).toBeCloseTo(0.4, 9);
  });

  it("draws as laid out under reduced motion, and nothing before a frame or when transparent", () => {
    const { instance, draws } = setup();
    expect(instance.draw({ now: 0, alpha: 1, zoom: null })).toBe(false);
    expect(draws).toHaveLength(0);
    const { frame: f } = rectScene({ layout: "flame.v1" });
    instance.setFrame(f, settings({ reduced: true }));
    instance.draw({ now: 0, alpha: 1, zoom: { row: f.rowOf(3), t: 0.5 } });
    expect(draws.map((d) => d.xform)).toEqual([
      [1, 1, 0, 0],
      [1, 1, 0, 0],
    ]);
    instance.draw({ now: 0, alpha: 0, zoom: null });
    expect(draws).toHaveLength(2);
  });

  it("gives its pictures back on dispose and uploads again after a lost context", () => {
    const { instance, uploads, disposed, keys, draws } = setup();
    const { frame: f } = rectScene({ layout: "flame.v1" });
    instance.setFrame(f, settings());
    const [main, over] = [uploads[0]!.picture, uploads[1]!.picture];
    instance.contextRestored();
    expect(keys).toEqual(["rect", "rect"]);
    expect(uploads.slice(2).map((u) => [u.count, u.reused])).toEqual([
      [f.count + 1, false],
      [uploads[1]!.count, false],
    ]);
    instance.draw({ now: 0, alpha: 1, zoom: null });
    expect(draws.map((d) => d.picture)).toEqual([uploads[2]!.picture, uploads[3]!.picture]);
    expect(draws[0]!.picture).not.toBe(main);
    instance.dispose();
    expect(disposed).toEqual([uploads[2]!.picture, uploads[3]!.picture]);
    expect(disposed).not.toContain(over);
    expect(instance.draw({ now: 0, alpha: 1, zoom: null })).toBe(false);
    expect(draws).toHaveLength(2);
    instance.setHover(HIT_NONE); // harmless after dispose
  });
});

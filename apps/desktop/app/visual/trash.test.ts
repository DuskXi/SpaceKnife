import { describe, expect, it } from "vitest";

import { Flag, Kind } from "./frame";
import { BIN_ICON, drawTrash, sweepAt, trashMarks, trashMoves } from "./overlays";
import { encodeFrame, PathRecorder, polarScene, rectScene } from "./testing/fixtures";
import { flameGeometry } from "./flame/geometry";
import { polarGeometry } from "./flamePolar/geometry";
import { treemapGeometry } from "./treemap/geometry";
import { HIT_ROOT, type PolarFrame, type RectFrame } from "./types";

const recorder = () => new PathRecorder() as unknown as Path2D;

/** A treemap frame: the bin with two blocks in it, and a folder that is not in a bin. */
function binFrame(): RectFrame {
  const T = Flag.TRASH;
  return encodeFrame<RectFrame>(
    "drawlist.v1",
    [
      { key: 1, node: 1, parent: -1, kind: Kind.FOLDER, depth: 1, geo: [0, 0, 300, 200], size: 900n, flags: T | Flag.TRASH_BIN, name: "$Recycle.Bin" },
      { key: 2, node: 2, parent: 0, kind: Kind.FILE, depth: 2, geo: [2, 20, 200, 178], size: 700n, flags: T, name: "$RX1.mp4" },
      { key: 3, node: 3, parent: 0, kind: Kind.FILE, depth: 2, geo: [202, 20, 96, 178], size: 200n, flags: T, name: "$RX2.iso" },
      { key: 4, node: 4, parent: -1, kind: Kind.FOLDER, depth: 1, geo: [300, 0, 300, 200], size: 900n, name: "Users" },
      { key: 5, node: 5, parent: 3, kind: Kind.FILE, depth: 2, geo: [302, 20, 296, 178], size: 900n, name: "trip.mp4" },
    ],
    { width: 600, height: 200 },
  ).frame;
}

describe("what lies in a recycle bin", () => {
  it("marks the bin once, not each block in it, and only the bin carries the badge", () => {
    const marks = trashMarks(treemapGeometry, binFrame(), 1, recorder);
    expect(marks.map((m) => [m.hit, m.bin, m.size])).toEqual([[0, true, 900]]);
    expect(marks[0]!.badge).not.toBeNull();
    expect(marks[0]!.bounds).toEqual({ x: 0, y: 0, w: 300, h: 200 });
  });

  it("marks every top row when the view is inside the bin", () => {
    const f = encodeFrame<RectFrame>(
      "drawlist.v1",
      [
        { key: 2, node: 2, parent: -1, kind: Kind.FILE, depth: 1, geo: [0, 0, 400, 200], size: 700n, flags: Flag.TRASH },
        { key: 3, node: 3, parent: -1, kind: Kind.FILE, depth: 1, geo: [400, 0, 200, 200], size: 200n, flags: Flag.TRASH },
      ],
      { width: 600, height: 200 },
    ).frame;
    expect(trashMarks(treemapGeometry, f, 1, recorder).map((m) => [m.hit, m.bin])).toEqual([
      [0, false],
      [1, false],
    ]);
  });

  it("veils the centre disc and every ring of a polar flame graph zoomed into the bin as one area", () => {
    const f = polarScene().frame as PolarFrame;
    f.info.root = { ...f.info.root!, flags: Flag.TRASH | Flag.TRASH_BIN, size: 1000 };
    for (let i = 0; i < f.count; i++) f.flags[i] = f.flags[i]! | Flag.TRASH;
    const marks = trashMarks(polarGeometry, f, 1, recorder);
    expect(marks.map((m) => [m.hit, m.bin, m.size])).toEqual([[HIT_ROOT, true, 1000]]);
    const area = (marks[0]!.area as unknown as PathRecorder).ops;
    const outline = (marks[0]!.path as unknown as PathRecorder).ops;
    expect(area.length).toBeGreaterThan(outline.length); // the disc and the rings around it
    expect(marks[0]!.sweep!.w).toBeGreaterThan(marks[0]!.bounds!.w);
  });

  it("veils a bin's bars with it in a flame graph, but outlines and labels only the bin", () => {
    const f = rectScene({ layout: "flame.v1" }).frame;
    const sub = Array.from({ length: f.count }, (_, i) => i).find((i) => f.names[i] === "sub")!;
    const inside = (r: number) => {
      for (let p = r; p >= 0; p = f.parent[p]!) if (p === sub) return true;
      return false;
    };
    for (let i = 0; i < f.count; i++) if (inside(i)) f.flags[i] = f.flags[i]! | Flag.TRASH | (i === sub ? Flag.TRASH_BIN : 0);
    const marks = trashMarks(flameGeometry, f, 1, recorder);
    expect(marks.map((m) => [m.hit, m.bin])).toEqual([[sub, true]]);
    const area = (marks[0]!.area as unknown as PathRecorder).ops;
    expect(area.length).toBeGreaterThan((marks[0]!.path as unknown as PathRecorder).ops.length);
  });

  it("leaves out blocks too thin to show anything", () => {
    const f = encodeFrame<RectFrame>("drawlist.v1", [{ key: 1, node: 1, parent: -1, kind: Kind.FILE, depth: 1, geo: [0, 0, 300, 2], size: 1n, flags: Flag.TRASH }], { width: 300, height: 200 }).frame;
    expect(trashMarks(treemapGeometry, f, 1, recorder)).toEqual([]);
  });

  it("moves only when animated and motion is not reduced", () => {
    const marks = trashMarks(treemapGeometry, binFrame(), 1, recorder);
    expect(trashMoves(marks, "animated", false)).toBe(true);
    expect(trashMoves(marks, "animated", true)).toBe(false);
    expect(trashMoves(marks, "static", false)).toBe(false);
    expect(trashMoves([], "animated", false)).toBe(false);
  });

  it("sweeps a band of light across now and then, each bin at its own time", () => {
    expect(sweepAt(0, 0)).toBe(0);
    expect(sweepAt(1400, 0)).toBeCloseTo(0.5);
    expect(sweepAt(3000, 0)).toBeNull(); // resting
    expect(sweepAt(7000, 0)).toBe(0); // the next sweep
    expect(sweepAt(0, 1)).not.toBe(sweepAt(0, 0));
  });

  it("draws the bin icon of the design set", () => {
    expect(BIN_ICON.startsWith("M")).toBe(true);
    expect(BIN_ICON).not.toContain('"');
  });
});

/** A 2D context that records the calls the trash layer makes. */
function canvas() {
  const calls: string[] = [];
  const texts: string[] = [];
  const state = { lineDashOffset: 0 };
  const rec =
    (name: string) =>
    (...args: unknown[]) => {
      calls.push(name);
      if (name === "fillText") texts.push(String(args[0]));
    };
  const ctx = {
    globalAlpha: 1,
    lineWidth: 1,
    strokeStyle: "",
    fillStyle: "" as unknown,
    shadowColor: "",
    shadowBlur: 0,
    font: "",
    textBaseline: "alphabetic",
    lineCap: "butt",
    lineJoin: "miter",
    get lineDashOffset() {
      return state.lineDashOffset;
    },
    set lineDashOffset(v: number) {
      state.lineDashOffset = v;
      calls.push(`dashOffset:${v === 0 ? 0 : "moving"}`);
    },
    save: rec("save"),
    restore: rec("restore"),
    clip: rec("clip"),
    fill: rec("fill"),
    fillRect: rec("fillRect"),
    stroke: rec("stroke"),
    setLineDash: rec("setLineDash"),
    beginPath: rec("beginPath"),
    roundRect: rec("roundRect"),
    translate: rec("translate"),
    scale: rec("scale"),
    fillText: rec("fillText"),
    measureText: (text: string) => ({ width: text.length * 6 }),
    createLinearGradient: () => {
      calls.push("gradient");
      return { addColorStop() {} };
    },
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, calls, texts };
}

describe("drawing the bin", () => {
  const marks = () => trashMarks(treemapGeometry, binFrame(), 1, recorder);
  const icon: string[] = [];
  const newPath = (d?: string) => {
    if (d) icon.push(d);
    return recorder();
  };

  it("veils, outlines and labels the bin; still: no sweep, the dashes stay", () => {
    const { ctx, calls, texts } = canvas();
    drawTrash(ctx, marks(), 1000, false, 1, (b) => `bin ${b}`, newPath);
    expect(calls).toContain("clip");
    expect(calls).toContain("fill");
    expect(calls).toContain("setLineDash");
    expect(calls).toContain("dashOffset:0");
    expect(calls).not.toContain("gradient");
    expect(texts).toEqual(["bin 900"]);
    expect(icon).toContain(BIN_ICON);
  });

  it("moving: the dashes march and the light sweeps while it is its time", () => {
    const { ctx, calls } = canvas();
    drawTrash(ctx, marks(), 1000, true, 1, (b) => `bin ${b}`, newPath);
    expect(calls).toContain("dashOffset:moving");
    expect(calls).toContain("gradient");
    const rest = canvas();
    drawTrash(rest.ctx, marks(), 5000, true, 1, (b) => `bin ${b}`, newPath);
    expect(rest.calls).not.toContain("gradient");
  });

  it("draws nothing without marks", () => {
    const { ctx, calls } = canvas();
    drawTrash(ctx, [], 0, true, 1, () => "", newPath);
    expect(calls).toEqual([]);
  });
});

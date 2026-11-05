// Label placement of the polar flame graph: upright in every quadrant, the width formulas of
// docs/protocol.md §4.4, label bits as the only source, and the centre disc's text.

import { describe, expect, it } from "vitest";

import type { PolarGeometry } from "@/protocol/generated";

import { Flag, Kind, Label } from "../frame";
import { polarScene, type SceneNode } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type PaintSettings, type PolarFrame } from "../types";
import { pointAt, tangentWidth, TURN } from "./geometry";
import { centreLabel, drawLabels, prepareLabels, radialMaxWidth, radialRotation, rowLabel, tangentMaxWidth, tangentRotation } from "./labels";

const S: PaintSettings = {
  colorMode: "branch",
  classIds: ["other", "video", "doc", "image"],
  classColors: [],
  scanning: false,
  formatSize: (b) => `${b} B`,
  formatInt: (n) => String(n),
  formatDelta: (b) => `${b < 0 ? "−" : "+"}${Math.abs(b)} B`,
  haloLevels: 3,
  hoverHighlight: true,
  selectionShadow: true,
  fontScale: 1,
  levelContrast: 1,
  borderContrast: 1,
  reduced: false,
  diffTiers: null,
};

const deg = (d: number) => (d / 180) * Math.PI;
const geo = (f: PolarFrame) => f.info.geometry as PolarGeometry;

/** A text's up direction on screen after `rot`: upright when it points up (y < 0) or sideways. */
const upright = (rot: number) => Math.cos(rot) >= -1e-12;

/** Root of 1000 B: labelled rows of every kind in ring 1, a radial one and a two-line one. */
const TREE: SceneNode = {
  key: 1,
  node: 1,
  kind: Kind.FOLDER,
  size: 1000n,
  name: "root",
  children: [
    { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "movie.mkv", cls: 1, label: Label.NAME | Label.SIZE },
    { key: 3, node: 3, kind: Kind.FOLDER, size: 300n, name: "docs", label: Label.NAME | Label.SIZE, flags: Flag.FOLDER_HIT, children: [{ key: 4, node: 4, kind: Kind.FILE, size: 300n, name: "a.pdf", label: 0 }] },
    { key: 5, node: 5, kind: Kind.PENDING, size: 100n, name: "cache", label: Label.NAME | Label.SIZE },
    { key: 6, node: 6, kind: Kind.FILE, size: 60n, name: "thin.bin", label: Label.NAME | Label.SHORT | Label.RADIAL },
    { key: (1 << 30) + 1, node: 1, kind: Kind.SMALL, size: 40n, count: 9, label: Label.NAME | Label.SHORT },
  ],
};

describe("rotations", () => {
  it("keeps tangential labels upright in all four quadrants", () => {
    expect(tangentRotation(deg(45))).toBeCloseTo(deg(45), 12);
    expect(tangentRotation(deg(135))).toBeCloseTo(deg(-45), 12);
    expect(tangentRotation(deg(225))).toBeCloseTo(deg(45), 12);
    expect(tangentRotation(deg(315))).toBeCloseTo(deg(-45), 12);
    expect(tangentRotation(0)).toBe(0);
    expect(tangentRotation(deg(180))).toBeCloseTo(0, 12);
    for (let d = 0; d < 720; d += 7.5) {
      const rot = tangentRotation(deg(d));
      expect(upright(rot)).toBe(true);
      // along the tangent: the angle itself or half a turn from it
      expect(Math.abs(Math.sin(rot - deg(d)))).toBeLessThan(1e-9);
    }
  });

  it("reads radial labels outward on the right half and right-aligns them on the left half", () => {
    expect(radialRotation(deg(45))).toEqual({ rot: deg(-45), align: "left" });
    expect(radialRotation(deg(90))).toEqual({ rot: 0, align: "left" });
    expect(radialRotation(deg(135)).align).toBe("left");
    expect(radialRotation(deg(135)).rot).toBeCloseTo(deg(45), 12);
    expect(radialRotation(deg(225)).align).toBe("right");
    expect(radialRotation(deg(225)).rot).toBeCloseTo(deg(-45), 12);
    expect(radialRotation(deg(315)).align).toBe("right");
    expect(radialRotation(deg(315)).rot).toBeCloseTo(deg(45), 12);
    for (let d = 0; d < 360; d += 5) {
      const { rot, align } = radialRotation(deg(d));
      expect(upright(rot)).toBe(true);
      // the text's x axis points away from the centre on the right half, toward it on the left
      const out = [Math.sin(deg(d)), -Math.cos(deg(d))];
      const along = Math.cos(rot) * out[0]! + Math.sin(rot) * out[1]!;
      expect(along).toBeCloseTo(align === "left" ? 1 : -1, 9);
    }
  });
});

describe("widths", () => {
  it("follows the backend's formulas", () => {
    expect(tangentMaxWidth(0.5, 100, 140, 3, 2)).toBeCloseTo(tangentWidth(0.5, 100, 140, 3, 2) - 24, 12);
    expect(tangentMaxWidth(0.5, 100, 140, 3, 2)).toBeCloseTo(Math.min(0.5 * 120 - 3, 2 * Math.sqrt(136 ** 2 - 120 ** 2)) - 24, 12);
    expect(radialMaxWidth(100, 140, 2)).toBe(24);
    expect(radialMaxWidth(100, 140, 1)).toBe(32);
  });
});

describe("rowLabel", () => {
  const { frame: f } = polarScene({ tree: TREE, width: 800, height: 800, detail: 3 });
  const g = geo(f);
  const label = (key: number) => rowLabel(f, f.rowOf(key), S, "#fff", "#ccc", 1);

  it("places a tangential label at mid radius and mid angle, two lines along the radius", () => {
    const i = f.rowOf(2);
    const l = label(2)!;
    const mid = (f.a0[i]! + f.a1[i]!) / 2;
    expect({ x: l.x, y: l.y }).toEqual(pointAt(g, (f.r0[i]! + f.r1[i]!) / 2, mid));
    expect(l.rot).toBeCloseTo(tangentRotation((mid / g.unit) * TURN), 12);
    expect(l.align).toBe("center");
    expect(l.maxWidth).toBeCloseTo(tangentMaxWidth(((f.a1[i]! - f.a0[i]!) / g.unit) * TURN, f.r0[i]!, f.r1[i]!, g.gap, 1), 12);
    expect(l.lines.map((x) => x.text)).toEqual(["movie.mkv", "400 B"]);
    // the name first in reading order, both lines about the anchor
    expect(l.lines[0]!.dy).toBeLessThan(l.lines[1]!.dy);
    expect(l.lines[0]!.dy).toBeLessThan(0);
    expect(l.lines[0]!.font).toMatch(/^500 11px /);
    expect(l.lines[1]!.font).toMatch(/^400 10\.5px "Geist Mono"/);
    // the size line in the sub colour, the name's while lit; the label knows its row
    expect(l.lines.map((x) => [x.color, x.lit])).toEqual([
      ["#fff", undefined],
      ["#ccc", "#fff"],
    ]);
    expect(l.row).toBe(i);
  });

  it("writes folders semibold, bold on a folder-mask hit, and waiting folders' second line", () => {
    expect(label(3)!.lines[0]!.font).toMatch(/^700 /);
    expect(label(5)!.lines.map((x) => x.text)).toEqual(["cache", "等待扫描…"]);
  });

  it("places a radial label from the inner edge, one line", () => {
    const i = f.rowOf(6);
    const l = label(6)!;
    const mid = (f.a0[i]! + f.a1[i]!) / 2;
    expect({ x: l.x, y: l.y }).toEqual(pointAt(g, f.r0[i]! + 4, mid));
    expect(l).toMatchObject(radialRotation((mid / g.unit) * TURN));
    expect(l.maxWidth).toBe(radialMaxWidth(f.r0[i]!, f.r1[i]!, 1));
    expect(l.lines.map((x) => x.text)).toEqual(["thin.bin"]);
    expect(l.lines[0]!.font).toMatch(/^500 10\.5px /);
  });

  it("takes the text of aggregates from rowTitle and labels nothing without the bits", () => {
    expect(label((1 << 30) + 1)!.lines.map((x) => x.text)).toEqual(["9 个小项"]);
    expect(label(4)).toBeNull();
    expect(f.names[f.rowOf(4)]).toBe("");
  });

  it("writes the change in compare views", () => {
    const tree: SceneNode = { ...TREE, children: [{ key: 2, node: 2, kind: Kind.FILE, size: 400n, delta: -25n, dstat: 3, name: "x", label: Label.NAME | Label.SIZE }] };
    const { frame: c } = polarScene({ tree, width: 800, height: 800, detail: 3 });
    expect(rowLabel(c, 0, { ...S, diffTiers: [1, 2, 3] }, "#fff", "#ccc", 1)!.lines[1]!.text).toBe("−25 B");
    expect(rowLabel(c, 0, S, "#fff", "#ccc", 1)!.lines[1]!.text).toBe("400 B");
  });
});

describe("centreLabel", () => {
  const { frame: f } = polarScene({ tree: TREE, width: 800, height: 800, detail: 3 });
  const g = geo(f);

  it("names the zoom root and its size", () => {
    const c = centreLabel(f, S, HIT_NONE)!;
    expect([c.x, c.y, c.rot, c.align, c.maxWidth]).toEqual([g.cx, g.cy, 0, "center", 1.6 * g.hole]);
    expect(c.lines.map((l) => l.text)).toEqual(["root", "1000 B"]);
    expect(c.lines[0]!.font).toMatch(/^700 13px /);
    expect(c.lines[1]!.font).toMatch(/^400 11\.5px "Geist Mono"/);
    // centred on the disc
    expect(c.lines[0]!.dy + c.lines[1]!.dy).toBeGreaterThan(-5);
    expect(c.lines[0]!.dy + c.lines[1]!.dy).toBeLessThan(15);
    expect(centreLabel(f, S, HIT_ROOT)!.lines.map((l) => l.text)).toEqual(["root", "1000 B"]);
  });

  it("names the hovered row, its size and its share of the root", () => {
    expect(centreLabel(f, S, f.rowOf(2))!.lines.map((l) => l.text)).toEqual(["movie.mkv", "400 B", "占 40%"]);
    // a row without a name: size and share
    expect(centreLabel(f, S, f.rowOf(4))!.lines.map((l) => l.text)).toEqual(["300 B", "占 30%"]);
  });

  it("leaves out the lines a small disc cannot hold, and has nothing without a root", () => {
    const { frame: small } = polarScene({ tree: TREE, width: 120, height: 120, detail: 1 });
    expect(centreLabel(small, S, small.rowOf(2))!.lines.length).toBeLessThan(3);
    expect(centreLabel({ ...f, info: { ...f.info, root: null } }, S, HIT_NONE)).toBeNull();
  });
});

describe("prepare and draw", () => {
  /** A canvas stand-in: 6 px per character, and a log of what was drawn. */
  const fake = () => {
    const log: string[] = [];
    const ctx = {
      font: "",
      fillStyle: "",
      textAlign: "start",
      textBaseline: "alphabetic",
      globalAlpha: 1,
      measureText: (s: string) => ({ width: 6 * s.length }),
      save: () => log.push("save"),
      restore: () => log.push("restore"),
      translate: (x: number, y: number) => log.push(`translate ${x} ${y}`),
      rotate: (r: number) => log.push(`rotate ${r.toFixed(3)}`),
      fillText(s: string, x: number, y: number) {
        log.push(`fill ${s} ${x} ${y.toFixed(2)} ${this.textAlign} ${this.globalAlpha}`);
      },
    };
    return { ctx: ctx as unknown as CanvasRenderingContext2D, log };
  };

  it("fits each line once and drops labels without room", () => {
    const { ctx } = fake();
    const line = (text: string) => ({ text, font: "500 11px x", color: "#fff", dy: 0 });
    const out = prepareLabels(ctx, [
      { x: 0, y: 0, rot: 0, align: "center", maxWidth: 40, lines: [line("abcdefghij"), line("12 B")], alpha: 1 },
      { x: 0, y: 0, rot: 0, align: "center", maxWidth: 4, lines: [line("a")], alpha: 1 },
      { x: 0, y: 0, rot: 0, align: "center", maxWidth: 5, lines: [line("abc")], alpha: 1 },
    ]);
    expect(out.map((l) => l.shown)).toEqual([["abcde…", "12 B"]]);
  });

  it("draws turned around the anchor and fades with the pass", () => {
    const { ctx, log } = fake();
    const [l] = prepareLabels(ctx, [{ x: 10, y: 20, rot: Math.PI / 4, align: "right", maxWidth: 100, lines: [{ text: "name", font: "f", color: "#fff", dy: 3 }], alpha: 0.5 }]);
    drawLabels(ctx, [l!], 0.5);
    expect(log).toEqual(["save", "save", "translate 10 20", "rotate 0.785", "fill name 0 3.00 right 0.25", "restore", "restore"]);
  });

  it("draws a lit row's lines in their lit colours", () => {
    const { ctx } = fake();
    const colours: string[] = [];
    const fillText = ctx.fillText.bind(ctx);
    ctx.fillText = (s: string, x: number, y: number) => {
      colours.push(`${s} ${String(ctx.fillStyle)}`);
      fillText(s, x, y);
    };
    const lines = [
      { text: "name", font: "f", color: "#fff", dy: 0 },
      { text: "12 B", font: "f", color: "#ccc", lit: "#fff", dy: 12 },
    ];
    const prepared = prepareLabels(ctx, [{ x: 0, y: 0, rot: 0, align: "center", maxWidth: 100, lines, alpha: 1, row: 4 }]);
    drawLabels(ctx, prepared, 1);
    drawLabels(ctx, prepared, 1, new Set([3]));
    drawLabels(ctx, prepared, 1, new Set([4]));
    expect(colours).toEqual(["name #fff", "12 B #ccc", "name #fff", "12 B #ccc", "name #fff", "12 B #fff"]);
  });
});

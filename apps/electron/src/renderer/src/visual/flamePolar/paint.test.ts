// Sector instances of the polar flame graph: the centre disc, colour rules, stripes and dots, the
// overlay (halo, hover, selection) within its limit, labels only from label bits; and the sector
// program's attribute layout, which has to match the instances (the shaders do not run under node).

import { themes, tokens } from "@design/tokens";
import { describe, expect, it } from "vitest";

import { applyTheme } from "@/lib/theme";
import type { PolarGeometry } from "@/protocol/generated";
import { fillOf, Pattern, rgba, type RGBA } from "@/treemap/paint";

import { branchFill, center, haloOverlay, ringFill } from "../colors";
import { DStat, Flag, Kind, Label } from "../frame";
import { polarScene, SCENE_TREE, type SceneNode } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type PaintSettings, type PolarFrame } from "../types";
import { discRadius, polarGeometry } from "./geometry";
import { overlayLimit, paintFrame, paintOverlay, type Painted, SLOT, STRIDE } from "./paint";
import { ATTRIBUTES, SECTOR_FS, SECTOR_VS, SEGMENTS, STRIP_VERTICES } from "./sectors";

const T = tokens.treemap;

const S: PaintSettings = {
  colorMode: "class",
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

const geo = (f: PolarFrame) => f.info.geometry as PolarGeometry;

/** Instance k of a picture, by attribute. */
function inst(p: Painted, k: number) {
  const at = k * STRIDE;
  const part = (slot: number) => Array.from(p.instances.subarray(at + slot, at + slot + 4));
  return { sector: part(SLOT.sector), fill: part(SLOT.fill), border: part(SLOT.border), pattern: part(SLOT.pattern), shape: part(SLOT.shape), extra: part(SLOT.extra) };
}

/** RGBA as the Float32Array holds it. */
const f32 = (c: RGBA) => Array.from(Float32Array.from(c));

/** The instance drawn for a row (the disc comes first). */
const rowInst = (p: Painted, row: number) => inst(p, row + 1);

describe("sector program layout", () => {
  it("binds every attribute where paint writes it", () => {
    expect(STRIDE).toBe(24);
    const slots = Object.values(SLOT).sort((a, b) => a - b);
    expect(slots).toEqual([0, 4, 8, 12, 16, 20]);
    expect(ATTRIBUTES.map((a) => a.offset).sort((a, b) => a - b)).toEqual(slots);
    expect(new Set(ATTRIBUTES.map((a) => a.location)).size).toBe(ATTRIBUTES.length);
    // the vertex shader declares the same names at the same locations; location 0 is the strip
    const declared = new Map([...SECTOR_VS.matchAll(/layout\(location=(\d+)\) in (vec\d) (\w+);/g)].map((m) => [m[3]!, { location: Number(m[1]), type: m[2] }]));
    expect(declared.get("a_strip")).toEqual({ location: 0, type: "vec2" });
    for (const a of ATTRIBUTES) expect(declared.get(a.name)).toEqual({ location: a.location, type: "vec4" });
    expect(declared.size).toBe(ATTRIBUTES.length + 1);
    expect([SEGMENTS, STRIP_VERTICES]).toEqual([tokens.visual.flamePolar.segments, 2 * tokens.visual.flamePolar.segments + 2]);
  });

  it("keeps the shaders as reviewed (update the digest with a deliberate change)", () => {
    // FNV-1a of both sources: a snapshot without a snapshot file
    let h = 0x811c9dc5;
    for (const ch of `${SECTOR_VS}\n${SECTOR_FS}`) {
      h ^= ch.charCodeAt(0);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    expect(h.toString(16).padStart(8, "0")).toBe("8035ad3b");
    // the shared rules with geometry.ts sectorDistance and the rectangle program
    expect(SECTOR_FS).toContain("min(gap * 0.5, max(0.0, (span * s.z - 1.0) * 0.5))");
    expect(SECTOR_FS).toContain("float inside = clamp(0.5 - d, 0.0, 1.0);");
    expect(SECTOR_VS).toContain(`const float SEGMENTS = ${SEGMENTS}.0;`);
    // GLSL ES 3.00 reserves these words: they must not name anything
    for (const word of ["half", "input", "output", "sample", "filter", "fixed"]) expect(`${SECTOR_VS}\n${SECTOR_FS}`).not.toMatch(new RegExp(`\\b(float|vec\\d|bool|int) ${word}\\b`));
  });
});

describe("paintFrame", () => {
  const scene = polarScene();
  const f = scene.frame;
  const g = geo(f);

  it("draws the centre disc, then one sector per row", () => {
    const p = paintFrame(f, S);
    expect(p.count).toBe(f.count + 1);
    expect(inst(p, 0).sector).toEqual([0, 1, 0, discRadius(g)]);
    expect(inst(p, 0).fill).toEqual(f32(center().bg));
    for (let i = 0; i < f.count; i++) {
      const s = rowInst(p, i).sector;
      expect(s).toEqual(f32([f.a0[i]! / g.unit, f.a1[i]! / g.unit, f.r0[i]!, f.r1[i]!]));
      expect(rowInst(p, i).extra[1]).toBe(g.gap);
    }
    const bare = paintFrame({ ...f, info: { ...f.info, root: null } }, S);
    expect(bare.count).toBe(f.count);
  });

  it("colours by class with ring tones for folders, or by branch, or by change", () => {
    const p = paintFrame(f, S);
    const [file, folder] = [f.rowOf(2), f.rowOf(3)];
    expect(rowInst(p, file).fill).toEqual(f32(fillOf(f, file, "class", S.classIds)));
    expect(rowInst(p, file).fill).toEqual(f32(rgba(T.class.video)));
    for (const [a, b] of rowInst(p, folder).fill.map((v, k) => [v, ringFill(1)[k]!])) expect(a).toBeCloseTo(b!, 6);
    const docs = f.rowOf(5);
    expect(f.depth[docs]).toBe(2);
    for (const [a, b] of rowInst(p, docs).fill.map((v, k) => [v, ringFill(2)[k]!])) expect(a).toBeCloseTo(b!, 6);

    const branch = paintFrame(f, { ...S, colorMode: "branch" });
    for (const i of [file, folder, docs, f.rowOf(4)]) expect(rowInst(branch, i).fill).toEqual(f32(branchFill(f.branch![i]!, f.depth[i]!)));
    expect(rowInst(branch, folder).fill).not.toEqual(rowInst(branch, file).fill);

    const tree: SceneNode = { ...SCENE_TREE, delta: 0n, dstat: DStat.CHANGED, children: SCENE_TREE.children!.map((c, k) => ({ ...c, delta: BigInt((k - 1) * 3000), dstat: k === 0 ? DStat.NEW : DStat.CHANGED })) };
    const c = polarScene({ tree }).frame;
    const diff = paintFrame(c, { ...S, colorMode: "branch", diffTiers: [1000, 2000, 5000] });
    expect(rowInst(diff, c.rowOf(3)).fill).toEqual(f32(fillOf(c, c.rowOf(3), "class", S.classIds, [1000, 2000, 5000])));
    expect(rowInst(diff, c.rowOf(3)).fill).toEqual(f32(rgba(T.collapsed)));
    expect(rowInst(diff, c.rowOf(2)).fill).not.toEqual(f32(rgba(T.collapsed)));
  });

  it("stripes free, unknown and waiting space, dots aggregates, dims what the filter left out", () => {
    const p = paintFrame(f, S);
    const free = rowInst(p, f.rowOf(0xc0000001));
    expect(free.fill).toEqual(f32(rgba(T.free.stripeA)));
    expect(free.pattern).toEqual(f32(rgba(T.free.stripeB)));
    expect(free.shape).toEqual([1, Pattern.STRIPES_135, T.geometry.stripeWidth, 0]);
    const pending = rowInst(p, f.rowOf(7));
    expect(pending.shape).toEqual([1, Pattern.STRIPES_135, 8, 1]);
    const small = rowInst(p, f.rowOf((1 << 30) + 3));
    expect(small.shape[1]).toBe(Pattern.DOTS);

    const tree: SceneNode = {
      ...SCENE_TREE,
      children: [
        { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "a", flags: Flag.DIM },
        { key: 0xc0000002, node: -1, kind: Kind.UNKNOWN, size: 300n, flags: Flag.DIM },
        { key: 0xc0000001, node: -1, kind: Kind.FREE, size: 200n },
      ],
    };
    const d = polarScene({ tree }).frame;
    const dp = paintFrame(d, S);
    expect(rowInst(dp, d.rowOf(2)).extra[0]).toBeCloseTo(T.dimOpacity, 6);
    const unknown = rowInst(dp, d.rowOf(0xc0000002));
    expect(unknown.extra[0]).toBeCloseTo(0.55, 6);
    expect(unknown.shape).toEqual([1, Pattern.STRIPES_45, 3, 1]);
    expect(rowInst(dp, d.rowOf(0xc0000001)).extra[0]).toBe(1);
  });

  it("labels only rows with label bits", () => {
    const tree: SceneNode = {
      ...SCENE_TREE,
      children: [
        { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "named", label: Label.NAME | Label.SIZE },
        { key: 3, node: 3, kind: Kind.FILE, size: 300n, name: "silent", label: 0 },
        { key: 4, node: 4, kind: Kind.FILE, size: 200n, name: "radial", label: Label.NAME | Label.SHORT | Label.RADIAL },
      ],
    };
    const p = paintFrame(polarScene({ tree }).frame, S);
    expect(p.labels.map((l) => l.lines[0]!.text)).toEqual(["named", "radial"]);
  });

  it("puts tag markers and compare badges on their anchors", () => {
    const big = polarScene({ width: 800, height: 800, detail: 3 }).frame;
    const p = paintFrame(big, S);
    const a = big.rowOf(2);
    const at = polarGeometry.anchor(big, a, "tag")!;
    expect(p.tags).toEqual([{ x: at.x - 7, y: at.y - 7, tag: 1, alpha: 1 }]);
    const tree: SceneNode = { ...SCENE_TREE, children: [{ key: 2, node: 2, kind: Kind.FILE, size: 600n, name: "new", delta: 600n, dstat: DStat.NEW }] };
    const c = polarScene({ tree, width: 800, height: 800, detail: 3 }).frame;
    const badge = polarGeometry.anchor(c, 0, "badge")!;
    expect(paintFrame(c, S).tags).toEqual([{ x: badge.x - 15, y: badge.y - 7.5, tag: 0, alpha: 1, badge: "new" }]);
  });

  it("reuses a buffer that is big enough and grows past it", () => {
    const first = paintFrame(f, S);
    expect(paintFrame(f, S, first.instances).instances).toBe(first.instances);
    const tiny = new Float32Array(STRIDE * 2);
    const grown = paintFrame(f, S, tiny);
    expect(grown.instances).not.toBe(tiny);
    expect(grown.instances.length).toBeGreaterThanOrEqual((f.count + 1) * STRIDE);
  });

  it("draws an empty frame as the disc alone", () => {
    const { frame: empty } = polarScene({ width: 60, height: 60 });
    expect(empty.count).toBe(0);
    const p = paintFrame(empty, S);
    expect([p.count, p.labels.length, p.tags.length, p.selected]).toEqual([1, 0, 0, -1]);
    expect(paintOverlay(empty, S, HIT_NONE, -1).count).toBe(0);
    expect(paintFrame({ ...empty, info: { ...empty.info, geometry: null } }, S).count).toBe(0);
  });
});

describe("paintOverlay", () => {
  const { frame: f } = polarScene();
  const g = geo(f);
  const selected = paintFrame(f, S).selected;
  const pdf = f.rowOf(6); // depth 3: docs, sub, then the disc above it

  it("finds the selection", () => {
    expect(selected).toBe(f.rowOf(2));
  });

  it("lights the ancestors of the hovered and the selected row, the disc among them", () => {
    const o = paintOverlay(f, { ...S, selectionShadow: false }, pdf, -1);
    // disc, sub, docs, then the hover inset
    expect(o.count).toBe(4);
    expect(inst(o, 0).sector).toEqual([0, 1, 0, discRadius(g)]);
    expect(inst(o, 0).fill).toEqual(f32(haloOverlay()));
    expect([inst(o, 1).sector, inst(o, 2).sector]).toEqual([f.rowOf(3), f.rowOf(5)].map((i) => f32([f.a0[i]! / g.unit, f.a1[i]! / g.unit, f.r0[i]!, f.r1[i]!])));
    expect(inst(o, 3).border).toEqual(f32(rgba(T.state.hoverInset)));
    expect(inst(o, 3).shape[0]).toBe(1);
    // one level: docs alone
    expect(paintOverlay(f, { ...S, haloLevels: 1, hoverHighlight: false }, pdf, -1).count).toBe(1);
    // the selection: its parent is the disc
    const sel = paintOverlay(f, { ...S, hoverHighlight: false }, HIT_NONE, selected);
    expect(inst(sel, 0).fill).toEqual(f32(haloOverlay()));
    expect(inst(sel, 0).sector[1]).toBe(1);
    // the rows lit, for the labels' lit colours
    expect([...o.lit].sort()).toEqual([f.rowOf(3), f.rowOf(5)].sort());
    expect(sel.lit.size).toBe(0);
  });

  it("lifts the selection above its shadow and rings it, grown outward", () => {
    const o = paintOverlay(f, { ...S, haloLevels: 0 }, HIT_NONE, selected);
    const r = Math.round(T.geometry.selectRingWidth);
    expect(o.count).toBe(4);
    expect(inst(o, 0).shape[1]).toBe(Pattern.SHADOW);
    // the row itself again, as the picture has it, so the shadow falls only around it
    expect(inst(o, 1)).toEqual(rowInst(paintFrame(f, S), selected));
    expect([inst(o, 0).extra[2], inst(o, 2).extra[2], inst(o, 3).extra[2]]).toEqual([-2 * r, -2 * r, -r]);
    expect(inst(o, 2).border).toEqual(f32(rgba(T.state.selectOuter)));
    expect(inst(o, 3).border).toEqual(f32(rgba(T.state.selectRing)));
    const flat = paintOverlay(f, { ...S, haloLevels: 0, selectionShadow: false }, HIT_NONE, selected);
    expect([flat.count, inst(flat, 0).extra[2]]).toEqual([2, -2 * r]);
  });

  it("paints the disc, the halo, the inset and the shadow in the theme on screen", () => {
    try {
      applyTheme("light");
      const L = themes.light;
      expect(inst(paintFrame(f, S), 0).fill).toEqual(f32(rgba(L.visual.center.bg)));
      const o = paintOverlay(f, { ...S, selectionShadow: false }, pdf, -1);
      expect(inst(o, 0).fill).toEqual(f32(rgba(L.visual.haloOverlay)));
      expect(inst(o, 3).border).toEqual(f32(rgba(L.treemap.state.hoverInset)));
      expect(inst(paintOverlay(f, { ...S, haloLevels: 0 }, HIT_NONE, selected), 0).fill).toEqual(f32(rgba(L.treemap.state.selectShadow)));
    } finally {
      applyTheme("dark");
    }
    expect(inst(paintFrame(f, S), 0).fill).toEqual(f32(rgba(themes.dark.visual.center.bg)));
  });

  it("outlines the disc when it is hovered", () => {
    const o = paintOverlay(f, { ...S, haloLevels: 0 }, HIT_ROOT, -1);
    expect(o.count).toBe(1);
    expect(inst(o, 0).sector).toEqual([0, 1, 0, discRadius(g)]);
    expect(inst(o, 0).fill).toEqual([0, 0, 0, 0]);
  });

  it("stays within its limit at every hover and halo depth", () => {
    for (const haloLevels of [0, 1, 3, 6, Infinity]) {
      const s = { ...S, haloLevels };
      const limit = overlayLimit(haloLevels, g.rings);
      for (const h of [HIT_NONE, HIT_ROOT, ...Array.from({ length: f.count }, (_, i) => i)]) {
        const o = paintOverlay(f, s, h, selected);
        expect(o.count).toBeLessThanOrEqual(limit);
        expect(paintFrame(f, s).count + o.count).toBeLessThanOrEqual(f.count + 1 + limit);
      }
    }
  });
});

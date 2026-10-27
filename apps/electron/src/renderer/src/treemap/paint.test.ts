import { themes, tokens } from "@design/tokens";
import { describe, expect, it } from "vitest";

import { applyTheme } from "@/lib/theme";
import type { FrameInfo } from "@/protocol/generated";
import type { Column } from "@/protocol/rpc";

import { branchFill } from "@/visual/colors";
import { frameInfo } from "@/visual/testing/fixtures";

import { decodeDraw, type DrawFrame, Flag, Kind, Label } from "./frame";
import { fillOf, labelColors, paint, type PaintOptions, Pattern, type RGBA, rgba, STRIDE } from "./paint";

const T = tokens.treemap;
/** Every tier of every 「顶层分支」 hue, and the neutral one. */
const BRANCH_FILLS: string[] = [
  ...Object.entries(tokens.visual.branch).flatMap(([k, v]) => (k === "neutral" ? [] : Object.values(v as Record<string, string>))),
  tokens.visual.branch.neutral,
];

// ------------------------------------------------------------------ reference WCAG maths
// Independent of paint.ts: relative luminance and contrast ratio as WCAG 2.x defines them.

function luminance([r, g, b]: RGBA): number {
  const f = (c: number) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

function contrast(a: RGBA, b: RGBA): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((p, q) => q - p) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

const grey = (v: number): RGBA => [v / 255, v / 255, v / 255, 1];

// ------------------------------------------------------------------ frames

interface Row {
  kind: number;
  rect?: [number, number, number, number];
  depth?: number;
  parent?: number;
  cls?: number;
  label?: number;
  size?: bigint;
  count?: number;
  flags?: number;
  tag?: number;
  tone?: number;
  branch?: number;
  name?: string;
}

const INFO: FrameInfo = frameInfo({ breadcrumb: [], height: 300, layout_ms: 0, scanned: 0, width: 300 });

/** A DrawFrame straight from typed columns (the wire codec is covered by frame.test.ts). */
function frameOf(rows: Row[]): DrawFrame {
  const enc = rows.map((r) => new TextEncoder().encode(r.name ?? ""));
  const names = new Uint8Array(enc.reduce((n, b) => n + b.length, 0));
  let at = 0;
  for (const b of enc) {
    names.set(b, at);
    at += b.length;
  }
  const columns: Record<string, Column> = {
    key: Uint32Array.from(rows, (_, i) => i + 1),
    node: Int32Array.from(rows, (_, i) => i + 1),
    parent: Int32Array.from(rows, (r) => r.parent ?? -1),
    kind: Uint8Array.from(rows, (r) => r.kind),
    depth: Uint8Array.from(rows, (r) => r.depth ?? 1),
    x: Int32Array.from(rows, (r) => r.rect?.[0] ?? 0),
    y: Int32Array.from(rows, (r) => r.rect?.[1] ?? 0),
    w: Int32Array.from(rows, (r) => r.rect?.[2] ?? 10),
    h: Int32Array.from(rows, (r) => r.rect?.[3] ?? 10),
    cls: Uint8Array.from(rows, (r) => r.cls ?? 0),
    label: Uint8Array.from(rows, (r) => r.label ?? 0),
    size: BigInt64Array.from(rows, (r) => r.size ?? 0n),
    count: Uint32Array.from(rows, (r) => r.count ?? 0),
    flags: Uint16Array.from(rows, (r) => r.flags ?? 0),
    tag: Uint8Array.from(rows, (r) => r.tag ?? 0),
    tone: Uint8Array.from(rows, (r) => r.tone ?? 0),
    branch: Uint8Array.from(rows, (r) => r.branch ?? 0),
    name_len: Uint32Array.from(enc, (b) => b.length),
  };
  return decodeDraw(
    { header: { stream: "view:v1", seq: 1, final: true, layout: "drawlist.v1", count: rows.length, columns: {} }, columns, blobs: { names } },
    { ...INFO, rows: rows.length },
  );
}

const expectRGBA = (actual: ArrayLike<number>, expected: RGBA) => {
  expect(actual).toHaveLength(4);
  for (let k = 0; k < 4; k++) expect(actual[k]).toBeCloseTo(expected[k]!, 5);
};

// ------------------------------------------------------------------ rgba

describe("rgba", () => {
  it("parses #RRGGBB as opaque and #RRGGBBAA with alpha", () => {
    expect(rgba("#FFFFFF")).toEqual([1, 1, 1, 1]);
    expect(rgba("#000000")).toEqual([0, 0, 0, 1]);
    expectRGBA(rgba("#FF8000"), [1, 128 / 255, 0, 1]);
    expectRGBA(rgba("#00000033"), [0, 0, 0, 0x33 / 255]);
    expectRGBA(rgba("FFFFFF"), [1, 1, 1, 1]);
  });

  it("multiplies an explicit alpha into the colour's own alpha", () => {
    expectRGBA(rgba("#FFFFFF", 0.5), [1, 1, 1, 0.5]);
    expectRGBA(rgba(T.label.lightSub, 0.5), [1, 1, 1, (0xdb / 255) * 0.5]);
    // a different alpha is a different colour, not a stale cache hit
    expect(rgba("#123456", 0.25)[3]).toBeCloseTo(0.25);
    expect(rgba("#123456")[3]).toBe(1);
  });
});

// ------------------------------------------------------------------ labelColors

describe("labelColors (UI-TM-19)", () => {
  const light: [string, string] = [T.label.light, T.label.lightSub];
  const dark: [string, string] = [T.label.dark, T.label.darkSub];

  it("puts dark text on light fills and light text on dark fills", () => {
    expect(labelColors([1, 1, 1, 1])).toEqual(dark);
    expect(labelColors([0, 0, 0, 1])).toEqual(light);
    expect(labelColors(rgba(T.class.archive))).toEqual(dark); // bright yellow
    expect(labelColors(rgba(T.time[0]))).toEqual(dark); // pale mint
    expect(labelColors(rgba(T.class.other))).toEqual(light); // near-black slate
    expect(labelColors(rgba(T.class.doc))).toEqual(light); // deep indigo
    expect(labelColors(rgba(T.collapsed))).toEqual(light);
  });

  it("always picks the candidate with the higher WCAG contrast", () => {
    const fills: RGBA[] = [
      ...Object.values(T.class).map((c) => rgba(c)),
      ...Object.values(T.time).map((c) => rgba(c)),
      ...BRANCH_FILLS.map((c) => rgba(c)),
      ...Array.from({ length: 256 }, (_, v) => grey(v)),
      [1, 0, 0, 1],
      [0, 1, 0, 1],
      [0, 0, 1, 1],
    ];
    const L = rgba(T.label.light);
    const D = rgba(T.label.dark);
    for (const fill of fills) {
      const [main, sub] = labelColors(fill);
      const best = contrast(fill, L) >= contrast(fill, D) ? light : dark;
      expect([main, sub]).toEqual(best);
      // the better of white / near-black is never below ~4.4:1 against any opaque fill
      expect(Math.max(contrast(fill, L), contrast(fill, D))).toBeGreaterThan(4.4);
    }
  });

  it("keeps the 「顶层分支」 labels at 4.5:1 or better on every tier", () => {
    for (const hex of BRANCH_FILLS) {
      const fill = rgba(hex);
      const [main] = labelColors(fill);
      expect(contrast(fill, rgba(main)), hex).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("flips exactly once along a black-to-white ramp", () => {
    const picks = Array.from({ length: 256 }, (_, v) => labelColors(grey(v))[0]);
    const flips = picks.filter((p, i) => i > 0 && p !== picks[i - 1]).length;
    expect(flips).toBe(1);
    expect(picks[0]).toBe(T.label.light);
    expect(picks[255]).toBe(T.label.dark);
  });

  it("returns the secondary colour matching the primary", () => {
    for (let v = 0; v < 256; v += 15) {
      const [main, sub] = labelColors(grey(v));
      expect(sub).toBe(main === T.label.light ? T.label.lightSub : T.label.darkSub);
    }
  });
});

// ------------------------------------------------------------------ fillOf

describe("fillOf", () => {
  const classIds = ["other", "video", "image", "not-a-token-class"];
  const f = frameOf([
    { kind: Kind.FILE, cls: 1, tone: 0 }, // 0 video file, newest
    { kind: Kind.SMALL, cls: 2, tone: 3 }, // 1 image small-items block
    { kind: Kind.COLLAPSED, cls: 1, tone: 1 }, // 2 collapsed folder
    { kind: Kind.MORE, cls: 2 }, // 3 "+N items"
    { kind: Kind.FILE, cls: 99 }, // 4 class index out of range
    { kind: Kind.FILE, cls: 3 }, // 5 class without a colour token
    { kind: Kind.FILE, tone: 9 }, // 6 tone out of range
    { kind: Kind.FILE, cls: 1, tone: 1 },
    { kind: Kind.FILE, cls: 1, tone: 2 },
    { kind: Kind.FILE, cls: 1, tone: 3 },
    { kind: Kind.FILE, cls: 1, tone: 4 }, // 10 oldest
  ]);

  it("colours files and small-items blocks by class", () => {
    expect(fillOf(f, 0, "class", classIds)).toEqual(rgba(T.class.video));
    expect(fillOf(f, 1, "class", classIds)).toEqual(rgba(T.class.image));
  });

  it("falls back to the 'other' colour for unknown classes", () => {
    expect(fillOf(f, 4, "class", classIds)).toEqual(rgba(T.class.other));
    expect(fillOf(f, 5, "class", classIds)).toEqual(rgba(T.class.other));
    expect(fillOf(f, 0, "class", [])).toEqual(rgba(T.class.other));
  });

  it("always paints collapsed folders and +N blocks in the collapsed colour", () => {
    for (const mode of ["class", "flat", "time"] as const) {
      expect(fillOf(f, 2, mode, classIds)).toEqual(rgba(T.collapsed));
      expect(fillOf(f, 3, mode, classIds)).toEqual(rgba(T.collapsed));
    }
  });

  it("paints everything else alike in flat mode", () => {
    for (const i of [0, 1, 4, 5, 6]) expect(fillOf(f, i, "flat", classIds)).toEqual(rgba(T.class.other));
  });

  it("colours files and small-items blocks by age in time mode", () => {
    expect(fillOf(f, 0, "time", classIds)).toEqual(rgba(T.time[0]));
    expect(fillOf(f, 1, "time", classIds)).toEqual(rgba(T.time[3]));
    expect(fillOf(f, 6, "time", classIds)).toEqual(rgba(T.time[4])); // unknown tone = oldest
    // newer is brighter: luminance strictly falls from tone 0 to tone 4
    const lum = [0, 7, 8, 9, 10].map((i) => luminance(fillOf(f, i, "time", classIds)));
    for (let k = 1; k < lum.length; k++) expect(lum[k]!).toBeLessThan(lum[k - 1]!);
  });

  it("colours files, collapsed folders and aggregates by top-level branch and depth in branch mode", () => {
    const g = frameOf([
      { kind: Kind.FILE, cls: 1, depth: 1, branch: 3 },
      { kind: Kind.COLLAPSED, depth: 2, branch: 3 },
      { kind: Kind.SMALL, depth: 3, branch: 5 },
      { kind: Kind.MORE, depth: 6, branch: 8 },
      { kind: Kind.FILE, depth: 2, branch: 0 }, // a row without a branch: neutral
    ]);
    expect(fillOf(g, 0, "branch", classIds)).toEqual(branchFill(3, 1));
    expect(fillOf(g, 1, "branch", classIds)).toEqual(branchFill(3, 2));
    expect(fillOf(g, 2, "branch", classIds)).toEqual(branchFill(5, 3));
    expect(fillOf(g, 3, "branch", classIds)).toEqual(branchFill(8, 4));
    expect(fillOf(g, 4, "branch", classIds)).toEqual(rgba(tokens.visual.branch.neutral));
    // the hues differ between branches and the tiers between depths
    expect(branchFill(3, 1)).not.toEqual(branchFill(3, 2));
    expect(branchFill(3, 1)).not.toEqual(branchFill(4, 1));
    // a frame without the branch column (a server before visuals.v1) is neutral
    expect(fillOf({ kind: g.kind, cls: g.cls, tone: g.tone, delta: null }, 0, "branch", classIds)).toEqual(rgba(tokens.visual.branch.neutral));
  });

  it("lets the compare colour scale win over branch colouring", () => {
    const g = frameOf([{ kind: Kind.FILE, branch: 2 }]);
    const withDelta = { ...g, delta: BigInt64Array.of(5n << 30n) };
    expect(fillOf(withDelta, 0, "branch", classIds, [1, 2, 3])).not.toEqual(branchFill(2, 1));
  });

  it("gives distinct classes distinct colours", () => {
    const ids = Object.keys(T.class);
    const g = frameOf(ids.map((_, i) => ({ kind: Kind.FILE, cls: i })));
    const fills = ids.map((_, i) => fillOf(g, i, "class", ids).join(","));
    expect(new Set(fills).size).toBe(ids.length);
  });
});

// ------------------------------------------------------------------ paint

describe("paint", () => {
  //   0 root folder           0,0 300x200 (ON_PATH)
  //   ├ 1 movie.mkv           2,20 150x100 (selected, red tag)
  //   ├ 2 37 small items      154,20 60x40 (dimmed)
  //   └ 5 pending             216,20 80x60
  //   3 free space            0,200 300x50
  //   4 unknown space         0,250 300x50
  const rows: Row[] = [
    { kind: Kind.FOLDER, rect: [0, 0, 300, 200], depth: 1, label: Label.NAME, size: 5000n, flags: Flag.ON_PATH, name: "root" },
    {
      kind: Kind.FILE,
      rect: [2, 20, 150, 100],
      depth: 2,
      parent: 0,
      cls: 1,
      label: Label.NAME | Label.SIZE,
      size: 4000n,
      flags: Flag.SELECTED,
      tag: 1,
      name: "movie.mkv",
    },
    { kind: Kind.SMALL, rect: [154, 20, 60, 40], depth: 2, parent: 0, size: 500n, count: 37, flags: Flag.DIM },
    { kind: Kind.FREE, rect: [0, 200, 300, 50], size: 2000n },
    { kind: Kind.UNKNOWN, rect: [0, 250, 300, 50], size: 100n },
    { kind: Kind.PENDING, rect: [216, 20, 80, 60], depth: 2, parent: 0, label: Label.NAME | Label.SIZE, size: 0n, name: "later" },
  ];
  const opts: PaintOptions = {
    dpr: 1,
    colorMode: "class",
    classIds: ["other", "video"],
    hovered: -1,
    scanning: true,
    formatSize: (b) => `${b} B`,
    formatInt: (n) => `#${n}`,
  };
  const slot = (p: { instances: Float32Array }, k: number) => p.instances.subarray(k * STRIDE, (k + 1) * STRIDE);

  it("refills the previous instance array when it is large enough", () => {
    const f = frameOf(rows);
    const first = paint(f, opts);
    const again = paint(f, { ...opts, hovered: 1 }, first.instances); // a hover repaint
    expect(again.instances).toBe(first.instances);
    const fresh = paint(f, { ...opts, hovered: 1 });
    expect(Array.from(again.instances.subarray(0, again.count * STRIDE))).toEqual(Array.from(fresh.instances.subarray(0, fresh.count * STRIDE)));
    const small = new Float32Array(STRIDE);
    expect(paint(f, opts, small).instances).not.toBe(small); // too small: a new one
  });

  it("emits one instance per block, two per folder, three for the selection", () => {
    const p = paint(frameOf(rows), opts);
    expect(p.count).toBe(2 + 1 + 1 + 1 + 1 + 1 + 3);
    expect(p.instances.length).toBeGreaterThanOrEqual(p.count * STRIDE);
  });

  it("writes the rectangle and fill of each block", () => {
    const p = paint(frameOf(rows), opts);
    const file = slot(p, 2);
    expect(Array.from(file.subarray(0, 4))).toEqual([2, 20, 150, 100]);
    expectRGBA(file.subarray(4, 8), rgba(T.class.video));
    expect(file[19]).toBe(Pattern.NONE);
    expect(file[22]).toBe(1);
  });

  it("draws an expanded folder as a box plus a header lit while on the selection path", () => {
    const p = paint(frameOf(rows), opts);
    const box = slot(p, 0);
    const header = slot(p, 1);
    expect(Array.from(box.subarray(0, 4))).toEqual([0, 0, 300, 200]);
    expectRGBA(box.subarray(4, 8), rgba(T.level[0].bg));
    expectRGBA(box.subarray(8, 12), rgba(T.level[0].border));
    expect(box[18]).toBe(1); // border width at dpr 1
    expectRGBA(header.subarray(4, 8), rgba(T.halo.bg));
    expect(header[17]).toBe(1); // top corners only
    expect(header[3]).toBe(T.geometry.headerHeight);
    const title = p.texts.find((t) => t.text === "root")!;
    expect(title.color).toBe(T.halo.text);
    expect(title.sub?.text).toBe("5000 B");
    expect(title.font.startsWith("600 ")).toBe(true);
  });

  it("uses the level colours for a folder off the selection path, and the halo for a hovered block's ancestors", () => {
    const f = frameOf(rows);
    f.flags[0] = 0;
    const idle = paint(f, opts);
    expectRGBA(slot(idle, 1).subarray(4, 8), rgba(T.level[0].headerBg));
    expect(idle.texts.find((t) => t.text === "root")!.color).toBe(T.level[0].headerText);

    const hovered = paint(f, { ...opts, hovered: 1 });
    expectRGBA(slot(hovered, 1).subarray(4, 8), rgba(T.halo.bg));
    expect(hovered.count).toBe(idle.count + 1); // the hover inset
  });

  it("labels files with name and size in a contrasting colour", () => {
    const p = paint(frameOf(rows), opts);
    const [main, sub] = labelColors(rgba(T.class.video));
    const name = p.texts.find((t) => t.text === "movie.mkv")!;
    const size = p.texts.find((t) => t.text === "4000 B")!;
    expect(name.color).toBe(main);
    expect(size.color).toBe(sub);
    expect(size.y).toBeGreaterThan(name.y);
    expect(name.maxWidth).toBeLessThan(150);
  });

  it("labels aggregate and special blocks with their own text", () => {
    const p = paint(frameOf(rows), opts);
    const texts = p.texts.map((t) => t.text);
    expect(texts).toContain("#37 个小项");
    expect(texts).toContain("500 B");
    expect(texts).toContain("可用空间");
    expect(texts).toContain("2000 B");
    expect(texts).toContain("未知空间 · 尚未扫描");
    expect(texts).toContain("later");
    expect(texts).toContain("等待扫描…");

    const done = paint(frameOf(rows), { ...opts, scanning: false }).texts.map((t) => t.text);
    expect(done).toContain("未知空间");
    expect(done).not.toContain("未知空间 · 尚未扫描");
  });

  it("dims flagged blocks and dots aggregates", () => {
    const p = paint(frameOf(rows), opts);
    const small = slot(p, 3);
    expect(small[22]).toBeCloseTo(T.dimOpacity, 5);
    expect(small[19]).toBe(Pattern.DOTS);
    for (const t of p.texts.filter((t) => t.text === "#37 个小项" || t.text === "500 B")) expect(t.alpha).toBe(T.dimOpacity);
  });

  it("stripes free, unknown and pending space, dashing all but free space", () => {
    const p = paint(frameOf(rows), opts);
    const [free, unknown, pending] = [slot(p, 4), slot(p, 5), slot(p, 6)];
    expect(free[19]).toBe(Pattern.STRIPES_135);
    expect(free[21]).toBe(0);
    expectRGBA(free.subarray(4, 8), rgba(T.free.stripeA));
    expect(unknown[19]).toBe(Pattern.STRIPES_45);
    expect(unknown[21]).toBe(1);
    expect(pending[19]).toBe(Pattern.STRIPES_135);
    expect(pending[21]).toBe(1);
    expect(p.texts.find((t) => t.text === "可用空间")!.color).toBe(T.free.text);
  });

  it("draws the selection ring last, around the selected block", () => {
    const p = paint(frameOf(rows), opts);
    const r = T.geometry.selectRingWidth;
    const ring = slot(p, p.count - 1);
    const outer = slot(p, p.count - 2);
    const shadow = slot(p, p.count - 3);
    expect(Array.from(ring.subarray(0, 4))).toEqual([2 - r, 20 - r, 150 + 2 * r, 100 + 2 * r]);
    expectRGBA(ring.subarray(8, 12), rgba(T.state.selectRing));
    expect(ring[18]).toBe(r);
    expect(Array.from(outer.subarray(0, 4))).toEqual([2 - 2 * r, 20 - 2 * r, 150 + 4 * r, 100 + 4 * r]);
    expect(shadow[19]).toBe(Pattern.SHADOW);
    // dropped 12 px, with a hole the size of the selected block where it lies: the shadow falls
    // around the selection, not on it or on its children (UI-TM-28)
    expect(Array.from(shadow.subarray(0, 4))).toEqual([2 - 2 * r, 20 - 2 * r + 12, 150 + 4 * r, 100 + 4 * r]);
    expect(Array.from(shadow.subarray(8, 12))).toEqual([0, -12, 150, 100]);
    expect(shadow[12]).toBe(T.geometry.fileRadius);
    // a selected folder: its own radius, at the pixel ratio
    const folder = frameOf([{ ...rows[0]!, flags: Flag.SELECTED }]);
    const p2 = paint(folder, { ...opts, dpr: 2 });
    const s2 = slot(p2, p2.count - 3);
    expect(Array.from(s2.subarray(8, 12))).toEqual([0, -24, 300, 200]);
    expect(s2[12]).toBe(2 * T.geometry.folderRadius);
    // without the shadow, no instance for it
    expect(paint(frameOf(rows), { ...opts, selectionShadow: false }).count).toBe(p.count - 1);
  });

  it("places tag dots on tagged blocks big enough to carry them", () => {
    const p = paint(frameOf(rows), opts);
    expect(p.tags).toEqual([{ x: 2 + 150 - 18, y: 24, tag: 1, alpha: 1 }]);
    const tiny = frameOf([{ kind: Kind.FILE, rect: [0, 0, 20, 20], tag: 3 }]);
    expect(paint(tiny, opts).tags).toEqual([]);
  });

  it("fills blocks by branch and keeps the level colours of folder boxes in branch mode", () => {
    const f = frameOf(rows.map((r, i) => ({ ...r, branch: i === 3 || i === 4 ? 0 : 2 })));
    const p = paint(f, { ...opts, colorMode: "branch" });
    const box = slot(p, 0);
    expectRGBA(box.subarray(4, 8), rgba(T.level[0].bg)); // the root folder's box
    expectRGBA(slot(p, 2).subarray(4, 8), branchFill(2, 2)); // movie.mkv at depth 2
    expectRGBA(slot(p, 3).subarray(4, 8), branchFill(2, 2)); // the small-items block
    const [main] = labelColors(branchFill(2, 2));
    expect(p.texts.find((t) => t.text === "movie.mkv")!.color).toBe(main);
    // free and unknown space keep their stripes
    expectRGBA(slot(p, 4).subarray(4, 8), rgba(T.free.stripeA));
  });

  it("scales geometry with the device pixel ratio", () => {
    const f = frameOf(rows);
    const p1 = paint(f, opts);
    const p2 = paint(f, { ...opts, dpr: 2 });
    expect(slot(p2, 1)[3]).toBe(2 * slot(p1, 1)[3]!); // header height
    expect(slot(p2, 0)[18]).toBe(2); // border width
  });

  it("paints in the colours of the theme on screen (W7 外观 · 主题)", () => {
    const f = frameOf(rows);
    const dark = paint(f, { ...opts, hovered: 2 });
    expectRGBA(slot(dark, dark.count - 3).subarray(4, 8), rgba(themes.dark.treemap.state.selectShadow)); // the selection's shadow
    try {
      applyTheme("light");
      const L = themes.light.treemap;
      const p = paint(f, { ...opts, hovered: 2 });
      expectRGBA(slot(p, 0).subarray(4, 8), rgba(L.level[0].bg));
      expectRGBA(slot(p, 1).subarray(4, 8), rgba(L.halo.bg));
      expectRGBA(slot(p, 2).subarray(4, 8), rgba(L.class.video));
      expect(p.texts.find((t) => t.text === "root")!.color).toBe(L.halo.text);
      expect(p.texts.find((t) => t.text === "可用空间")!.color).toBe(L.free.text);
      expectRGBA(slot(p, p.count - 4).subarray(8, 12), rgba(L.state.hoverInset));
      expectRGBA(slot(p, p.count - 3).subarray(4, 8), rgba(themes.light.treemap.state.selectShadow));
      expectRGBA(slot(p, p.count - 1).subarray(8, 12), rgba(L.state.selectRing));
      expect(fillOf(f, 1, "flat", opts.classIds)).toEqual(rgba(L.flat.file));
      expect(L.flat.file).toBe(L.class.other);
    } finally {
      applyTheme("dark");
    }
    expectRGBA(slot(paint(f, opts), 2).subarray(4, 8), rgba(T.class.video));
  });

  it("paints every row of a larger frame", () => {
    const many = frameOf(Array.from({ length: 100 }, (_, i) => ({ kind: Kind.FILE, rect: [i * 3, 0, 3, 3] as [number, number, number, number] })));
    const p = paint(many, opts);
    expect(p.count).toBe(100);
    expect(Array.from(slot(p, 99).subarray(0, 4))).toEqual([297, 0, 3, 3]);
  });
});

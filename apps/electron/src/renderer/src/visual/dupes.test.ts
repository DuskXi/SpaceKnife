// The 「重复」 colouring and the lit duplicate group (docs/dupes.md §10.2, §10.3).

import { describe, expect, it } from "vitest";

import type { DupesStatus } from "@/protocol/generated";
import { palette } from "@/lib/theme";
import { dupeStyle, Dup, fillOf, paint, Pattern, rgba, STRIDE, stripeOn } from "@/treemap/paint";
import { applyTheme } from "@/lib/theme";
import { dupesOf, dupesSummary, getDupes, isRunning, onDupesChanged, onDupesProgress, setDupes } from "@/state/dupes";
import { nextColor } from "@/state/visuals";

import { fillFor } from "./flame/paint";
import { sectorFill } from "./flamePolar/paint";
import { Kind } from "./frame";
import { classMarks, rowMarks } from "./overlays";
import { encodeFrame, PathRecorder, polarScene, rectScene } from "./testing/fixtures";
import { treemapGeometry } from "./treemap/geometry";
import type { PaintSettings, RectFrame } from "./types";

const recorder = () => new PathRecorder() as unknown as Path2D;
const opsOf = (p: Path2D) => (p as unknown as PathRecorder).ops;

type R = { kind: number; parent: number; cls: number; geo: readonly [number, number, number, number] };
const ROWS: R[] = [
  { kind: Kind.FOLDER, parent: -1, cls: 255, geo: [0, 0, 300, 100] },
  { kind: Kind.FILE, parent: 0, cls: 1, geo: [2, 20, 90, 70] },
  { kind: Kind.FILE, parent: 0, cls: 1, geo: [100, 20, 90, 70] },
  { kind: Kind.FILE, parent: 0, cls: 2, geo: [200, 20, 90, 70] },
  { kind: Kind.COLLAPSED, parent: -1, cls: 255, geo: [300, 0, 100, 100] },
];
const frameOf = () =>
  encodeFrame<RectFrame>(
    "drawlist.v1",
    ROWS.map((r, i) => ({ key: i + 1, node: i + 1, parent: r.parent, kind: r.kind, depth: 1, geo: r.geo, cls: r.cls, size: 1n })),
    { width: 600, height: 200 },
  ).frame;

describe("the 「重复」 colouring", () => {
  const D = () => palette().treemap.dupe;
  const one = (kind: number, dup: number) => ({ kind: Uint8Array.of(kind), dup: Uint8Array.of(dup) });

  it("paints files by their state: extra copies striped magenta, the kept one with a rim", () => {
    const extra = dupeStyle(one(Kind.FILE, Dup.EXTRA), 0);
    expect(extra.fill).toEqual(rgba(D().share[4]));
    expect(extra.pattern).toBe(Pattern.STRIPES_45);
    const keep = dupeStyle(one(Kind.FILE, Dup.KEEP), 0);
    expect(keep.rim).toEqual(rgba(D().keepRim));
    expect(keep.pattern).toBe(Pattern.NONE);
    const pending = dupeStyle(one(Kind.FILE, Dup.PENDING), 0);
    expect(pending.fill).toEqual(rgba(D().pending));
    expect(pending.pattern).toBe(Pattern.STRIPES_135); // never called a duplicate before it is checked
    expect(dupeStyle(one(Kind.FILE, Dup.SHARED), 0).fill).toEqual(rgba(D().shared));
    expect(dupeStyle(one(Kind.FILE, Dup.NONE), 0).fill).toEqual(rgba(palette().treemap.class.other));
  });

  it("draws its stripes in a shade of the fill, so labels on them stay readable in both themes", () => {
    for (const theme of ["dark", "light"] as const) {
      applyTheme(theme);
      for (const v of [Dup.EXTRA, Dup.PENDING]) {
        const st = dupeStyle(one(Kind.FILE, v), 0);
        expect(st.patternColor).toEqual(stripeOn(st.fill));
        expect(st.patternColor[3]).toBe(st.fill[3]); // opaque like the fill: the shader paints stripes over nothing
        const diff = Math.max(...[0, 1, 2].map((k) => Math.abs(st.patternColor[k]! - st.fill[k]!)));
        expect(diff).toBeGreaterThan(0.02);
        expect(diff).toBeLessThan(0.25);
      }
    }
    applyTheme("dark");
  });

  it("paints folders and blocks by the share of extra copies, in four tiers", () => {
    for (const tier of [1, 2, 3, 4] as const) {
      expect(dupeStyle(one(Kind.COLLAPSED, tier), 0).fill).toEqual(rgba(D().share[tier]));
      expect(dupeStyle(one(Kind.FOLDER, tier), 0).fill).toEqual(rgba(D().share[tier]));
    }
    expect(dupeStyle(one(Kind.SMALL, 0), 0).fill).toEqual(rgba(palette().treemap.collapsed));
  });

  it("is what fillOf gives in that mode, and nothing without the column", () => {
    const f = { kind: Uint8Array.of(Kind.FILE), cls: Uint8Array.of(0), tone: Uint8Array.of(0), delta: null, dup: Uint8Array.of(Dup.EXTRA) };
    expect(fillOf(f, 0, "dupes", ["other"])).toEqual(rgba(D().share[4]));
    expect(fillOf({ ...f, dup: null }, 0, "dupes", ["other"])).toEqual(rgba(palette().treemap.class.other));
  });

  it("comes after the four colour modes in the C cycle, only while results exist", () => {
    expect(nextColor("branch")).toBe("class");
    expect(nextColor("branch", true)).toBe("dupes");
    expect(nextColor("dupes", true)).toBe("class");
    expect(nextColor("dupes", false)).toBe("class");
  });
});

describe("the 「重复」 colouring in each picture", () => {
  const D = () => palette().treemap.dupe;
  const settings = (patch: Partial<PaintSettings> = {}): PaintSettings => ({
    colorMode: "dupes",
    classIds: ["other", "video", "doc", "archive"],
    classColors: [],
    scanning: false,
    formatSize: (b) => `${b} B`,
    formatInt: (n) => `#${n}`,
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

  it("treemap: extra copies striped at 45°, copies being checked at 135°, the kept one with a rim", () => {
    const f = frameOf();
    f.dup = Uint8Array.of(2, Dup.EXTRA, Dup.KEEP, Dup.PENDING, 4);
    const p = paint(f, { dpr: 1, colorMode: "dupes", classIds: ["other", "doc", "video"], hovered: -1, scanning: false, formatSize: (b) => `${b} B`, formatInt: (n) => `#${n}` });
    // instances: the folder's box and header, then one per block
    const inst = (k: number) => p.instances.subarray(k * STRIDE, (k + 1) * STRIDE);
    const [extra, keep, pending, collapsed] = [inst(2), inst(3), inst(4), inst(5)];
    expect(Array.from(extra.subarray(4, 8))).toEqual(rgba(D().share[4]).map((v) => Math.fround(v)));
    expect(extra[19]).toBe(Pattern.STRIPES_45);
    expect(keep[19]).toBe(Pattern.NONE);
    expect(Array.from(keep.subarray(8, 12))).toEqual(rgba(D().keepRim).map((v) => Math.fround(v)));
    expect(keep[18]).toBeGreaterThanOrEqual(1);
    expect(pending[19]).toBe(Pattern.STRIPES_135);
    // a collapsed folder in its tier of extra bytes
    expect(Array.from(collapsed.subarray(4, 8))).toEqual(rgba(D().share[4]).map((v) => Math.fround(v)));
  });

  it("flame: folder bars become a heat map of extra copies, files keep their state", () => {
    const { frame: f } = rectScene({ layout: "flame.v1" });
    const folder = f.rowOf(3);
    const file = f.rowOf(2);
    f.dup = new Uint8Array(f.count);
    f.dup[folder] = 3;
    f.dup[file] = Dup.SHARED;
    expect(fillFor(f, folder, settings())).toEqual(rgba(D().share[3]));
    expect(fillFor(f, file, settings())).toEqual(rgba(D().shared));
    // another colouring leaves the column alone
    expect(fillFor(f, file, settings({ colorMode: "class" }))).not.toEqual(rgba(D().shared));
  });

  it("polar: the same rules on the rings", () => {
    const { frame: f } = polarScene();
    const folder = f.rowOf(3);
    const file = f.rowOf(2);
    f.dup = new Uint8Array(f.count);
    f.dup[folder] = 1;
    f.dup[file] = Dup.EXTRA;
    expect(sectorFill(f, folder, settings())).toEqual(rgba(D().share[1]));
    expect(sectorFill(f, file, settings())).toEqual(rgba(D().share[4]));
    f.dup[file] = Dup.NONE;
    expect(sectorFill(f, file, settings())).toEqual(rgba(palette().treemap.class.other));
  });

  it("flame and polar files keep the stripes and the kept copy's rim", async () => {
    const { paintFlame } = await import("./flame/paint");
    const { frame: f } = rectScene({ layout: "flame.v1" });
    const file = f.rowOf(2);
    f.dup = new Uint8Array(f.count);
    f.dup[file] = Dup.EXTRA;
    const slot = (p: { instances: Float32Array }, k: number) => p.instances.subarray(k * STRIDE, (k + 1) * STRIDE);
    const extra = slot(paintFlame(f, settings()), file + 1);
    expect(extra[19]).toBe(Pattern.STRIPES_45);
    f.dup[file] = Dup.KEEP;
    const keep = slot(paintFlame(f, settings()), file + 1);
    expect(keep[19]).toBe(Pattern.NONE);
    expect(Array.from(keep.subarray(8, 12))).toEqual(rgba(D().keepRim).map((v) => Math.fround(v)));
    expect(keep[18]).toBeGreaterThanOrEqual(1);
    // other colourings never stripe a file
    expect(slot(paintFlame(f, settings({ colorMode: "class" })), file + 1)[19]).toBe(Pattern.NONE);
  });

  it("paints everything as not compared while the frame has no column", () => {
    const { frame: f } = rectScene({ layout: "flame.v1" });
    expect(f.dup).toBeNull();
    expect(fillFor(f, f.rowOf(2), settings())).toEqual(rgba(palette().treemap.class.other));
  });
});

describe("lighting a duplicate group in every pane", () => {
  it("rowMarks lights exactly what classMarks lights for the same rows", () => {
    const f = frameOf();
    const a = classMarks(treemapGeometry, f, 1, -1, 1, recorder);
    const b = rowMarks(treemapGeometry, f, (i) => f.cls[i] === 1 && f.kind[i] === Kind.FILE, -1, 1, recorder);
    expect(opsOf(b.cut)).toEqual(opsOf(a.cut));
    expect(b.rows).toBe(a.rows);
    expect(a.cls).toBe(1);
  });

  it("picks the copies by their group column", () => {
    const f = frameOf();
    f.dupg = Uint32Array.of(0, 7, 7, 3, 0);
    const m = rowMarks(treemapGeometry, f, (i) => f.dupg![i] === 7, -1, 1, recorder);
    expect(m.rows).toBe(2);
    expect(opsOf(m.cut).map((o) => o.args)).toEqual([
      [2, 20, 90, 70],
      [100, 20, 90, 70],
    ]);
  });
});

describe("the search's state", () => {
  const status = (over: Partial<DupesStatus>): DupesStatus =>
    ({
      job_id: "dupes-1",
      state: "verifying",
      scan_ids: ["scan-a", "scan-b"],
      mode: "all",
      cross_volume: true,
      progress: 0.43,
      funnel: {},
      bytes: { read: 1_200_000_000, probe: 0, full: 0, planned_probe: 0, full_upper: 0, cache_hits: 0 },
      rate: 0,
      eta_max_s: null,
      load: "quiet",
      waiting_for: "",
      ledger: [],
      skipped_by_reason: {},
      atime_unkept: 0,
      atime_moved_by_others: 0,
      groups: 3,
      extra: 4,
      reclaim: 12_300_000_000,
      version: 1,
      error: "",
      elapsed_s: 1,
      ...over,
    }) as DupesStatus;

  it("is filed under every scan it covers, and cleared with its results", () => {
    setDupes({ byScan: {} });
    onDupesProgress(status({}));
    expect(dupesOf("scan-a")?.job_id).toBe("dupes-1");
    expect(dupesOf("scan-b")?.job_id).toBe("dupes-1");
    expect(isRunning(dupesOf("scan-a"))).toBe(true);
    onDupesChanged({ job_id: "dupes-1", version: 2, reason: "cleared" });
    expect(getDupes().byScan).toEqual({});
  });

  it("says how far it got, then what it frees", () => {
    expect(dupesSummary(status({}))).toContain("43");
    expect(dupesSummary(status({ state: "done" }))).toMatch(/12[.,]3|11[.,]5/);
    expect(dupesSummary(status({ state: "waiting_scan", waiting_for: "D:" }))).toContain("D:");
  });
});

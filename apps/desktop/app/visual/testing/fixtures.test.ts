// The scenes other tests build on follow the row contract and the projection formulas.

import { describe, expect, it } from "vitest";

import type { FlameGeometry, PolarGeometry } from "@/protocol/generated";

import { Flag, Kind, Label } from "../frame";
import type { VisualFrame } from "../types";
import { PathRecorder, polarScene, rectScene, SCENE_TREE, type SceneNode } from "./fixtures";

const U = 1 << 20;

/** Row contract 1, 2, 6 and the shared-boundary rule, for any layout; `span` gives a row's [start, end). */
function contract(f: VisualFrame, span: (i: number) => [number, number], level: (i: number) => number): void {
  expect(new Set(f.key).size).toBe(f.count);
  for (let i = 0; i < f.count; i++) {
    const p = f.parent[i]!;
    expect(p).toBeLessThan(i);
    if (f.names[i]) expect(f.label[i]! !== 0 && f.kind[i]! <= Kind.PENDING).toBe(true);
    if (p >= 0) {
      const [a, b] = span(i);
      const [pa, pb] = span(p);
      expect(a).toBeGreaterThanOrEqual(pa);
      expect(b).toBeLessThanOrEqual(pb);
      expect(level(i)).toBe(level(p) + 1);
    }
  }
  // same level: increasing and never overlapping
  const byLevel = new Map<number, number[]>();
  for (let i = 0; i < f.count; i++) byLevel.set(level(i), [...(byLevel.get(level(i)) ?? []), i]);
  for (const rows of byLevel.values())
    for (let k = 1; k < rows.length; k++) expect(span(rows[k]!)[0]).toBeGreaterThanOrEqual(span(rows[k - 1]!)[1]);
}

describe("polarScene", () => {
  const { frame: f, info } = polarScene();
  const g = info.geometry as PolarGeometry;

  it("lays the rings out by the polar.v1 formulas", () => {
    // 400 × 400 at dpr 1: R = 192, hole = max(round(0.24 R), 36) = 46, 10 rings fit, detail 5
    expect(g).toEqual({ kind: "polar", cx: 200, cy: 200, radius: 46 + 5 * 29, hole: 46, ring_w: 29, rings: 5, gap: 2, unit: U });
    for (let i = 0; i < f.count; i++) {
      const lo = g.hole + (f.depth[i]! - 1) * g.ring_w;
      expect(f.r0[i]).toBe(lo + 1);
      expect(f.r1[i]).toBe(lo + g.ring_w - 1);
      expect(f.a0[i]).toBeGreaterThanOrEqual(0);
      expect(f.a1[i]).toBeLessThanOrEqual(U);
    }
  });

  it("follows the row contract", () => {
    contract(
      f,
      (i) => [f.a0[i]!, f.a1[i]!],
      (i) => f.depth[i]!,
    );
  });

  it("gives siblings shared boundaries and leaves the root's own share blank at the end", () => {
    const top = Array.from({ length: f.count }, (_, i) => i).filter((i) => f.parent[i] === -1);
    expect(f.a0[top[0]!]).toBe(0);
    for (let k = 1; k < top.length; k++) expect(f.a0[top[k]!]).toBe(f.a1[top[k - 1]!]);
    expect(f.a1[top[top.length - 1]!]).toBe(Math.round(0.9 * U));
  });

  it("draws an only child as a full ring", () => {
    const tree: SceneNode = { key: 1, node: 1, kind: Kind.FOLDER, size: 10n, name: "one", children: [{ key: 2, node: 2, kind: Kind.FILE, size: 10n, name: "only" }] };
    const s = polarScene({ tree });
    expect(s.frame.count).toBe(1);
    expect([s.frame.a0[0], s.frame.a1[0]]).toEqual([0, U]);
  });

  it("carries the zoom root, the branch slots and the selection", () => {
    expect(info.root).toMatchObject({ key: 1, node: 1, kind: Kind.FOLDER, size: 1000, branch: 0 });
    expect(info.breadcrumb).toEqual([{ key: 1, label: "root" }]);
    expect(info.selected_row).toBe(f.rowOf(2));
    // level 1 takes slots in order (free space 0); deeper rows keep their branch's slot
    const slot = (key: number) => f.branch![f.rowOf(key)];
    expect([slot(2), slot(3), slot(7)]).toEqual([1, 2, 3]);
    expect([slot(4), slot(5), slot(6)]).toEqual([2, 2, 2]);
    expect(f.branch![f.count - 1]).toBe(0);
    expect(f.kind[f.count - 1]).toBe(Kind.FREE);
  });

  it("has no rings when there is no room for one", () => {
    const s = polarScene({ width: 60, height: 60 });
    expect((s.info.geometry as PolarGeometry).rings).toBe(0);
    expect(s.frame.count).toBe(0);
  });
});

describe("rectScene", () => {
  it("is the treemap scene by default", () => {
    const { frame: f, info } = rectScene();
    expect(f.layout).toBe("drawlist.v1");
    expect([info.width, info.height, info.geometry, info.root]).toEqual([260, 100, null, null]);
    expect(f.flags[1]! & Flag.SELECTED).toBeTruthy();
  });

  it("lays a flame graph out by the flame.v1 formulas, root at the bottom", () => {
    const { frame: f, info } = rectScene({ layout: "flame.v1" });
    const g = info.geometry as FlameGeometry;
    // 400 × 200 at dpr 1: 9 bands fit at 20 px, detail 5 → row_h = 200 // 6 = 33
    expect(g).toEqual({ kind: "flame", origin: "bottom", row_h: 33, rows: 5, gap: 2, root: { x: 0, y: 200 - 31, w: 400, h: 31 } });
    for (let i = 0; i < f.count; i++) {
      expect(f.y[i]).toBe(200 - f.depth[i]! * 33 - 31);
      expect(f.h[i]).toBe(31);
    }
    contract(
      f,
      (i) => [f.x[i]!, f.x[i]! + f.w[i]! + g.gap],
      (i) => f.depth[i]!,
    );
    // the open folders are expanded bars; the folder at the last level is collapsed
    expect(f.kind[f.rowOf(3)]).toBe(Kind.FOLDER);
    expect(f.label[f.rowOf(2)]).toBe(Label.NAME);
  });

  it("mirrors the bands for the root at the top", () => {
    const bottom = rectScene({ layout: "flame.v1" }).frame;
    const top = rectScene({ layout: "flame.v1", origin: "top" });
    expect((top.info.geometry as FlameGeometry).root.y).toBe(0);
    expect(Array.from(top.frame.x)).toEqual(Array.from(bottom.x));
    for (let i = 0; i < bottom.count; i++) expect(top.frame.y[i]! + bottom.y[i]! + bottom.h[i]!).toBe(200);
  });

  it("collapses folders below the last level", () => {
    const { frame: f } = rectScene({ layout: "flame.v1", detail: 2 });
    expect(Math.max(...f.depth)).toBe(2);
    expect(f.kind[f.rowOf(5)]).toBe(Kind.COLLAPSED);
    expect(f.rowOf(6)).toBe(-1);
  });

  it("partitions the default tree completely at enough levels", () => {
    const keys = new Set<number>();
    const walk = (n: SceneNode) => n.children?.forEach((c) => (keys.add(c.key), walk(c)));
    walk(SCENE_TREE);
    const { frame: f } = rectScene({ layout: "flame.v1" });
    expect(new Set(f.key)).toEqual(keys);
  });
});

describe("PathRecorder", () => {
  const round = (rec: PathRecorder) => rec.points(2).map((p) => [Math.round(p.x) + 0, Math.round(p.y) + 0]);

  it("samples arcs the way a canvas draws them", () => {
    const cw = new PathRecorder();
    cw.arc(0, 0, 10, 0, Math.PI / 2);
    expect(round(cw)).toEqual([
      [10, 0],
      [7, 7],
      [0, 10],
    ]);
    const ccw = new PathRecorder();
    ccw.arc(0, 0, 10, 0, Math.PI / 2, true); // the long way round, through 12 o'clock
    expect(round(ccw)).toEqual([
      [10, 0],
      [-7, -7],
      [0, 10],
    ]);
    const full = new PathRecorder();
    full.arc(0, 0, 10, -Math.PI / 2, 3 * Math.PI / 2);
    expect(round(full)).toEqual([
      [0, -10],
      [0, 10],
      [0, -10],
    ]);
  });

  it("gives rectangle corners and vertices", () => {
    const rec = new PathRecorder();
    rec.rect(1, 2, 3, 4);
    rec.moveTo(5, 6);
    rec.lineTo(7, 8);
    rec.closePath();
    expect(rec.points()).toEqual([
      { x: 1, y: 2 },
      { x: 4, y: 2 },
      { x: 4, y: 6 },
      { x: 1, y: 6 },
      { x: 5, y: 6 },
      { x: 7, y: 8 },
    ]);
  });
});

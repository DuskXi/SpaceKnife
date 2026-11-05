import { describe, expect, it } from "vitest";

import { Flag, Kind } from "../frame";
import { checkGeometry } from "../testing/conformance";
import { encodeFrame, PathRecorder, rectScene, TREEMAP_ROWS } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type RectFrame } from "../types";
import { treemapComponent } from "./component";
import { treemapGeometry as g } from "./geometry";

describe("treemap geometry", () => {
  it("conforms on the fixture scene, at dpr 1 and 2", () => {
    expect(checkGeometry(treemapComponent, rectScene().frame)).toEqual([]);
    expect(checkGeometry(treemapComponent, rectScene({ dpr: 2 }).frame)).toEqual([]);
  });

  it("conforms on frames of many small blocks", () => {
    const rows = Array.from({ length: 60 }, (_, i) => ({
      key: i + 1,
      node: i + 1,
      parent: -1,
      kind: i % 3 ? Kind.FILE : Kind.COLLAPSED,
      depth: 1,
      geo: [(i % 10) * 26, Math.floor(i / 10) * 16, 25, 15] as const,
      size: 10n,
      flags: i % 7 ? 0 : Flag.SCANNING,
    }));
    expect(checkGeometry(treemapComponent, encodeFrame<RectFrame>("drawlist.v1", rows).frame)).toEqual([]);
  });

  it("hits the deepest block and nothing outside", () => {
    const f = rectScene().frame;
    expect(g.hitTest(f, 10, 30)).toBe(1); // a.mp4
    expect(g.hitTest(f, 110, 50)).toBe(3); // 中文.txt inside sub
    expect(g.hitTest(f, 5, 5)).toBe(0); // the root folder's header
    expect(g.hitTest(f, 260, 50)).toBe(HIT_NONE);
    expect(g.hitTest(f, -1, 0)).toBe(HIT_NONE);
  });

  it("fills everything but open folders", () => {
    const f = rectScene().frame;
    expect(Array.from({ length: f.count }, (_, i) => g.filled(f, i))).toEqual(TREEMAP_ROWS.map((r) => r.kind !== Kind.FOLDER));
  });

  it("measures a block by its long and short side", () => {
    const f = rectScene().frame;
    expect(g.box(f, 1)).toEqual({ major: 100, minor: 78 });
    expect(g.box(f, 5)).toEqual({ major: 100, minor: 60 }); // free space, 60 × 100
    expect(g.box(f, HIT_NONE)).toEqual({ major: 0, minor: 0 });
  });

  it("puts the label at the centre, the tag at the top right and the badge against the bottom-right corner", () => {
    const f = rectScene().frame;
    expect(g.anchor(f, 1, "label")).toEqual({ x: 52, y: 59 });
    expect(g.anchor(f, 1, "tag")).toEqual({ x: 2 + 100 - 18, y: 24 });
    expect(g.anchor(f, 1, "badge")).toEqual({ x: 102, y: 98 });
    const tiny = encodeFrame<RectFrame>("drawlist.v1", [{ key: 1, node: 1, parent: -1, kind: Kind.FILE, depth: 1, geo: [0, 0, 20, 20], size: 1n }]).frame;
    expect(g.anchor(tiny, 0, "tag")).toBeNull();
  });

  it("outlines a block moved inward, never thinner than a pixel", () => {
    const f = rectScene().frame;
    const rec = new PathRecorder();
    expect(g.path(f, 1, 2, rec)).toBe(true);
    expect(rec.ops).toEqual([{ op: "rect", args: [4, 22, 96, 74] }]);
    const thin = new PathRecorder();
    g.path(f, 1, 60, thin);
    expect(thin.ops[0]!.args.slice(2)).toEqual([1, 1]);
  });

  it("has no zoom root to hit, bound or outline", () => {
    const f = rectScene().frame;
    expect(g.bounds(f, HIT_ROOT)).toBeNull();
    expect(g.anchor(f, HIT_ROOT, "label")).toBeNull();
    expect(g.path(f, HIT_ROOT, 0, new PathRecorder())).toBe(false);
    expect(g.box(f, HIT_ROOT)).toEqual({ major: 0, minor: 0 });
  });

  it("is the baseline layout: no capability, the card beside the block", () => {
    expect(treemapComponent.layout).toBe("drawlist.v1");
    expect(treemapComponent.capability).toBeNull();
    expect(treemapComponent.hoverPlacement).toBe("beside");
  });
});

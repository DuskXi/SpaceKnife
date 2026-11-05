// drawlist.v1 geometry: nested rectangles, folders as boxes with their children inside. There is no
// root row: the zoom root fills the canvas and is not drawn on its own (HIT_ROOT never hits).

import { hitTest, Kind } from "@/treemap/frame";

import type { Box, RectFrame, VisualGeometry } from "../types";

function bounds(f: RectFrame, hit: number): Box | null {
  if (hit < 0 || hit >= f.count) return null;
  return { x: f.x[hit]!, y: f.y[hit]!, w: f.w[hit]!, h: f.h[hit]! };
}

export const treemapGeometry: VisualGeometry<RectFrame> = {
  hitTest: (f, px, py) => hitTest(f, px, py),
  bounds,
  anchor(f, hit, slot) {
    const b = bounds(f, hit);
    if (!b) return null;
    const dpr = f.info.dpr;
    // the label at the centre; a tag marker at the top right (where paint puts a file's); a badge
    // against the bottom-right corner
    if (slot === "label") return { x: b.x + b.w / 2, y: b.y + b.h / 2 };
    if (slot === "tag") return b.w >= 22 * dpr && b.h >= 18 * dpr ? { x: b.x + b.w - 18 * dpr, y: b.y + 4 * dpr } : null;
    return { x: b.x + b.w, y: b.y + b.h };
  },
  box(f, hit) {
    const b = bounds(f, hit);
    return b ? { major: Math.max(b.w, b.h), minor: Math.min(b.w, b.h) } : { major: 0, minor: 0 };
  },
  path(f, hit, inset, out) {
    const b = bounds(f, hit);
    if (!b) return false;
    out.rect(b.x + inset, b.y + inset, Math.max(1, b.w - 2 * inset), Math.max(1, b.h - 2 * inset));
    return true;
  },
  // an open folder is a box with a header; its children cover its area
  filled: (f, row) => f.kind[row] !== Kind.FOLDER,
};

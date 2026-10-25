// Where a row can be pointed at: a point of the canvas whose hit test gives that row (not a child
// drawn over it, not a gap). plugin shot's canvas steps (click:node=a/b, state/devlink.ts) press the
// mouse there, so the event goes through the view's own handlers as a user's would.

import { HIT_NONE, type Hit, type Pt, type VisualFrame, type VisualGeometry } from "./types";

/** Grid sizes tried inside the row's box after its label point and its centre. */
const GRIDS = [3, 6, 12, 24, 48] as const;

/** A point (physical px of the canvas) where `hit` is hit: its label point, its centre, else the
 * first of finer and finer grids over its box; null when the row is not drawn or covered
 * everywhere (a folder whose children fill it). */
export function pointOf<F extends VisualFrame>(geometry: VisualGeometry<F>, f: F, hit: Hit): Pt | null {
  if (hit === HIT_NONE) return null;
  const b = geometry.bounds(f, hit);
  if (!b || b.w <= 0 || b.h <= 0) return null;
  const width = f.info.width;
  const height = f.info.height;
  const hits = (p: Pt) => p.x >= 0 && p.y >= 0 && p.x < width && p.y < height && geometry.hitTest(f, p.x, p.y) === hit;
  const label = geometry.anchor(f, hit, "label");
  if (label && hits(label)) return label;
  const centre = { x: b.x + b.w / 2, y: b.y + b.h / 2 };
  if (hits(centre)) return centre;
  for (const n of GRIDS) {
    for (let j = 0; j < n; j++) {
      for (let i = 0; i < n; i++) {
        const p = { x: b.x + ((i + 0.5) * b.w) / n, y: b.y + ((j + 0.5) * b.h) / n };
        if (hits(p)) return p;
      }
    }
  }
  return null;
}

// shapes.v1 geometry (docs/protocol.md §4, layout/shapes.py): a plugin's own layout, one shape per
// row: a rounded rectangle in its box (a circle when the radius is half its sides) or a convex
// polygon. Rows are painted in row order, so the pointer hits the last row whose shape holds it; the
// zoom root has a shape of its own when ShapesGeometry.root says so. Pure: the host's hit tests and
// outlines, the paint's label and marker positions.

import type { PixelShape, ShapesGeometry } from "@/protocol/generated";

import { type AnchorSlot, type Box, HIT_NONE, HIT_ROOT, type Hit, type PathSink, type Pt, type ShapesFrame, type VisualGeometry } from "../types";

/** One row's (or the root's) shape: its box, corner radius and polygon points (absolute px). */
export interface Shape {
  x: number;
  y: number;
  w: number;
  h: number;
  r: number;
  /** x0 y0 x1 y1 … of a convex polygon; null for the rounded rectangle */
  pts: Int32Array | number[] | null;
}

/** label geometry, in lu = dpr · font scale (mirrors layout/shapes.py) */
export const TWO_LINES = 34;
export const ONE_LINE = 18;
export const TOP_INSET = 4;
export const TOP_BAND = 15;
export const POLY_INSET = 0.9;

/** The frame's shapes geometry; null on a frame of another layout. */
export function shapesOf(f: ShapesFrame): ShapesGeometry | null {
  const g = f.info.geometry;
  return g && g.kind === "shapes" ? g : null;
}

/** The zoom root's shape: drawn when the frame has a root and the plugin gave it one. */
export function rootShape(f: ShapesFrame): Shape | null {
  const s: PixelShape | null | undefined = shapesOf(f)?.root;
  if (!s || !f.info.root) return null;
  return { x: s.x, y: s.y, w: s.w, h: s.h, r: s.r, pts: s.points.length >= 6 ? s.points : null };
}

/** The shape of a row or of the root; null for HIT_NONE and rows the frame does not have. */
export function shapeOf(f: ShapesFrame, hit: Hit): Shape | null {
  if (hit === HIT_ROOT) return rootShape(f);
  if (!(hit >= 0 && hit < f.count)) return null;
  const n = f.poly[hit]!;
  const at = f.pointStart[hit]!;
  return {
    x: f.x[hit]!,
    y: f.y[hit]!,
    w: f.w[hit]!,
    h: f.h[hit]!,
    r: f.r[hit]!,
    pts: n >= 3 ? f.points.subarray(2 * at, 2 * (at + n)) : null,
  };
}

/** A circle or a pill: rounded all along its shorter side. */
export const isRound = (s: Shape): boolean => !s.pts && 2 * s.r >= Math.min(s.w, s.h) - 1;

/** Signed distance from a rounded rectangle's edge (negative inside). */
function roundDistance(s: Shape, px: number, py: number): number {
  const hw = s.w / 2;
  const hh = s.h / 2;
  const r = Math.max(0, Math.min(s.r, hw, hh));
  const qx = Math.abs(px - (s.x + hw)) - hw + r;
  const qy = Math.abs(py - (s.y + hh)) - hh + r;
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r;
}

/** Whether a point lies in a convex polygon (either orientation; its edges count as inside). */
function inPolygon(pts: ArrayLike<number>, px: number, py: number): boolean {
  const k = pts.length >> 1;
  let sign = 0;
  for (let i = 0; i < k; i++) {
    const x0 = pts[2 * i]!;
    const y0 = pts[2 * i + 1]!;
    const x1 = pts[2 * ((i + 1) % k)]!;
    const y1 = pts[2 * ((i + 1) % k) + 1]!;
    const c = (x1 - x0) * (py - y0) - (y1 - y0) * (px - x0);
    if (c === 0) continue;
    const s = c > 0 ? 1 : -1;
    if (sign === 0) sign = s;
    else if (s !== sign) return false;
  }
  return true;
}

/** Whether the point is in the shape (its box first: a cheap rejection). */
export function contains(s: Shape, px: number, py: number): boolean {
  if (px < s.x || py < s.y || px >= s.x + s.w || py >= s.y + s.h) return false;
  return s.pts ? inPolygon(s.pts, px, py) : roundDistance(s, px + 0.5, py + 0.5) <= 0.5;
}

/** The area centroid of a polygon (inside a convex one), or its box's centre. */
export function centreOf(s: Shape): Pt {
  const p = s.pts;
  if (p) {
    const k = p.length >> 1;
    let a = 0;
    let cx = 0;
    let cy = 0;
    for (let i = 0; i < k; i++) {
      const x0 = p[2 * i]!;
      const y0 = p[2 * i + 1]!;
      const x1 = p[2 * ((i + 1) % k)]!;
      const y1 = p[2 * ((i + 1) % k) + 1]!;
      const c = x0 * y1 - x1 * y0;
      a += c;
      cx += (x0 + x1) * c;
      cy += (y0 + y1) * c;
    }
    if (Math.abs(a) > 1e-9) return { x: cx / (3 * a), y: cy / (3 * a) };
  }
  return { x: s.x + s.w / 2, y: s.y + s.h / 2 };
}

/** Width of the shape between `top` and `bottom` px below its top edge: the narrower end of that
 * band (a polygon: POLY_INSET of the ellipse in its box). Mirrors layout/shapes.py band_width. */
export function bandWidth(s: Shape, top: number, bottom: number): number {
  const { w, h } = s;
  if (top < 0 || bottom > h || bottom <= top) return 0;
  if (s.pts) {
    const half = h / 2;
    const dy = Math.max(Math.abs(top - half), Math.abs(bottom - half));
    return half > 0 ? POLY_INSET * w * Math.sqrt(Math.max(0, 1 - (dy / half) ** 2)) : 0;
  }
  const r = Math.max(0, Math.min(s.r, w / 2, h / 2));
  let worst = 0;
  if (top < r) worst = Math.max(worst, r - top);
  if (bottom > h - r) worst = Math.max(worst, bottom - (h - r));
  if (worst <= 0) return w;
  return w - 2 * (r - Math.sqrt(Math.max(0, r * r - worst * worst)));
}

function hitTest(f: ShapesFrame, px: number, py: number): Hit {
  if (!(px >= 0 && py >= 0 && px < f.info.width && py < f.info.height)) return HIT_NONE;
  // the last row painted over the point wins: rows come after the ones they lie in
  for (let i = f.count - 1; i >= 0; i--) {
    const x = f.x[i]!;
    const y = f.y[i]!;
    if (px < x || py < y || px >= x + f.w[i]! || py >= y + f.h[i]!) continue;
    const s = shapeOf(f, i);
    if (s && contains(s, px, py)) return i;
  }
  const root = rootShape(f);
  return root && contains(root, px, py) ? HIT_ROOT : HIT_NONE;
}

function bounds(f: ShapesFrame, hit: Hit): Box | null {
  const s = shapeOf(f, hit);
  return s ? { x: s.x, y: s.y, w: s.w, h: s.h } : null;
}

/** Label: the shape's centre (a polygon's centroid). Tag marker (its top-left): the top right of a
 * rectangle, on the upper right of a round shape or a polygon, when it has room. Badge (its centre):
 * the bottom left of a rectangle, below the centre of the rest. */
function anchor(f: ShapesFrame, hit: Hit, slot: AnchorSlot): Pt | null {
  const s = shapeOf(f, hit);
  if (!s) return null;
  const dpr = f.info.dpr;
  const c = centreOf(s);
  if (slot === "label") return c;
  const boxy = !s.pts && !isRound(s);
  if (slot === "tag") {
    if (boxy) return s.w >= 22 * dpr && s.h >= 18 * dpr ? { x: s.x + s.w - 18 * dpr, y: s.y + 4 * dpr } : null;
    if (s.w < 48 * dpr || s.h < 48 * dpr) return null;
    return { x: c.x + 0.35 * s.w - 7 * dpr, y: c.y - 0.35 * s.h - 7 * dpr };
  }
  if (boxy) return s.w >= 70 * dpr && s.h >= 40 * dpr ? { x: s.x + 21 * dpr, y: s.y + s.h - 13.5 * dpr } : null;
  return s.w >= 80 * dpr && s.h >= 80 * dpr ? { x: c.x, y: c.y + 0.25 * s.h } : null;
}

function box(f: ShapesFrame, hit: Hit): { major: number; minor: number } {
  const s = shapeOf(f, hit);
  return s ? { major: Math.max(0, Math.max(s.w, s.h)), minor: Math.max(0, Math.min(s.w, s.h)) } : { major: 0, minor: 0 };
}

/** The outline moved `inset` px inward: a polygon's edges each moved along their normal (a convex
 * polygon stays one), a rounded rectangle's box and radius shrunk alike. */
export function outline(s: Shape, inset: number, out: PathSink): boolean {
  if (s.pts) {
    const pts = insetPolygon(s.pts, inset);
    if (!pts) return false;
    out.moveTo(pts[0]!, pts[1]!);
    for (let i = 2; i < pts.length; i += 2) out.lineTo(pts[i]!, pts[i + 1]!);
    out.closePath();
    return true;
  }
  const x = s.x + inset;
  const y = s.y + inset;
  const w = s.w - 2 * inset;
  const h = s.h - 2 * inset;
  if (!(w > 0 && h > 0)) return false;
  const r = Math.max(0, Math.min(s.r - inset, w / 2, h / 2));
  if (r <= 0.5) {
    out.rect(x, y, w, h);
    return true;
  }
  const H = Math.PI / 2;
  out.moveTo(x + r, y);
  out.lineTo(x + w - r, y);
  out.arc(x + w - r, y + r, r, -H, 0);
  out.lineTo(x + w, y + h - r);
  out.arc(x + w - r, y + h - r, r, 0, H);
  out.lineTo(x + r, y + h);
  out.arc(x + r, y + h - r, r, H, 2 * H);
  out.lineTo(x, y + r);
  out.arc(x + r, y + r, r, 2 * H, 3 * H);
  out.closePath();
  return true;
}

/** A convex polygon with every edge moved `d` px inward (outward for d < 0); null when it vanishes. */
export function insetPolygon(pts: ArrayLike<number>, d: number): number[] | null {
  const k = pts.length >> 1;
  if (k < 3) return null;
  if (d === 0) return Array.from(pts);
  let area = 0;
  for (let i = 0; i < k; i++) area += pts[2 * i]! * pts[2 * ((i + 1) % k) + 1]! - pts[2 * ((i + 1) % k)]! * pts[2 * i + 1]!;
  const sgn = area > 0 ? 1 : -1; // inward normal side
  // each edge as a line n·p = c moved inward by d
  const lines: [number, number, number][] = [];
  for (let i = 0; i < k; i++) {
    const x0 = pts[2 * i]!;
    const y0 = pts[2 * i + 1]!;
    const x1 = pts[2 * ((i + 1) % k)]!;
    const y1 = pts[2 * ((i + 1) % k) + 1]!;
    const len = Math.hypot(x1 - x0, y1 - y0);
    if (len < 1e-9) continue;
    const nx = (-(y1 - y0) / len) * sgn;
    const ny = ((x1 - x0) / len) * sgn;
    lines.push([nx, ny, nx * x0 + ny * y0 + d]);
  }
  const m = lines.length;
  if (m < 3) return null;
  // moved past its middle: the edges would cross (checked from the centroid, enough for outlines)
  if (d > 0) {
    const c = centreOf({ x: 0, y: 0, w: 0, h: 0, r: 0, pts: Array.from(pts) });
    if (lines.some(([nx, ny, cc]) => nx * c.x + ny * c.y <= cc)) return null;
  }
  const out: number[] = [];
  for (let i = 0; i < m; i++) {
    const [a1, b1, c1] = lines[(i + m - 1) % m]!;
    const [a2, b2, c2] = lines[i]!;
    const det = a1 * b2 - a2 * b1;
    if (Math.abs(det) < 1e-9) continue; // collinear edges: the corner is not one
    out.push((c1 * b2 - c2 * b1) / det, (a1 * c2 - a2 * c1) / det);
  }
  return out.length >= 6 ? out : null;
}

function path(f: ShapesFrame, hit: Hit, inset: number, out: PathSink): boolean {
  const s = shapeOf(f, hit);
  return s ? outline(s, inset, out) : false;
}

export const shapesGeometry: VisualGeometry<ShapesFrame> = {
  hitTest,
  bounds,
  anchor,
  box,
  path,
  // every row has a fill of its own, open folders included
  filled: () => true,
};

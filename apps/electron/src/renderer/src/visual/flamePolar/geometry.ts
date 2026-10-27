// polar.v1 geometry (docs/protocol.md §4.4, docs/visuals.md): sectors in PolarGeometry.unit per turn
// from 12 o'clock clockwise, radii from the centre. Hits go by cell: a ring's cell holds the radial
// gaps around it and a sector's span holds the seams beside it, so the pointer never falls through
// a gap. Pure functions, the reference for the sector shader (sectors.ts) and the other front ends.

import type { PolarGeometry } from "@/protocol/generated";

import { HIT_NONE, HIT_ROOT, type AnchorSlot, type Box, type Hit, type PathSink, type PolarFrame, type Pt, type VisualGeometry } from "../types";

export const TURN = 2 * Math.PI;

/** The frame's PolarGeometry; null when it carries none. */
export function polarOf(f: Pick<PolarFrame, "info">): PolarGeometry | null {
  const g = f.info.geometry;
  return g && g.kind === "polar" ? g : null;
}

/** Wire angle (units from 12 o'clock, clockwise) of a canvas point, in [0, unit). */
export function angleAt(g: PolarGeometry, px: number, py: number): number {
  let th = Math.atan2(px - g.cx, g.cy - py);
  if (th < 0) th += TURN;
  const u = (th / TURN) * g.unit;
  return u >= g.unit ? 0 : u;
}

/** Canvas point at a radius and a wire angle. */
export function pointAt(g: PolarGeometry, r: number, a: number): Pt {
  const th = (a / g.unit) * TURN;
  return { x: g.cx + r * Math.sin(th), y: g.cy - r * Math.cos(th) };
}

/** Ring (1-based) whose cell holds a radius; 0 inside the hole or without rings. */
export function ringAt(g: PolarGeometry, r: number): number {
  return g.ring_w > 0 && r >= g.hole ? Math.floor((r - g.hole) / g.ring_w) + 1 : 0;
}

/** Radius of the centre disc: its cell is [0, hole), drawn like a ring's outer edge. */
export const discRadius = (g: PolarGeometry): number => g.hole - Math.floor(g.gap / 2);

/** Sweep of a row in radians. */
export const sweepOf = (f: PolarFrame, g: PolarGeometry, row: number): number => ((f.a1[row]! - f.a0[row]!) / g.unit) * TURN;

/** A sector spanning the whole turn has no radial edges. */
export const isFullTurn = (f: PolarFrame, g: PolarGeometry, row: number): boolean => f.a1[row]! - f.a0[row]! >= g.unit;

/** How far each radial edge moves in, perpendicular to it, to draw the seam between neighbours:
 * half the gap, but never so far that the sector is narrower than 1 px at its inner radius.
 * `delta` is the sweep in radians; a full turn has no seams. */
export function seamInset(delta: number, r0: number, gap: number): number {
  if (delta >= TURN) return 0;
  return Math.min(gap / 2, Math.max(0, (delta * r0 - 1) / 2));
}

/** A sector in turns (t0 < t1 ≤ t0 + 1, 0 = 12 o'clock, clockwise) and px around a centre. */
export interface Sector {
  cx: number;
  cy: number;
  t0: number;
  t1: number;
  r0: number;
  r1: number;
}

/** Signed distance of a point from a sector as drawn (negative inside): the radial distance,
 * and the perpendicular distance from the nearer radial edge moved in by seamInset. The sector
 * shader evaluates the same function per pixel. */
export function sectorDistance(px: number, py: number, s: Sector, gap: number): number {
  const x = px - s.cx;
  const y = py - s.cy;
  const r = Math.hypot(x, y);
  const radial = Math.max(s.r0 - r, r - s.r1);
  const span = (s.t1 - s.t0) * TURN;
  if (span >= TURN) return radial;
  const th0 = s.t0 * TURN;
  const th1 = s.t1 * TURN;
  // signed distances from the two edge lines, positive on the sector's side
  const d0 = x * Math.cos(th0) + y * Math.sin(th0);
  const d1 = -(x * Math.cos(th1) + y * Math.sin(th1));
  // up to half a turn the sector is the intersection of the two half-planes, beyond it their union
  const edge = span <= Math.PI ? -Math.min(d0, d1) : -Math.max(d0, d1);
  return Math.max(radial, edge + seamInset(span, s.r0, gap));
}

/** Width a tangential label may take (docs/protocol.md §4.4): the arc at mid radius less a gap,
 * and the chord at mid radius that stays round(2·dpr) inside the outer edge. */
export function tangentWidth(delta: number, r0: number, r1: number, gap: number, dpr: number): number {
  const mid = (r0 + r1) / 2;
  const edge = r1 - Math.round(2 * dpr);
  return Math.min(delta * mid - gap, 2 * Math.sqrt(Math.max(0, edge * edge - mid * mid)));
}

// ------------------------------------------------------------------ hit index

interface Rings {
  /** rows of ring k are rows[start[k] .. start[k + 1]), by a0 */
  start: Int32Array;
  rows: Int32Array;
  a0: Int32Array;
}

const INDEX = new WeakMap<PolarFrame, Rings>();

/** Rows grouped by ring, built once per frame (row contract 8 already orders a ring by a0). */
function ringsOf(f: PolarFrame, g: PolarGeometry): Rings {
  const known = INDEX.get(f);
  if (known) return known;
  const n = f.count;
  const start = new Int32Array(g.rings + 2);
  const ring = new Int32Array(n);
  for (let i = 0; i < n; i++) {
    const k = ringAt(g, f.r0[i]!);
    ring[i] = k >= 1 && k <= g.rings ? k : 0;
    start[ring[i]! + 1]++;
  }
  for (let k = 1; k < start.length; k++) start[k] += start[k - 1]!;
  const rows = new Int32Array(start[start.length - 1]!);
  const fill = start.slice();
  for (let i = 0; i < n; i++) rows[fill[ring[i]!]++] = i;
  // ring 0 (rows outside every cell) is never searched
  const a0 = Int32Array.from(rows, (i) => f.a0[i]!);
  for (let k = 1; k <= g.rings; k++) {
    const [lo, hi] = [start[k]!, start[k + 1]!];
    let sorted = true;
    for (let j = lo + 1; j < hi && sorted; j++) sorted = a0[j - 1]! <= a0[j]!;
    if (sorted) continue;
    const part = Array.from(rows.subarray(lo, hi)).sort((p, q) => f.a0[p]! - f.a0[q]!);
    rows.set(part, lo);
    a0.set(part.map((i) => f.a0[i]!), lo);
  }
  const out = { start, rows, a0 };
  INDEX.set(f, out);
  return out;
}

// ------------------------------------------------------------------ outlines

/** Outline of a ring part from `ri` to `ro` over [th0, th0 + span] (radians from 12 o'clock), its
 * radial edges moved `edge` px in, perpendicular to them (out when negative); `full`: no edges. */
function sectorPath(out: PathSink, cx: number, cy: number, th0: number, span: number, ri: number, ro: number, edge: number, full: boolean): void {
  const phi = th0 - Math.PI / 2; // canvas angles start at 3 o'clock
  // how far along an arc the moved edge meets it; never past the middle, never beyond a turn
  const along = (r: number) => Math.max((span - TURN) / 2, Math.min(span / 2, Math.asin(Math.max(-1, Math.min(1, edge / r)))));
  const ao = along(ro);
  if (full || span - 2 * ao >= TURN) {
    // a whole turn: an annulus (or a disc)
    out.moveTo(cx + ro * Math.cos(phi), cy + ro * Math.sin(phi));
    out.arc(cx, cy, ro, phi, phi + TURN);
    out.closePath();
    if (ri > 0) {
      out.moveTo(cx + ri * Math.cos(phi), cy + ri * Math.sin(phi));
      out.arc(cx, cy, ri, phi + TURN, phi, true);
      out.closePath();
    }
    return;
  }
  const o0 = phi + ao;
  out.moveTo(cx + ro * Math.cos(o0), cy + ro * Math.sin(o0));
  out.arc(cx, cy, ro, o0, phi + span - ao);
  if (ri > 0) {
    const ai = along(ri);
    out.arc(cx, cy, ri, phi + span - ai, phi + ai, true);
  } else out.lineTo(cx, cy);
  out.closePath();
}

// ------------------------------------------------------------------ the geometry

function rowBounds(f: PolarFrame, g: PolarGeometry, row: number): Box {
  const [a0, a1, r0, r1] = [f.a0[row]!, f.a1[row]!, f.r0[row]!, f.r1[row]!];
  const pts = [pointAt(g, r0, a0), pointAt(g, r1, a0), pointAt(g, r0, a1), pointAt(g, r1, a1)];
  // the outer arc's extremes: wherever it crosses 12, 3, 6 or 9 o'clock
  for (let q = 0; q < 4; q++) {
    const a = (q * g.unit) / 4;
    if (a >= a0 && a < a1) pts.push(pointAt(g, r1, a));
  }
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const p of pts) {
    x0 = Math.min(x0, p.x);
    y0 = Math.min(y0, p.y);
    x1 = Math.max(x1, p.x);
    y1 = Math.max(y1, p.y);
  }
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

const hasRow = (f: PolarFrame, hit: Hit) => hit >= 0 && hit < f.count;

/** Anchors are the centres of what goes there, on the bisector: the label at mid radius, the tag
 * marker 10 px inside the outer edge, the badge 12 px outside the inner edge; on the disc, its
 * centre, top and bottom. */
function anchorOf(f: PolarFrame, hit: Hit, slot: AnchorSlot): Pt | null {
  const g = polarOf(f);
  if (!g) return null;
  const dpr = f.info.dpr;
  if (hit === HIT_ROOT) {
    const rd = discRadius(g);
    if (!f.info.root || rd <= 0) return null;
    if (slot === "label") return { x: g.cx, y: g.cy };
    // a tag marker at the top of the disc, a badge at its bottom, below the centre label
    if (slot === "tag") return rd >= 22 * dpr ? { x: g.cx, y: g.cy - rd + 10 * dpr } : null;
    return 2 * rd >= 70 * dpr ? { x: g.cx, y: g.cy + rd - 12 * dpr } : null;
  }
  if (!hasRow(f, hit)) return null;
  const [r0, r1] = [f.r0[hit]!, f.r1[hit]!];
  const mid = (f.a0[hit]! + f.a1[hit]!) / 2;
  if (slot === "label") return pointAt(g, (r0 + r1) / 2, mid);
  const delta = sweepOf(f, g, hit);
  const T = r1 - r0;
  if (slot === "tag") {
    // the marker's centre, 10 px inside the outer edge, where the arc still holds it
    const r = r1 - 10 * dpr;
    return T >= 22 * dpr && delta * r >= 18 * dpr ? pointAt(g, r, mid) : null;
  }
  // 「新增」 / 「已删除」 near the inner edge, on a label box of at least 70 × 40 (UI-W6-16)
  const across = tangentWidth(delta, r0, r1, g.gap, dpr);
  const inner = delta * r0 - g.gap;
  const fits = (across >= 70 * dpr && T >= 40 * dpr) || (T >= 70 * dpr && inner >= 40 * dpr);
  return fits ? pointAt(g, r0 + 12 * dpr, mid) : null;
}

export const polarGeometry: VisualGeometry<PolarFrame> = {
  hitTest(f, px, py) {
    const g = polarOf(f);
    if (!g || px < 0 || py < 0 || px >= f.info.width || py >= f.info.height) return HIT_NONE;
    const r = Math.hypot(px - g.cx, py - g.cy);
    if (r < g.hole) return f.info.root ? HIT_ROOT : HIT_NONE;
    const k = ringAt(g, r);
    if (k < 1 || k > g.rings) return HIT_NONE;
    const idx = ringsOf(f, g);
    const u = angleAt(g, px, py);
    // the last sector of the ring starting at or before u
    let lo = idx.start[k]!;
    let hi = idx.start[k + 1]! - 1;
    let found = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (idx.a0[mid]! <= u) {
        found = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    if (found < 0) return HIT_NONE;
    const row = idx.rows[found]!;
    // past its end: the folder's own share, left blank
    return u < f.a1[row]! ? row : HIT_NONE;
  },

  bounds(f, hit) {
    const g = polarOf(f);
    if (!g) return null;
    if (hit === HIT_ROOT) {
      const rd = discRadius(g);
      return f.info.root && rd > 0 ? { x: g.cx - rd, y: g.cy - rd, w: 2 * rd, h: 2 * rd } : null;
    }
    return hasRow(f, hit) ? rowBounds(f, g, hit) : null;
  },

  anchor: anchorOf,

  box(f, hit) {
    const g = polarOf(f);
    if (!g) return { major: 0, minor: 0 };
    if (hit === HIT_ROOT) {
      const d = f.info.root ? 2 * Math.max(0, discRadius(g)) : 0;
      return { major: d, minor: d };
    }
    if (!hasRow(f, hit)) return { major: 0, minor: 0 };
    const T = f.r1[hit]! - f.r0[hit]!;
    const arc = sweepOf(f, g, hit) * ((f.r0[hit]! + f.r1[hit]!) / 2);
    return { major: Math.max(arc, T), minor: Math.min(arc, T) };
  },

  path(f, hit, inset, out) {
    const g = polarOf(f);
    if (!g) return false;
    if (hit === HIT_ROOT) {
      const r = discRadius(g) - inset;
      if (!f.info.root || r <= 0) return false;
      out.moveTo(g.cx + r, g.cy);
      out.arc(g.cx, g.cy, r, 0, TURN);
      out.closePath();
      return true;
    }
    if (!hasRow(f, hit)) return false;
    const ro = f.r1[hit]! - inset;
    const ri = Math.max(0, f.r0[hit]! + inset);
    if (ro <= ri) return false;
    const span = sweepOf(f, g, hit);
    const full = isFullTurn(f, g, hit);
    // the seam rule with the inset added: thin spikes keep 1 px at their inner radius
    const edge = full ? 0 : Math.min(g.gap / 2 + inset, Math.max(0, (span * ri - 1) / 2));
    sectorPath(out, g.cx, g.cy, (f.a0[hit]! / g.unit) * TURN, span, ri, ro, edge, full);
    return true;
  },

  filled: () => true,
};

/** Polar zoom transform of the sector shader, [sθ, tθ, sr, tr]: angles t' = t·sθ + tθ (turns),
 * radii r' = r·sr + tr (px). */
export type PolarXform = readonly [number, number, number, number];

export const IDENTITY: PolarXform = [1, 0, 1, 0];

/** The zoom animation: at t = 1 `row` spans the whole turn and sits in the centre disc's place,
 * its children in the first ring. Linear in t from the identity. */
export function zoomTransform(f: PolarFrame, row: number, t: number): PolarXform {
  const g = polarOf(f);
  if (!g || !hasRow(f, row) || t <= 0) return IDENTITY;
  const t0 = f.a0[row]! / g.unit;
  const span = (f.a1[row]! - f.a0[row]!) / g.unit;
  if (span <= 0) return IDENTITY;
  const k = ringAt(g, f.r0[row]!);
  return [1 + (1 / span - 1) * t, (-t0 / span) * t, 1, -k * g.ring_w * t];
}

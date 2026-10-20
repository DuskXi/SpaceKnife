// flame.v1 geometry (docs/protocol.md §4.3, docs/visuals.md): one band per level, the zoom root's
// bar in band 0, bars in x order within a band. Pure: the host's hit tests and outlines, the paint's
// marker positions.

import type { FlameGeometry } from "@/protocol/generated";

import { type AnchorSlot, type Box, HIT_NONE, HIT_ROOT, type Hit, type PathSink, type Pt, type RectFrame, type VisualGeometry } from "../types";

/** The frame's flame geometry; null on a frame of another layout. */
export function flameOf(f: RectFrame): FlameGeometry | null {
  const g = f.info.geometry;
  return g && g.kind === "flame" ? g : null;
}

/** The zoom root's bar; null when the frame has no root to draw. */
export function rootBar(f: RectFrame): Box | null {
  const g = flameOf(f);
  return g && f.info.root ? { ...g.root } : null;
}

/** Band whose cell holds a y: 0 is the root bar's, k the k-th level, -1 beyond the drawn bands. A cell
 * is a band's bars plus the gap after them, counted away from the root's edge of the canvas, so the
 * gaps between bands hit too. */
export function bandAt(g: FlameGeometry, py: number): number {
  if (!(g.row_h > 0)) return -1;
  const k = g.origin === "top" ? Math.floor((py - g.root.y) / g.row_h) : Math.ceil((g.root.y + g.root.h - py) / g.row_h) - 1;
  return k >= 0 && k <= g.rows ? k : -1;
}

/** Rows by band (CSR: band k's rows are rows[start[k]] … rows[start[k + 1] - 1]) in x order. */
interface Bands {
  start: Int32Array;
  rows: Int32Array;
}

// one index per decoded frame, built on the first hit test; frames are never mutated
const index = new WeakMap<RectFrame, Bands>();

function bandsOf(f: RectFrame, g: FlameGeometry): Bands {
  let b = index.get(f);
  if (b) return b;
  const n = g.rows + 1;
  const band = new Int32Array(f.count);
  const start = new Int32Array(n + 1);
  for (let i = 0; i < f.count; i++) {
    const k = bandAt(g, f.y[i]! + f.h[i]! / 2);
    band[i] = k;
    if (k >= 1) start[k + 1]!++;
  }
  for (let k = 1; k <= n; k++) start[k]! += start[k - 1]!;
  const rows = new Int32Array(start[n]!);
  const fill = start.slice(0, n);
  for (let i = 0; i < f.count; i++) if (band[i]! >= 1) rows[fill[band[i]!]!++] = i;
  // a pre-ordered frame lists each band left to right (row contract 8); sort only what is not
  for (let k = 1; k < n; k++) {
    const sub = rows.subarray(start[k]!, start[k + 1]!);
    for (let j = 1; j < sub.length; j++)
      if (f.x[sub[j]!]! < f.x[sub[j - 1]!]!) {
        sub.sort((p, q) => f.x[p]! - f.x[q]!);
        break;
      }
  }
  b = { start, rows };
  index.set(f, b);
  return b;
}

function hitTest(f: RectFrame, px: number, py: number): Hit {
  const g = flameOf(f);
  if (!g || !(px >= 0 && py >= 0 && px < f.info.width && py < f.info.height)) return HIT_NONE;
  const k = bandAt(g, py);
  if (k < 0) return HIT_NONE;
  if (k === 0) return f.info.root && px >= g.root.x && px < g.root.x + g.root.w ? HIT_ROOT : HIT_NONE;
  const { start, rows } = bandsOf(f, g);
  // the last bar starting at or left of px, if px is within it or the gap on its right
  let lo = start[k]!;
  let hi = start[k + 1]!;
  const first = lo;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (f.x[rows[mid]!]! <= px) lo = mid + 1;
    else hi = mid;
  }
  if (lo === first) return HIT_NONE;
  const r = rows[lo - 1]!;
  return px < f.x[r]! + f.w[r]! + g.gap ? r : HIT_NONE;
}

function bounds(f: RectFrame, hit: Hit): Box | null {
  if (hit === HIT_ROOT) return rootBar(f);
  if (!(hit >= 0 && hit < f.count)) return null;
  return { x: f.x[hit]!, y: f.y[hit]!, w: f.w[hit]!, h: f.h[hit]! };
}

/** Label: the bar's centre. Tag marker (its top-left): at the right end, on bars of at least 22 × 18
 * CSS px. Badge (compare 「新增」 / 「已删除」, the static change badge; its centre): bottom left, on
 * bars of at least 70 × 40. */
function anchor(f: RectFrame, hit: Hit, slot: AnchorSlot): Pt | null {
  const b = bounds(f, hit);
  if (!b) return null;
  const dpr = f.info.dpr;
  if (slot === "label") return { x: b.x + b.w / 2, y: b.y + b.h / 2 };
  if (slot === "tag") return b.w >= 22 * dpr && b.h >= 18 * dpr ? { x: b.x + b.w - 18 * dpr, y: b.y + 4 * dpr } : null;
  return b.w >= 70 * dpr && b.h >= 40 * dpr ? { x: b.x + 21 * dpr, y: b.y + b.h - 13.5 * dpr } : null;
}

/** Along the bar and across it. */
function box(f: RectFrame, hit: Hit): { major: number; minor: number } {
  const b = bounds(f, hit);
  return b ? { major: Math.max(0, b.w), minor: Math.max(0, b.h) } : { major: 0, minor: 0 };
}

function path(f: RectFrame, hit: Hit, inset: number, out: PathSink): boolean {
  const b = bounds(f, hit);
  if (!b) return false;
  const w = b.w - 2 * inset;
  const h = b.h - 2 * inset;
  if (!(w > 0 && h > 0)) return false;
  out.rect(b.x + inset, b.y + inset, w, h);
  return true;
}

export const flameGeometry: VisualGeometry<RectFrame> = {
  hitTest,
  bounds,
  anchor,
  box,
  path,
  // every bar has a fill of its own, open folders included (the compare scale counts them all)
  filled: () => true,
};

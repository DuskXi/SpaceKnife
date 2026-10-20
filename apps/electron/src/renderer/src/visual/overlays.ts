// What the view host draws over any visualization: file-change flashes and, under reduced motion,
// their static badges (UI-TM-43/44); the pointer trail (「鼠标拖尾」); and the breathing outline of
// folders still being scanned (UI-TM-42 / UI-W2-04), on a canvas of its own so the pulse never
// repaints the shapes or their labels. The rhythm is the host's; the outline of a row comes from the
// component's geometry, so a block, a bar and a sector flash and breathe alike.

import { tokens } from "@design/tokens";

import type { FlashItem } from "@/protocol/generated";
import { fmtSize } from "@/lib/format";

import { Flag } from "./frame";
import { type Box, HIT_ROOT, type Hit, type Pt, type VisualFrame, type VisualGeometry } from "./types";

/** Makes the outlines the overlays stroke: Path2D in the app, a recorder in tests. */
export type NewPath = () => Path2D;
const path2d: NewPath = () => new Path2D();

// ------------------------------------------------------------------ breathing (UI-TM-42)

export interface Breath {
  hit: Hit;
  depth: number;
  lineWidth: number;
  path: Path2D;
}

const PERIOD_MS = 2400; // one breath
const RIPPLE_MS = 220; // each level breathes this much later than the one around it: a wave inward
const LEVEL_ALPHA = [1, 0.7, 0.45]; // outer levels brighter
const MAX_BLOCKS = 300;
const ACCENT = tokens.color.accent.default;

/** The marked rows (and the zoom root, when its own flags say so) roomy enough to carry an outline:
 * the largest first when there are more than MAX_BLOCKS. */
export function breathingRows<F extends VisualFrame>(g: VisualGeometry<F>, f: F, dpr: number, newPath: NewPath = path2d): Breath[] {
  const min = 12 * dpr;
  const picked: { hit: Hit; depth: number; area: number }[] = [];
  const consider = (hit: Hit, depth: number) => {
    const b = g.box(f, hit);
    if (b.minor >= min) picked.push({ hit, depth: Math.max(1, depth), area: b.major * b.minor });
  };
  const root = f.info.root;
  if (root && root.flags & Flag.SCANNING) consider(HIT_ROOT, 1);
  for (let i = 0; i < f.count; i++) if (f.flags[i]! & Flag.SCANNING) consider(i, f.depth[i]!);
  if (picked.length > MAX_BLOCKS) picked.sort((a, b) => b.area - a.area).length = MAX_BLOCKS;
  const out: Breath[] = [];
  for (const p of picked) {
    const lineWidth = (p.depth === 1 ? 2 : 1.5) * dpr;
    const path = newPath();
    if (g.path(f, p.hit, lineWidth / 2, path)) out.push({ hit: p.hit, depth: p.depth, lineWidth, path });
  }
  return out;
}

/** 0–1 strength of a row's outline at `now`; steady under reduced motion. */
export function breathAlpha(depth: number, now: number, reduced: boolean): number {
  const level = LEVEL_ALPHA[Math.min(depth, LEVEL_ALPHA.length) - 1]!;
  if (reduced) return 0.6 * level;
  const phase = 0.5 - 0.5 * Math.cos((2 * Math.PI * (now - depth * RIPPLE_MS)) / PERIOD_MS);
  return (0.2 + 0.7 * phase) * level;
}

export function drawBreath(ctx: CanvasRenderingContext2D, rows: Breath[], now: number, reduced: boolean, dpr: number): void {
  ctx.save();
  ctx.strokeStyle = ACCENT;
  ctx.shadowColor = ACCENT;
  for (const b of rows) {
    const a = breathAlpha(b.depth, now, reduced);
    ctx.globalAlpha = a;
    ctx.lineWidth = b.lineWidth;
    ctx.shadowBlur = (reduced ? 6 : 4 + 10 * a) * dpr;
    ctx.stroke(b.path);
  }
  ctx.restore();
}

// ------------------------------------------------------------------ file changes (UI-TM-43/44)

export interface FlashMark {
  path: Path2D;
  /** where the static badge goes (reduced motion) and the room it has */
  badge: Pt | null;
  bounds: Box | null;
  delta: number;
}

export interface Flash {
  start: number;
  marks: FlashMark[];
}

const FLASH_MS = tokens.motion.duration.flash;
const FLASH_FADE_MS = 300;
const BADGE_MS = 10_000;
const FLASH = tokens.treemap.state.flash;

/** The rows a frame says changed (row -1: the zoom root); null when none of them is drawn. */
export function flashOf<F extends VisualFrame>(g: VisualGeometry<F>, f: F, items: readonly FlashItem[], now: number, newPath: NewPath = path2d): Flash | null {
  const dpr = f.info.dpr;
  const marks: FlashMark[] = [];
  for (const it of items) {
    const hit = it.row < 0 ? HIT_ROOT : it.row;
    const path = newPath();
    if (!g.path(f, hit, dpr, path)) continue;
    marks.push({ path, badge: g.anchor(f, hit, "badge"), bounds: g.bounds(f, hit), delta: it.delta });
  }
  return marks.length ? { start: now, marks } : null;
}

/** Top-left of a w × h badge centred on the anchor and pulled back inside the row's bounds, `margin`
 * all round; null when it does not fit (the treemap's anchor is the block's bottom-right corner, so
 * its badge ends up in that corner). */
export function badgeAt(anchor: Pt, bounds: Box, w: number, h: number, margin: number): Pt | null {
  if (w + 2 * margin > bounds.w || h + 2 * margin > bounds.h) return null;
  const x = Math.max(bounds.x + margin, Math.min(anchor.x - w / 2, bounds.x + bounds.w - margin - w));
  const y = Math.max(bounds.y + margin, Math.min(anchor.y - h / 2, bounds.y + bounds.h - margin - h));
  return { x, y };
}

/** Draw the changed rows: two pulses, then a fade; under reduced motion the change of size as a
 * static badge ("+212 MB") where it fits. False once there is nothing left to show. */
export function drawFlash(ctx: CanvasRenderingContext2D, flash: Flash, now: number, reduced: boolean, dpr: number): boolean {
  const t = now - flash.start;
  if (reduced) {
    if (t > BADGE_MS) return false;
    ctx.save();
    ctx.font = `${11 * dpr}px ${tokens.font.family.mono}`;
    ctx.textBaseline = "middle";
    for (const m of flash.marks) {
      if (!m.badge || !m.bounds) continue;
      const label = `${m.delta >= 0 ? "+" : "−"}${fmtSize(Math.abs(m.delta))}`;
      const tw = ctx.measureText(label).width + 8 * dpr;
      const th = 16 * dpr;
      const at = badgeAt(m.badge, m.bounds, tw, th, 2 * dpr);
      if (!at) continue;
      ctx.fillStyle = tokens.treemap.state.newBadgeBg;
      ctx.fillRect(at.x, at.y, tw, th);
      ctx.fillStyle = tokens.treemap.state.newBadgeText;
      ctx.fillText(label, at.x + 4 * dpr, at.y + th / 2);
    }
    ctx.restore();
    return true;
  }
  if (t > FLASH_MS + FLASH_FADE_MS) return false;
  // two pulses over FLASH_MS, then the outline fades out
  const pulse = t < FLASH_MS ? 0.5 - 0.5 * Math.cos((4 * Math.PI * t) / FLASH_MS) : 1 - (t - FLASH_MS) / FLASH_FADE_MS;
  ctx.save();
  ctx.globalAlpha = Math.max(0, Math.min(1, t < FLASH_MS ? 0.35 + 0.65 * pulse : pulse));
  ctx.strokeStyle = FLASH;
  ctx.lineWidth = 2 * dpr;
  ctx.shadowColor = `${FLASH}8C`; // 55 %
  ctx.shadowBlur = 18 * dpr;
  for (const m of flash.marks) ctx.stroke(m.path);
  ctx.restore();
  return true;
}

// ------------------------------------------------------------------ 「鼠标拖尾」

export interface TrailMark {
  path: Path2D;
  t: number;
}

const TRAIL_MS = 500; // rows the pointer left fade out over this long

/** The outline of the row the pointer just left; null when the row has none. */
export function trailMark<F extends VisualFrame>(g: VisualGeometry<F>, f: F, hit: Hit, now: number, newPath: NewPath = path2d): TrailMark | null {
  const path = newPath();
  return g.path(f, hit, f.info.dpr / 2, path) ? { path, t: now } : null;
}

/** Draw the trail; returns the marks still fading. */
export function drawTrail(ctx: CanvasRenderingContext2D, trail: TrailMark[], now: number, dpr: number): TrailMark[] {
  const left = trail.filter((m) => now - m.t < TRAIL_MS);
  ctx.save();
  ctx.lineWidth = dpr;
  ctx.strokeStyle = tokens.treemap.state.hoverInset;
  for (const m of left) {
    ctx.globalAlpha = 0.6 * (1 - (now - m.t) / TRAIL_MS);
    ctx.stroke(m.path);
  }
  ctx.restore();
  return left;
}

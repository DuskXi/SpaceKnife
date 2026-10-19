// Folders still being scanned (UI-TM-42 / UI-W2-04): the backend marks the top levels whose subtree is
// not fully listed yet (Flag.SCANNING); they breathe with an accent outline. Drawn on a canvas of its
// own, so the pulse never repaints the blocks or their labels.

import { tokens } from "@design/tokens";

import { type DrawFrame, Flag } from "./frame";

export interface Breath {
  x: number;
  y: number;
  w: number;
  h: number;
  depth: number;
}

const PERIOD_MS = 2400; // one breath
const RIPPLE_MS = 220; // each level breathes this much later than the one around it: a wave inward
const LEVEL_ALPHA = [1, 0.7, 0.45]; // outer levels brighter
const MAX_BLOCKS = 300;
const ACCENT = tokens.color.accent.default;

/** The marked blocks big enough to carry an outline (the largest first, at most MAX_BLOCKS). */
export function breathingBlocks(f: Pick<DrawFrame, "count" | "flags" | "depth" | "x" | "y" | "w" | "h">, dpr: number): Breath[] {
  const min = 12 * dpr;
  const out: Breath[] = [];
  for (let i = 0; i < f.count; i++) {
    if (!(f.flags[i]! & Flag.SCANNING) || f.w[i]! < min || f.h[i]! < min) continue;
    out.push({ x: f.x[i]!, y: f.y[i]!, w: f.w[i]!, h: f.h[i]!, depth: Math.max(1, f.depth[i]!) });
  }
  if (out.length > MAX_BLOCKS) out.sort((a, b) => b.w * b.h - a.w * a.h).length = MAX_BLOCKS;
  return out;
}

/** 0–1 strength of a block's outline at `now`; steady under reduced motion. */
export function breathAlpha(depth: number, now: number, reduced: boolean): number {
  const level = LEVEL_ALPHA[Math.min(depth, LEVEL_ALPHA.length) - 1]!;
  if (reduced) return 0.6 * level;
  const phase = 0.5 - 0.5 * Math.cos((2 * Math.PI * (now - depth * RIPPLE_MS)) / PERIOD_MS);
  return (0.2 + 0.7 * phase) * level;
}

export function drawBreath(ctx: CanvasRenderingContext2D, blocks: Breath[], now: number, reduced: boolean, dpr: number): void {
  ctx.save();
  ctx.strokeStyle = ACCENT;
  ctx.shadowColor = ACCENT;
  for (const b of blocks) {
    const a = breathAlpha(b.depth, now, reduced);
    const lw = (b.depth === 1 ? 2 : 1.5) * dpr;
    ctx.globalAlpha = a;
    ctx.lineWidth = lw;
    ctx.shadowBlur = (reduced ? 6 : 4 + 10 * a) * dpr;
    ctx.strokeRect(b.x + lw / 2, b.y + lw / 2, Math.max(1, b.w - lw), Math.max(1, b.h - lw));
  }
  ctx.restore();
}

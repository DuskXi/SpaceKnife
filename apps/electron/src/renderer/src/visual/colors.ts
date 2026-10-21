// Colours the visualizations share beyond the treemap's (design/tokens/visual.tokens.json): the
// 「顶层分支」 palette, folder bars / sectors outside branch colouring, the zoom root, the halo overlay.
// Parsed once at load; no runtime import of treemap/paint, which imports this module.

import { tokens } from "@design/tokens";

import type { RGBA } from "@/treemap/paint";

const V = tokens.visual;

function parse(hex: string): RGBA {
  const h = hex.replace("#", "");
  const a = h.length >= 8 ? parseInt(h.slice(6, 8), 16) / 255 : 1;
  return [parseInt(h.slice(0, 2), 16) / 255, parseInt(h.slice(2, 4), 16) / 255, parseInt(h.slice(4, 6), 16) / 255, a];
}

/** Tier of a depth: rows below the zoom root start at depth 1; deeper than 4 shares tier 4. */
const tier = (depth: number) => Math.min(4, Math.max(1, depth));

const SLOTS = 8;
const BRANCH = Array.from({ length: SLOTS }, (_, s) => {
  const hue = (V.branch as unknown as Record<string, Record<string, string>>)[String(s + 1)]!;
  return [1, 2, 3, 4].map((k) => parse(hue[String(k)]!));
});
const NEUTRAL = parse(V.branch.neutral);
const RING = [1, 2, 3, 4].map((k) => parse((V.ring as Record<string, string>)[String(k)]!));

/** 「顶层分支」 fill of a row: its branch slot's hue at its depth's tier; slot 0 (or unknown) is neutral. */
export function branchFill(slot: number, depth: number): RGBA {
  return slot >= 1 && slot <= SLOTS ? BRANCH[slot - 1]![tier(depth) - 1]! : NEUTRAL;
}

/** Fill of an open folder's bar or sector when not colouring by branch. */
export function ringFill(depth: number): RGBA {
  return RING[tier(depth) - 1]!;
}

/** The zoom root's bar and centre disc: GL fill, label colours. */
export const CENTER = { bg: parse(V.center.bg), text: V.center.text, sub: V.center.sub } as const;

/** Laid over the ancestors of the hovered / selected row in the flame graphs. */
export const HALO_OVERLAY: RGBA = parse(V.haloOverlay);

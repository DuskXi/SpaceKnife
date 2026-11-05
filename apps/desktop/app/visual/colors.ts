// Colours the visualizations share beyond the treemap's (design/tokens/visual.tokens.json): the
// 「顶层分支」 palette, folder bars / sectors outside branch colouring, the zoom root, the halo overlay.
// Parsed once per theme (lib/theme.ts), read at each paint; no runtime import of treemap/paint,
// which imports this module.

import type { ThemeName } from "@design/tokens";

import { palette, themeName } from "@/lib/theme";
import type { RGBA } from "@/treemap/paint";

function parse(hex: string): RGBA {
  const h = hex.replace("#", "");
  const a = h.length >= 8 ? parseInt(h.slice(6, 8), 16) / 255 : 1;
  return [parseInt(h.slice(0, 2), 16) / 255, parseInt(h.slice(2, 4), 16) / 255, parseInt(h.slice(4, 6), 16) / 255, a];
}

/** Tier of a depth: rows below the zoom root start at depth 1; deeper than 4 shares tier 4. */
const tier = (depth: number) => Math.min(4, Math.max(1, depth));

const SLOTS = 8;

interface Parsed {
  branch: RGBA[][];
  neutral: RGBA;
  ring: RGBA[];
  center: { bg: RGBA; text: string; sub: string };
  halo: RGBA;
}

const parsed = new Map<ThemeName, Parsed>();

/** The theme on screen's colours, parsed the first time they are asked for. */
function current(): Parsed {
  const name = themeName();
  let p = parsed.get(name);
  if (!p) {
    const V = palette().visual;
    p = {
      branch: Array.from({ length: SLOTS }, (_, s) => {
        const hue = (V.branch as unknown as Record<string, Record<string, string>>)[String(s + 1)]!;
        return [1, 2, 3, 4].map((k) => parse(hue[String(k)]!));
      }),
      neutral: parse(V.branch.neutral),
      ring: [1, 2, 3, 4].map((k) => parse((V.ring as Record<string, string>)[String(k)]!)),
      center: { bg: parse(V.center.bg), text: V.center.text, sub: V.center.sub },
      halo: parse(V.haloOverlay),
    };
    parsed.set(name, p);
  }
  return p;
}

/** 「顶层分支」 fill of a row: its branch slot's hue at its depth's tier; slot 0 (or unknown) is neutral. */
export function branchFill(slot: number, depth: number): RGBA {
  const p = current();
  return slot >= 1 && slot <= SLOTS ? p.branch[slot - 1]![tier(depth) - 1]! : p.neutral;
}

/** Fill of an open folder's bar or sector when not colouring by branch. */
export function ringFill(depth: number): RGBA {
  return current().ring[tier(depth) - 1]!;
}

/** The zoom root's bar and centre disc: GL fill, label colours. */
export const center = (): Parsed["center"] => current().center;

/** Laid over the ancestors of the hovered / selected row in the flame graphs. */
export const haloOverlay = (): RGBA => current().halo;

// Compare mode (W6): colour tiers of a change, shared by the treemap, the legend and the lists.

import { tokens } from "@design/tokens";

import { getUnitBase } from "./format";

/** Lower bounds of tiers 1, 2 and 3 in bytes (tier 0 below the first). */
export type Tiers = [number, number, number];

const STEPS = [1, 3, 8];
const UNITS = ["KB", "MB", "GB"];

/** UI-W6-14/27: 1–3–8 GB; when the largest change is smaller the tiers step down (100 MB, 10 MB, …,
 * 1 KB) until it reaches the top tier, so a folder that changed by a few MB still shows depth (W6 待定 8). */
export function diffTiers(maxAbs: number, base: number = getUnitBase()): { tiers: Tiers; heads: string[] } {
  let unit = 2; // GB
  let mult = 1;
  while (!(unit === 0 && mult === 1) && maxAbs < 8 * mult * base ** (unit + 1)) {
    if (mult === 1) {
      unit--;
      mult = 100;
    } else mult /= 10;
  }
  const scale = mult * base ** (unit + 1);
  const tiers = STEPS.map((s) => s * scale) as Tiers;
  const n = STEPS.map((s) => String(s * mult));
  const heads = [`<${n[0]}`, `${n[0]}–${n[1]}`, `${n[1]}–${n[2]}`, `≥${n[2]} ${UNITS[unit]}`];
  return { tiers, heads };
}

export function tierOf(abs: number, tiers: Tiers): number {
  return abs < tiers[0] ? 0 : abs < tiers[1] ? 1 : abs < tiers[2] ? 2 : 3;
}

const GROW = [0, 1, 2, 3].map((i) => (tokens.treemap.diff.grow as Record<string, string>)[String(i)]!);
const SHRINK = [0, 1, 2].map((i) => (tokens.treemap.diff.shrink as Record<string, string>)[String(i)]!);

/** Growth: four tiers light → deep; shrinkage: three (≥ 3 GB share the deepest), UI-W6-14. */
export function diffColor(delta: number, tiers: Tiers): string {
  const t = tierOf(Math.abs(delta), tiers);
  return delta >= 0 ? GROW[t]! : SHRINK[Math.min(t, 2)]!;
}

export const GROW_COLORS = GROW;
export const SHRINK_COLORS = SHRINK;

/** Largest absolute change of the blocks that are coloured: by default files, aggregates and
 * collapsed folders (a treemap's open folders are not filled); a visualization says which rows it
 * fills with `filled` (the flame graphs fill every row). */
export function maxChange(delta: BigInt64Array, kind: Uint8Array, filled?: (row: number) => boolean): number {
  let m = 0;
  for (let i = 0; i < delta.length; i++) {
    if (filled ? !filled(i) : kind[i] === 1) continue;
    const a = Math.abs(Number(delta[i]));
    if (a > m) m = a;
  }
  return m;
}

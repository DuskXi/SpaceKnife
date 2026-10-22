// Turns a polar.v1 frame into sector instances for sectors.ts, labels and tag markers: the centre
// disc and one sector per row in the picture, the halo, the hover outline and the selection in an
// overlay that a hover rewrites on its own. Colours follow the flame graph's rules (docs/visuals.md):
// the compare scale, then 「顶层分支」, then file colours with ring tones for folders.

import { tokens } from "@design/tokens";

import type { PolarGeometry } from "@/protocol/generated";
import { fillOf, labelColors, paletteFill, Pattern, rgba, type RGBA, type TagItem } from "@/treemap/paint";

import { branchFill, CENTER, HALO_OVERLAY, ringFill } from "../colors";
import { DStat, Flag, Kind } from "../frame";
import { HIT_ROOT, type Hit, type PaintSettings, type PolarFrame } from "../types";
import { discRadius, polarGeometry, polarOf } from "./geometry";
import { rowLabel, type LabelSpec } from "./labels";

const T = tokens.treemap;

/** Floats per instance. */
export const STRIDE = 24;
/** Where each attribute of an instance starts (floats); sectors.ts binds them in this order. */
export const SLOT = {
  /** t0, t1 (turns from 12 o'clock), r0, r1 (px) */
  sector: 0,
  fill: 4,
  border: 8,
  pattern: 12,
  /** border width, pattern, pattern size, dashed */
  shape: 16,
  /** alpha, gap, inset (negative: grown, the selection rings and shadow), unused */
  extra: 20,
} as const;

const NONE: RGBA = [0, 0, 0, 0];

function luminance([r, g, b]: RGBA): number {
  const f = (x: number) => (x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

export interface SectorStyle {
  border?: RGBA;
  borderWidth?: number;
  pattern?: number;
  patternColor?: RGBA;
  patternSize?: number;
  dashed?: boolean;
  alpha?: number;
  /** the frame's seam gap (PolarGeometry.gap); 0 draws the sector edge to edge */
  gap?: number;
  /** outline moved in by this many px; negative grows it */
  inset?: number;
}

/** Instances in a buffer that only grows: a repaint reuses the array it is given. */
export class SectorBuilder {
  data: Float32Array;
  n = 0;

  constructor(reuse: Float32Array | null | undefined, capacity: number) {
    this.data = reuse && reuse.length >= capacity * STRIDE ? reuse : new Float32Array(Math.max(64, capacity) * STRIDE);
  }

  push(t0: number, t1: number, r0: number, r1: number, fill: RGBA, o: SectorStyle = {}): void {
    if ((this.n + 1) * STRIDE > this.data.length) {
      const next = new Float32Array(this.data.length * 2);
      next.set(this.data);
      this.data = next;
    }
    const d = this.data;
    const at = this.n++ * STRIDE;
    d[at + SLOT.sector] = t0;
    d[at + SLOT.sector + 1] = t1;
    d[at + SLOT.sector + 2] = r0;
    d[at + SLOT.sector + 3] = r1;
    d.set(fill, at + SLOT.fill);
    d.set(o.border ?? NONE, at + SLOT.border);
    d.set(o.patternColor ?? NONE, at + SLOT.pattern);
    d[at + SLOT.shape] = o.borderWidth ?? 0;
    d[at + SLOT.shape + 1] = o.pattern ?? Pattern.NONE;
    d[at + SLOT.shape + 2] = o.patternSize ?? 0;
    d[at + SLOT.shape + 3] = o.dashed ? 1 : 0;
    d[at + SLOT.extra] = o.alpha ?? 1;
    d[at + SLOT.extra + 1] = o.gap ?? 0;
    d[at + SLOT.extra + 2] = o.inset ?? 0;
    d[at + SLOT.extra + 3] = 0;
  }
}

/** Fill of a sector (not FREE / UNKNOWN / PENDING, which are striped): the compare scale, then
 * 「顶层分支」, then the file colours (fillOf) with ring tones for folders. */
export function sectorFill(f: PolarFrame, i: number, s: PaintSettings): RGBA {
  const kind = f.kind[i]!;
  if (s.diffTiers && f.delta) return fillOf(f, i, s.colorMode, s.classIds, s.diffTiers, s.classColors);
  if (s.colorMode === "branch") return branchFill(f.branch ? f.branch[i]! : 0, f.depth[i]!);
  // a plugin's colour mode colours folder bars too when its palette says so (folders = "palette")
  if (kind === Kind.FOLDER || kind === Kind.COLLAPSED) return s.palette?.folders ? paletteFill(f, i, s.palette) : ringFill(f.depth[i]!);
  return fillOf(f, i, s.colorMode, s.classIds, undefined, s.classColors, s.palette);
}

export interface Painted {
  instances: Float32Array;
  count: number;
}

export interface PolarPaint extends Painted {
  labels: LabelSpec[];
  tags: TagItem[];
  /** the selected row, -1 when none is drawn */
  selected: number;
}

/** Instances the overlay holds at most: the halo of the hovered and of the selected row (each up
 * to `haloLevels` ancestors, the centre disc among them), the selection's shadow with the row
 * lifted above it (and its halo again), the hover outline, the selection's double ring. */
export const overlayLimit = (haloLevels: number, rings: number): number => 2 * Math.min(haloLevels, rings + 1) + 6;

/** Push a row's sector; returns its opacity and label colours. */
function pushRow(b: SectorBuilder, f: PolarFrame, g: PolarGeometry, i: number, s: PaintSettings): { alpha: number; text: string; sub: string } {
  const kind = f.kind[i]!;
  const dim = (f.flags[i]! & Flag.DIM) !== 0;
  const dpr = f.info.dpr;
  const [t0, t1, r0, r1] = [f.a0[i]! / g.unit, f.a1[i]! / g.unit, f.r0[i]!, f.r1[i]!];
  let alpha = dim ? T.dimOpacity : 1;
  if (kind === Kind.FREE || kind === Kind.UNKNOWN || kind === Kind.PENDING) {
    const P = kind === Kind.FREE ? T.free : kind === Kind.UNKNOWN ? T.unknown : T.pending;
    if (dim) alpha = kind === Kind.PENDING ? T.dimOpacity : 0.55;
    const edge = rgba(P.border);
    b.push(t0, t1, r0, r1, rgba(P.stripeA), {
      border: [edge[0], edge[1], edge[2], Math.min(1, edge[3] * s.borderContrast)],
      borderWidth: Math.max(1, Math.round(dpr)),
      pattern: kind === Kind.UNKNOWN ? Pattern.STRIPES_45 : Pattern.STRIPES_135,
      patternColor: rgba(P.stripeB),
      patternSize: (kind === Kind.UNKNOWN ? 3 : kind === Kind.PENDING ? 8 : T.geometry.stripeWidth) * dpr,
      dashed: kind !== Kind.FREE,
      alpha,
      gap: g.gap,
    });
    return { alpha, text: P.text, sub: P.sub };
  }
  const fill = sectorFill(f, i, s);
  const aggregate = kind === Kind.SMALL || kind === Kind.MORE;
  b.push(t0, t1, r0, r1, fill, {
    pattern: aggregate ? Pattern.DOTS : Pattern.NONE,
    patternColor: aggregate ? rgba(luminance(fill) > 0.18 ? T.dot.onLight : T.dot.onDark) : NONE,
    patternSize: 6 * dpr,
    alpha,
    gap: g.gap,
  });
  const [text, sub] = labelColors(fill);
  return { alpha, text, sub };
}

/** The picture of a frame: the centre disc, then the rows in order. */
export function paintFrame(f: PolarFrame, s: PaintSettings, reuse?: Float32Array | null): PolarPaint {
  const g = polarOf(f);
  const b = new SectorBuilder(reuse, f.count + 1);
  const labels: LabelSpec[] = [];
  const tags: TagItem[] = [];
  let selected = -1;
  if (!g) return { instances: b.data, count: 0, labels, tags, selected };
  const dpr = f.info.dpr;
  const tagAt = (hit: Hit, tag: number, alpha: number) => {
    const p = polarGeometry.anchor(f, hit, "tag");
    if (p) tags.push({ x: p.x - 7 * dpr, y: p.y - 7 * dpr, tag, alpha });
  };

  const root = f.info.root;
  const rd = discRadius(g);
  if (root && rd > 0) {
    const alpha = root.flags & Flag.DIM ? T.dimOpacity : 1;
    b.push(0, 1, 0, rd, CENTER.bg, { alpha });
    if (root.tag) tagAt(HIT_ROOT, root.tag, alpha);
  }

  for (let i = 0; i < f.count; i++) {
    if (f.flags[i]! & Flag.SELECTED) selected = i;
    const { alpha, text, sub } = pushRow(b, f, g, i, s);
    const label = rowLabel(f, i, s, text, sub, alpha);
    if (label) labels.push(label);
    if (f.tag[i]) tagAt(i, f.tag[i]!, alpha);
    // 「新增」 / 「已删除」 where the sector holds a 70 × 40 box (UI-W6-16)
    const st = f.dstat ? f.dstat[i]! : DStat.SAME;
    if (st === DStat.NEW || st === DStat.DELETED) {
      const p = polarGeometry.anchor(f, i, "badge");
      if (p) tags.push({ x: p.x - 15 * dpr, y: p.y - 7.5 * dpr, tag: 0, alpha, badge: st === DStat.NEW ? "new" : "deleted" });
    }
  }
  return { instances: b.data, count: b.n, labels, tags, selected };
}

/** The overlay: the halo over the ancestors of the hovered and the selected row (the centre disc
 * too); the selection's shadow with the row drawn again above it; the hover outline; the
 * selection's double ring on top. */
export function paintOverlay(f: PolarFrame, s: PaintSettings, hovered: Hit, selected: number, reuse?: Float32Array | null): Painted {
  const g = polarOf(f);
  const b = new SectorBuilder(reuse, overlayLimit(s.haloLevels, g?.rings ?? 0));
  if (!g) return { instances: b.data, count: 0 };
  const dpr = f.info.dpr;
  const gap = g.gap;
  const rd = discRadius(g);
  const disc = !!f.info.root && rd > 0;
  const row = (i: number) => [f.a0[i]! / g.unit, f.a1[i]! / g.unit, f.r0[i]!, f.r1[i]!] as const;
  const dimmed = (flags: number) => (flags & Flag.DIM ? T.dimOpacity : 1);
  const hovering = hovered >= 0 && hovered < f.count;
  const picked = selected >= 0 && selected < f.count;

  // ancestors lit around the hovered and the selected row (UI-TM-29), up to haloLevels
  const levels = s.haloLevels;
  const lit = new Set<number>();
  let litDisc = false;
  const light = (from: number) => {
    let p = f.parent[from]!;
    for (let n = 0; n < levels; n++) {
      if (p < 0) {
        litDisc = true;
        return;
      }
      lit.add(p);
      p = f.parent[p]!;
    }
  };
  if (hovering) light(hovered);
  if (levels === Infinity) {
    // all of the selection's path, as the backend marked it
    for (let i = 0; i < f.count; i++)
      if (f.flags[i]! & Flag.ON_PATH) {
        lit.add(i);
        litDisc = true;
      }
    if (picked) litDisc = true;
  } else if (picked) light(selected);
  if (litDisc && disc) b.push(0, 1, 0, rd, HALO_OVERLAY, { alpha: dimmed(f.info.root!.flags) });
  for (const i of [...lit].sort((p, q) => p - q)) b.push(...row(i), HALO_OVERLAY, { gap, alpha: dimmed(f.flags[i]!) });

  // the selection's shadow falls around it, not on it: the row goes again on top (UI-W7-31)
  const r = Math.round(T.geometry.selectRingWidth * dpr);
  if (picked && s.selectionShadow) {
    b.push(...row(selected), [0, 0, 0, 0.65], { pattern: Pattern.SHADOW, patternSize: 32 * dpr, gap, inset: -2 * r });
    pushRow(b, f, g, selected, s);
    if (lit.has(selected)) b.push(...row(selected), HALO_OVERLAY, { gap, alpha: dimmed(f.flags[selected]!) });
  }

  // hover: a white inset (UI-TM-28)
  if (s.hoverHighlight) {
    const inset = { border: rgba(T.state.hoverInset), borderWidth: Math.max(1, Math.round(dpr)), gap };
    if (hovering) b.push(...row(hovered), NONE, inset);
    else if (hovered === HIT_ROOT && disc) b.push(0, 1, 0, rd, NONE, inset);
  }

  // the selection's dark outer ring and white inner ring on top (UI-TM-30)
  if (picked) {
    b.push(...row(selected), NONE, { border: rgba(T.state.selectOuter), borderWidth: r, gap, inset: -2 * r });
    b.push(...row(selected), NONE, { border: rgba(T.state.selectRing), borderWidth: r, gap, inset: -r });
  }
  return { instances: b.data, count: b.n };
}


// Labels of the polar flame graph (docs/protocol.md §4.4, docs/visuals.md): the backend decides
// which rows carry one and in which direction (label bits); this module only places them.
// Tangential labels sit across the sector at mid radius, turned to stay upright; radial ones run
// along the bisector from the inner edge. The centre disc names the zoom root, or the hovered row.
// Text is fitted once per frame or hover, then drawn every display frame.

import { fmtShare } from "@/lib/format";
import { t } from "@/lib/i18n";
import { fit } from "@/treemap/text";

import { CENTER } from "../colors";
import { Flag, Kind, Label } from "../frame";
import { rowTitle } from "../rowTitle";
import { HIT_ROOT, type Hit, type PaintSettings, type PolarFrame } from "../types";
import { discRadius, pointAt, polarOf, sweepOf, tangentWidth, TURN } from "./geometry";

// the same stacks as treemap/paint.ts
const FONT_UI = '"Geist", "Geist Variable", "Noto Sans SC", "Noto Sans SC Variable", system-ui, sans-serif';
const FONT_MONO = '"Geist Mono", "Geist Mono Variable", ui-monospace, "Cascadia Mono", monospace';

export interface LabelLine {
  text: string;
  font: string;
  color: string;
  /** baseline offset from the anchor along the label's own vertical */
  dy: number;
}

/** Lines drawn around an anchor, turned by `rot` (radians, clockwise on screen). */
export interface LabelSpec {
  x: number;
  y: number;
  rot: number;
  align: "left" | "right" | "center";
  maxWidth: number;
  lines: LabelLine[];
  alpha: number;
}

export interface PreparedLabel extends LabelSpec {
  /** each line fitted to maxWidth ("" = not drawn) */
  shown: string[];
}

const QUARTER = Math.PI / 2;
const norm = (theta: number) => ((theta % TURN) + TURN) % TURN;

/** Rotation of a tangential label at angle θ (radians from 12 o'clock, clockwise): along the
 * tangent, turned half a turn on the lower half so it stays upright; always within [−90°, 90°]. */
export function tangentRotation(theta: number): number {
  const th = norm(theta);
  if (th <= QUARTER) return th;
  return th < 3 * QUARTER ? th - Math.PI : th - TURN;
}

/** Rotation and alignment of a radial label at θ: on the right half it reads outward from the
 * inner edge; on the left half it is turned half a turn and right-aligned to the inner edge. */
export function radialRotation(theta: number): { rot: number; align: "left" | "right" } {
  const th = norm(theta);
  return th < Math.PI ? { rot: th - QUARTER, align: "left" } : { rot: th + QUARTER - TURN, align: "right" };
}

/** Text width of a tangential label: the backend's Wt less 12 px. */
export const tangentMaxWidth = (delta: number, r0: number, r1: number, gap: number, dpr: number): number => tangentWidth(delta, r0, r1, gap, dpr) - 12 * dpr;

/** Text width of a radial label: the ring's thickness less 8 px. */
export const radialMaxWidth = (r0: number, r1: number, dpr: number): number => r1 - r0 - 8 * dpr;

/** Weight of a row's name: folders semibold (bold on a folder-mask hit), aggregates regular. */
function weightOf(kind: number, flags: number): number {
  if (kind === Kind.FOLDER || kind === Kind.COLLAPSED) return flags & Flag.FOLDER_HIT ? 700 : 600;
  if (kind === Kind.SMALL || kind === Kind.MORE) return 400;
  return kind === Kind.PENDING ? 600 : 500;
}

/** The size line of a row or the root: its size, or its signed change in compare views. */
function sizeText(s: PaintSettings, size: number, delta: number | null): string {
  return s.diffTiers && delta !== null ? s.formatDelta(delta) : s.formatSize(size);
}

/** The label of a row, placed by its label bits; null when the backend gave it none. */
export function rowLabel(f: PolarFrame, row: number, s: PaintSettings, color: string, sub: string, alpha: number): LabelSpec | null {
  const g = polarOf(f);
  const lbl = f.label[row]!;
  if (!g || !(lbl & Label.NAME)) return null;
  const title = rowTitle(f, row, s);
  if (!title.line1) return null;
  const dpr = f.info.dpr;
  const fsc = s.fontScale;
  const [r0, r1] = [f.r0[row]!, f.r1[row]!];
  const mid = (f.a0[row]! + f.a1[row]!) / 2;
  const theta = (mid / g.unit) * TURN;
  const weight = weightOf(f.kind[row]!, f.flags[row]!);
  if (lbl & Label.RADIAL) {
    const { rot, align } = radialRotation(theta);
    const p = pointAt(g, r0 + 4 * dpr, mid);
    const fs = 10.5 * fsc * dpr;
    return { x: p.x, y: p.y, rot, align, maxWidth: radialMaxWidth(r0, r1, dpr), lines: [{ text: title.line1, font: `${weight} ${fs}px ${FONT_UI}`, color, dy: fs * 0.36 }], alpha };
  }
  const p = pointAt(g, (r0 + r1) / 2, mid);
  const short = (lbl & Label.SHORT) !== 0;
  const fs = (short ? 10.5 : 11) * fsc * dpr;
  const name = { text: title.line1, font: `${weight} ${fs}px ${FONT_UI}`, color, dy: fs * 0.36 };
  const lines: LabelLine[] = [name];
  if (!short && lbl & Label.SIZE) {
    // two lines stacked along the radius, the name first in reading order
    const fs2 = 10.5 * fsc * dpr;
    const half = 7 * fsc * dpr;
    name.dy -= half;
    const delta = f.delta ? Number(f.delta[row]) : null;
    lines.push({ text: title.line2 ?? sizeText(s, Number(f.size[row]), delta), font: `400 ${fs2}px ${FONT_MONO}`, color: sub, dy: half + fs2 * 0.36 });
  }
  return { x: p.x, y: p.y, rot: tangentRotation(theta), align: "center", maxWidth: tangentMaxWidth(sweepOf(f, g, row), r0, r1, g.gap, dpr), lines, alpha };
}

/** The centre disc's text: the zoom root's name and size, or while a row is hovered its name,
 * size and share of the root. Lines that do not fit the disc's height are left out, last first. */
export function centreLabel(f: PolarFrame, s: PaintSettings, hovered: Hit): LabelSpec | null {
  const g = polarOf(f);
  const root = f.info.root;
  if (!g || !root) return null;
  const rd = discRadius(g);
  if (rd <= 0) return null;
  const dpr = f.info.dpr;
  const fsc = s.fontScale;
  const nameFont = `700 ${13 * fsc * dpr}px ${FONT_UI}`;
  const sizeFont = `400 ${11.5 * fsc * dpr}px ${FONT_MONO}`;
  const shareFont = `400 ${10.5 * fsc * dpr}px ${FONT_UI}`;
  const rows: { text: string; font: string; color: string; px: number }[] = [];
  if (hovered >= 0 && hovered < f.count) {
    const title = rowTitle(f, hovered, s);
    const size = Number(f.size[hovered]);
    if (title.line1) rows.push({ text: title.line1, font: nameFont, color: CENTER.text, px: 13 });
    rows.push({ text: title.line2 ?? sizeText(s, size, f.delta ? Number(f.delta[hovered]) : null), font: sizeFont, color: CENTER.sub, px: 11.5 });
    rows.push({ text: t("占 {pct}", { pct: fmtShare(size, root.size) }), font: shareFont, color: CENTER.sub, px: 10.5 });
  } else {
    const title = rowTitle(f, HIT_ROOT, s);
    if (title.line1) rows.push({ text: title.line1, font: nameFont, color: CENTER.text, px: 13 });
    rows.push({ text: title.line2 ?? sizeText(s, root.size, root.delta), font: sizeFont, color: CENTER.sub, px: 11.5 });
  }
  const lead = (r: { px: number }) => 1.35 * r.px * fsc * dpr;
  while (rows.length > 1 && rows.reduce((h, r) => h + lead(r), 0) > 1.5 * rd) rows.pop();
  const height = rows.reduce((h, r) => h + lead(r), 0);
  let top = -height / 2;
  const lines = rows.map((r) => {
    const dy = top + lead(r) / 2 + 0.36 * r.px * fsc * dpr;
    top += lead(r);
    return { text: r.text, font: r.font, color: r.color, dy };
  });
  return { x: g.cx, y: g.cy, rot: 0, align: "center", maxWidth: 1.6 * g.hole, lines, alpha: 1 };
}

/** Fit every line once (UI-TM-17); the display frames reuse the result. */
export function prepareLabels(ctx: CanvasRenderingContext2D, specs: LabelSpec[]): PreparedLabel[] {
  const out: PreparedLabel[] = [];
  for (const spec of specs) {
    if (spec.maxWidth <= 4) continue;
    const shown = spec.lines.map((line) => {
      ctx.font = line.font;
      return fit(ctx, line.text, spec.maxWidth);
    });
    if (shown.some(Boolean)) out.push({ ...spec, shown });
  }
  return out;
}

export function drawLabels(ctx: CanvasRenderingContext2D, items: PreparedLabel[], alpha = 1): void {
  ctx.save();
  ctx.textBaseline = "alphabetic";
  for (const it of items) {
    const a = it.alpha * alpha;
    if (a <= 0) continue;
    ctx.save();
    ctx.globalAlpha = a;
    ctx.textAlign = it.align;
    ctx.translate(it.x, it.y);
    if (it.rot) ctx.rotate(it.rot);
    it.lines.forEach((line, k) => {
      const text = it.shown[k];
      if (!text) return;
      ctx.font = line.font;
      ctx.fillStyle = line.color;
      ctx.fillText(text, 0, line.dy);
    });
    ctx.restore();
  }
  ctx.restore();
}

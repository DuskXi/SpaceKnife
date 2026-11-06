// What the view host draws over any visualization: file-change flashes and, under reduced motion,
// their static badges (UI-TM-43/44); the pointer trail (「鼠标拖尾」); and, on a canvas of its own so
// their motion never repaints the shapes or their labels, the breathing outline of folders still
// being scanned (UI-TM-42 / UI-W2-04), what lies in a recycle bin (「回收站特效」) and the class the
// pointer is on in 「类别统计」 (UI-MAIN-65). The rhythm is the host's; the outline of a row comes from
// the component's geometry, so a block, a bar and a sector flash, breathe and light up alike. The
// colours are the theme on screen's (lib/theme.ts), read as they are drawn.

import { icons } from "@design/icons";
import { tokens } from "@design/tokens";

import type { FlashItem } from "@/protocol/generated";
import { fmtSize } from "@/lib/format";
import { accentHex, palette, themeName } from "@/lib/theme";

import { Flag, Kind } from "./frame";
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
  const accent = accentHex(); // the user's (W7 外观 · 强调色)
  ctx.strokeStyle = accent;
  ctx.shadowColor = accent;
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
  const S = palette().treemap.state;
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
      ctx.fillStyle = S.newBadgeBg;
      ctx.fillRect(at.x, at.y, tw, th);
      ctx.fillStyle = S.newBadgeText;
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
  ctx.strokeStyle = S.flash;
  ctx.lineWidth = 2 * dpr;
  ctx.shadowColor = `${S.flash}8C`; // 55 %
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
  ctx.strokeStyle = palette().treemap.state.hoverInset;
  for (const m of left) {
    ctx.globalAlpha = 0.6 * (1 - (now - m.t) / TRAIL_MS);
    ctx.stroke(m.path);
  }
  ctx.restore();
  return left;
}

// ------------------------------------------------------------------ 「回收站特效」 (Flag.TRASH)

/** 「回收站特效」: moving (the dashes march, a band of light sweeps across now and then), still, or none. */
export type TrashMode = "animated" | "static" | "off";

export interface TrashMark {
  hit: Hit;
  /** the outline of the outermost row (dashed) */
  path: Path2D;
  /** what is veiled: the row itself in the treemap (its blocks lie inside it), the row and every
   * row of it in the flame graphs (their bars and rings lie beyond it) */
  area: Path2D;
  /** the bin folder itself: it carries the badge of what emptying it frees */
  bin: boolean;
  size: number;
  /** where the badge goes */
  badge: Pt | null;
  /** the row's bounds (the badge stays inside) */
  bounds: Box | null;
  /** around the whole area: the band of light crosses it */
  sweep: Box | null;
}

const TRASH_MAX = 200;
const TRASH_MEMBERS = 4000; // rows veiled with their bins in the flame graphs
const SWEEP_MS = 7000; // a band of light crosses each bin once in this time…
const SWEEP_SHARE = 0.4; // …during this share of it, then rests
const DASH_MS = 1800; // the outline's dashes move one dash and gap in this time
/** the bin icon of the design set (16 × 16, stroked), for the badge */
export const BIN_ICON = (icons.basket.match(/d="([^"]+)"/g) ?? []).map((d) => d.slice(3, -1)).join(" ");

/** The outermost rows in a recycle bin (the bin itself, or the top rows when the view is in it) and
 * the zoom root when it is in one, each with what lies in it: the largest first past TRASH_MAX. The
 * bin carries the badge, also as the zoom root of a flame graph or a polar flame graph. */
export function trashMarks<F extends VisualFrame>(g: VisualGeometry<F>, f: F, dpr: number, newPath: NewPath = path2d): TrashMark[] {
  const root = f.info.root;
  const rootIn = !!root && (root.flags & Flag.TRASH) !== 0;
  const nested = f.layout === "drawlist.v1"; // the treemap draws a folder's blocks inside it
  const inBin = (row: number) => row >= 0 && (f.flags[row]! & Flag.TRASH) !== 0;
  // each row in a bin under its outermost row in the bin (the zoom root when the view is in one)
  const groups = new Map<Hit, number[]>();
  if (rootIn && !nested) groups.set(HIT_ROOT, []);
  let members = 0;
  for (let i = 0; i < f.count; i++) {
    if (!inBin(i)) continue;
    let top = i;
    while (inBin(f.parent[top]!)) top = f.parent[top]!;
    const hit = rootIn && !nested ? HIT_ROOT : top;
    let rows = groups.get(hit);
    if (!rows) groups.set(hit, (rows = []));
    if (hit !== i && !nested && members < TRASH_MEMBERS) {
      rows.push(i);
      members++;
    }
  }
  const picked: { hit: Hit; area: number }[] = [];
  for (const hit of groups.keys()) {
    const b = g.box(f, hit);
    if (b.minor >= 3 * dpr || groups.get(hit)!.length) picked.push({ hit, area: b.major * b.minor });
  }
  if (picked.length > TRASH_MAX) picked.sort((a, b) => b.area - a.area).length = TRASH_MAX;
  const out: TrashMark[] = [];
  for (const { hit } of picked) {
    const path = newPath();
    const drawn = g.path(f, hit, 0.75 * dpr, path);
    const rows = groups.get(hit)!;
    let area = path;
    let sweep = g.bounds(f, hit);
    if (!nested) {
      area = newPath();
      let any = g.path(f, hit, 0, area);
      for (const r of rows) {
        if (!g.path(f, r, 0, area)) continue;
        any = true;
        sweep = union(sweep, g.bounds(f, r));
      }
      if (!any) continue;
    } else if (!drawn) continue;
    const flags = hit === HIT_ROOT ? root!.flags : f.flags[hit]!;
    const size = hit === HIT_ROOT ? root!.size : Number(f.size[hit]);
    out.push({ hit, path, area, bin: drawn && (flags & Flag.TRASH_BIN) !== 0, size, badge: g.anchor(f, hit, "badge"), bounds: g.bounds(f, hit), sweep });
  }
  return out;
}

function union(a: Box | null, b: Box | null): Box | null {
  if (!a || !b) return a ?? b;
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  return { x, y, w: Math.max(a.x + a.w, b.x + b.w) - x, h: Math.max(a.y + a.h, b.y + b.h) - y };
}

/** Whether the marks keep moving (the host's loop asks before each frame). */
export const trashMoves = (marks: TrashMark[], mode: TrashMode, reduced: boolean) => marks.length > 0 && mode === "animated" && !reduced;

/** 0–1 position of the band of light at `now`, null while it rests; each bin starts at its own time. */
export function sweepAt(now: number, seed: number): number | null {
  const t = ((((now + seed * 977) % SWEEP_MS) + SWEEP_MS) % SWEEP_MS) / SWEEP_MS;
  return t < SWEEP_SHARE ? t / SWEEP_SHARE : null;
}

const hatches = new Map<string, CanvasPattern | null>();
/** Fine cross-hatching in the bin's colour (one tile per theme and pixel ratio). */
function hatchOf(ctx: CanvasRenderingContext2D, dpr: number): CanvasPattern | null {
  const key = `${themeName()} ${dpr}`;
  if (hatches.has(key)) return hatches.get(key)!;
  let pattern: CanvasPattern | null = null;
  if (typeof OffscreenCanvas !== "undefined" && typeof ctx.createPattern === "function") {
    const n = Math.max(4, Math.round(7 * dpr));
    const tile = new OffscreenCanvas(n, n);
    const c = tile.getContext("2d");
    if (c) {
      c.strokeStyle = palette().treemap.trash.hatch;
      c.lineWidth = Math.max(1, 0.75 * dpr);
      c.beginPath();
      c.moveTo(0, n);
      c.lineTo(n, 0);
      c.moveTo(0, 0);
      c.lineTo(n, n);
      c.stroke();
      pattern = ctx.createPattern(tile, "repeat");
    }
  }
  hatches.set(key, pattern);
  return pattern;
}

/** Draw the marks: the contents dimmed under a veil and fine cross-hatching, a dashed outline with a
 * soft glow, and on the bin itself a badge with the bin icon and `label(size)`. `moving` also marches
 * the dashes and sweeps a band of light across now and then. */
export function drawTrash(
  ctx: CanvasRenderingContext2D,
  marks: TrashMark[],
  now: number,
  moving: boolean,
  dpr: number,
  label: (bytes: number) => string,
  newPath: (d?: string) => Path2D = (d) => new Path2D(d),
): void {
  if (!marks.length) return;
  const TRASH = palette().treemap.trash;
  const hatch = hatchOf(ctx, dpr);
  ctx.save();
  marks.forEach((m, k) => {
    ctx.save();
    ctx.clip(m.area);
    ctx.fillStyle = TRASH.veil;
    ctx.fill(m.area);
    if (hatch) {
      ctx.fillStyle = hatch;
      ctx.fill(m.area);
    }
    const p = moving && m.sweep ? sweepAt(now, k) : null;
    if (p !== null && m.sweep) {
      const b = m.sweep;
      const band = Math.max(28 * dpr, 0.2 * Math.min(b.w + b.h, 1600 * dpr));
      const s = -band + p * (b.w + b.h + 2 * band); // along the diagonal, from the top-left corner
      const grad = ctx.createLinearGradient(b.x + (s - band) / 2, b.y + (s - band) / 2, b.x + (s + band) / 2, b.y + (s + band) / 2);
      const clear = `${TRASH.sweep.slice(0, 7)}00`;
      grad.addColorStop(0, clear);
      grad.addColorStop(0.5, TRASH.sweep);
      grad.addColorStop(1, clear);
      ctx.fillStyle = grad;
      ctx.fillRect(b.x, b.y, b.w, b.h);
    }
    ctx.restore();
    ctx.save();
    const dash = 5 * dpr;
    const gap = 4 * dpr;
    ctx.setLineDash([dash, gap]);
    ctx.lineDashOffset = moving ? -((now % DASH_MS) / DASH_MS) * (dash + gap) : 0;
    ctx.strokeStyle = TRASH.outline;
    ctx.lineWidth = 1.5 * dpr;
    ctx.globalAlpha = 0.9;
    ctx.shadowColor = TRASH.outline;
    ctx.shadowBlur = 6 * dpr;
    ctx.stroke(m.path);
    ctx.restore();
    if (m.bin && m.badge && m.bounds) drawBinBadge(ctx, m, dpr, label(m.size), newPath, TRASH);
  });
  ctx.restore();
}

function drawBinBadge(ctx: CanvasRenderingContext2D, m: TrashMark, dpr: number, text: string, newPath: (d?: string) => Path2D, TRASH: { badgeBg: string; badgeText: string }): void {
  ctx.save();
  ctx.font = `600 ${11 * dpr}px ${tokens.font.family.ui}`;
  ctx.textBaseline = "middle";
  const icon = 12 * dpr;
  const pad = 7 * dpr;
  const w = pad + icon + 5 * dpr + ctx.measureText(text).width + pad;
  const h = 20 * dpr;
  const at = badgeAt(m.badge!, m.bounds!, w, h, 4 * dpr);
  if (at) {
    ctx.shadowColor = palette().treemap.state.selectShadow;
    ctx.shadowBlur = 8 * dpr;
    ctx.fillStyle = TRASH.badgeBg;
    ctx.beginPath();
    ctx.roundRect(at.x, at.y, w, h, h / 2);
    ctx.fill();
    ctx.shadowBlur = 0;
    ctx.fillStyle = TRASH.badgeText;
    ctx.fillText(text, at.x + pad + icon + 5 * dpr, at.y + h / 2 + 0.5 * dpr);
    ctx.translate(at.x + pad, at.y + (h - icon) / 2);
    ctx.scale(icon / 16, icon / 16);
    ctx.strokeStyle = TRASH.badgeText;
    ctx.lineWidth = 1.6;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.stroke(newPath(BIN_ICON));
  }
  ctx.restore();
}

// ------------------------------------------------------------------ class highlight (UI-MAIN-65)

/** One class lit in the picture: everything else under a veil, its rows cut out of it and outlined. */
export interface ClassMark {
  /** the class index (``filter.vocab`` order) */
  cls: number;
  /** what stays bright: the rows of the class */
  cut: Path2D;
  /** the outlines of the rows roomy enough for one */
  outline: Path2D;
  rows: number;
  outlined: number;
}

/** rows a class can light: files, aggregates of one class, folders a folder class swallows (the
 * backend sends 255 for every other folder, mixed aggregates, "+N items", free and unknown space) */
const CLASS_KINDS: ReadonlySet<number> = new Set([Kind.FILE, Kind.SMALL, Kind.FOLDER, Kind.COLLAPSED, Kind.PENDING]);
/** the veil comes and goes this fast (not under 「减少动画」) */
export const CLASS_FADE_MS = 120;
const GLOW_MAX = 2000; // past this many outlines the glow is left out

/** The rows of class `cls` in `scope` (a row and what lies in it; -1: the whole picture). In the
 * treemap a lit folder holds its blocks, so they are not drawn again. A scope drawn without its
 * contents (a collapsed folder) lights itself when nothing in it can. */
export function classMarks<F extends VisualFrame>(g: VisualGeometry<F>, f: F, cls: number, scope: number, dpr: number, newPath: NewPath = path2d): ClassMark {
  const nested = f.layout === "drawlist.v1";
  // rows come before their children, so one pass carries both down the tree
  const inside = scope >= 0 && scope < f.count ? new Uint8Array(f.count) : null;
  const lit = nested ? new Uint8Array(f.count) : null;
  const cut = newPath();
  const outline = newPath();
  const min = 4 * dpr;
  let rows = 0;
  let outlined = 0;
  const light = (i: number) => {
    if (!g.path(f, i, 0, cut)) return false;
    rows++;
    if (g.box(f, i).minor >= min && g.path(f, i, 0.75 * dpr, outline)) outlined++;
    return true;
  };
  if (inside) inside[scope] = 1;
  for (let i = 0; i < f.count; i++) {
    const p = f.parent[i]!;
    if (inside && i !== scope) {
      if (p < 0 || !inside[p]) continue;
      inside[i] = 1;
    }
    if (lit && p >= 0 && lit[p]) {
      lit[i] = 1;
      continue;
    }
    if (f.cls[i] !== cls || !CLASS_KINDS.has(f.kind[i]!)) continue;
    if (light(i) && lit) lit[i] = 1;
  }
  if (!rows && inside && (f.kind[scope] === Kind.COLLAPSED || f.kind[scope] === Kind.PENDING)) light(scope);
  return { cls, cut, outline, rows, outlined };
}

/** The glow around the lit rows: the class colour, the accent for a class too dark to glow. */
export function glowOf(hex: string): string {
  const v = /^#?([0-9a-f]{6})/i.exec(hex);
  if (!v) return accentHex();
  const n = parseInt(v[1]!, 16);
  const lin = (c: number) => ((c /= 255) <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const lum = 0.2126 * lin(n >> 16) + 0.7152 * lin((n >> 8) & 255) + 0.0722 * lin(n & 255);
  return lum < 0.06 ? accentHex() : `#${v[1]}`;
}

/** Draw the mark at `level` (0–1, the fade): the veil over the whole canvas minus the class's rows,
 * then their outlines with a soft glow. Drawn first on its canvas: the bins and the breathing go over it. */
export function drawClassMark(ctx: CanvasRenderingContext2D, m: ClassMark, color: string, level: number, w: number, h: number, dpr: number): void {
  if (level <= 0) return;
  const FOCUS = palette().treemap.classFocus;
  ctx.save();
  ctx.globalAlpha = level;
  ctx.fillStyle = FOCUS.veil;
  ctx.fillRect(0, 0, w, h);
  if (m.rows) {
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = "destination-out";
    ctx.fill(m.cut);
    ctx.globalCompositeOperation = "source-over";
  }
  if (m.outlined) {
    ctx.globalAlpha = level;
    ctx.strokeStyle = FOCUS.outline;
    ctx.lineWidth = dpr;
    if (m.outlined <= GLOW_MAX) {
      ctx.shadowColor = glowOf(color);
      ctx.shadowBlur = 8 * dpr;
    }
    ctx.stroke(m.outline);
  }
  ctx.restore();
}

/** The fade toward `on` after `dt` ms: there at once under 「减少动画」. */
export function classFade(level: number, on: boolean, dt: number, reduced: boolean): number {
  if (reduced) return on ? 1 : 0;
  const step = Math.max(0, dt) / CLASS_FADE_MS;
  return on ? Math.min(1, level + step) : Math.max(0, level - step);
}

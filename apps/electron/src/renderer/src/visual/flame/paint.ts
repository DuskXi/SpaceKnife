// flame.v1 paint (docs/visuals.md): the zoom root's bar and one solid bar per row as instances of
// treemap/gl.ts's rounded-rectangle program, with their labels and markers; and the overlay the
// pointer changes (halo, hover inset), the selection on top. Pure: no GL, no DOM.

import { palette } from "@/lib/theme";
import { dupeOf, dupePatternSize, dupeStyle, fillOf, labelColors, type Painted, Pattern, type RGBA, rgba, STRIDE, subOn, type TagItem, type TextItem } from "@/treemap/paint";
import type { PreparedText } from "@/treemap/text";

import { branchFill, center, haloOverlay, ringFill } from "../colors";
import { DStat, Flag, Kind, Label } from "../frame";
import { rowTitle } from "../rowTitle";
import { type DrawPass, HIT_ROOT, type Hit, type PaintSettings, type RectFrame, type VisualFrame } from "../types";
import { flameGeometry, flameOf, rootBar } from "./geometry";

// as treemap/paint.ts
const FONT_UI = '"Geist", "Geist Variable", "Noto Sans SC", "Noto Sans SC Variable", system-ui, sans-serif';
const FONT_MONO = '"Geist Mono", "Geist Mono Variable", ui-monospace, "Cascadia Mono", monospace';
const NONE: RGBA = [0, 0, 0, 0];

/** Instances the selection adds to the overlay: shadow, the selected bar again, outer and inner ring. */
export const SELECTION_INSTANCES = 4;

/** Most instances an overlay holds: the halo (the root bar is one of the lit levels), the hover inset, the selection. */
export const overlayLimit = (haloLevels: number): number => Math.min(haloLevels, 255) + 1 + SELECTION_INSTANCES;

/** Slot options of one instance (see the vertex shader in treemap/gl.ts). */
interface Shape {
  border?: RGBA;
  borderWidth?: number;
  pattern?: number;
  patternColor?: RGBA;
  patternSize?: number;
  radius?: number;
  dashed?: boolean;
  alpha?: number;
}

/** An instance buffer a component instance keeps: refilled by every paint, it only ever grows. */
export class Instances {
  data: Float32Array;
  count = 0;

  constructor(capacity = 64) {
    this.data = new Float32Array(Math.max(64, capacity) * STRIDE);
  }

  /** Empty, with room for at least `capacity` instances (doubling, so a growing scan does not reallocate every frame). */
  reset(capacity = 0): this {
    if (capacity * STRIDE > this.data.length) this.data = new Float32Array(Math.max(capacity * STRIDE, this.data.length * 2));
    this.count = 0;
    return this;
  }

  push(x: number, y: number, w: number, h: number, fill: RGBA, s: Shape = {}): void {
    if ((this.count + 1) * STRIDE > this.data.length) {
      const next = new Float32Array(this.data.length * 2);
      next.set(this.data);
      this.data = next;
    }
    const d = this.data;
    const o = this.count++ * STRIDE;
    d[o] = x;
    d[o + 1] = y;
    d[o + 2] = w;
    d[o + 3] = h;
    d.set(fill, o + 4);
    d.set(s.border ?? NONE, o + 8);
    d.set(s.patternColor ?? NONE, o + 12);
    d[o + 16] = s.radius ?? 0;
    d[o + 17] = 0;
    d[o + 18] = s.borderWidth ?? 0;
    d[o + 19] = s.pattern ?? Pattern.NONE;
    d[o + 20] = s.patternSize ?? 0;
    d[o + 21] = s.dashed ? 1 : 0;
    d[o + 22] = s.alpha ?? 1;
    d[o + 23] = 0;
  }
}

function luminance([r, g, b]: RGBA): number {
  const f = (x: number) => (x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

/** `top` composited over `under` (straight alpha): the halo baked into a bar's own fill. */
function over(top: RGBA, under: RGBA): RGBA {
  const a = top[3];
  const c = (i: number) => top[i]! * a + under[i]! * (1 - a);
  return [c(0), c(1), c(2), a + under[3] * (1 - a)];
}

/** Fill of a bar: the compare scale, else 「顶层分支」, else the files' colour mode with folder bars on the ring tones. */
export function fillFor(f: RectFrame, i: number, s: PaintSettings): RGBA {
  if (s.diffTiers && f.delta) return fillOf(f, i, s.colorMode, s.classIds, s.diffTiers, s.classColors);
  if (s.colorMode === "branch") return branchFill(f.branch?.[i] ?? 0, f.depth[i]!);
  if (s.colorMode === "dupes") return dupeStyle(f, i).fill; // folder bars: a heat map of extra copies
  const kind = f.kind[i]!;
  if (kind === Kind.FOLDER || kind === Kind.COLLAPSED) return ringFill(f.depth[i]!);
  return fillOf(f, i, s.colorMode, s.classIds, undefined, s.classColors);
}

interface Bar {
  fill: RGBA;
  shape: Shape;
  /** label colours */
  text: string;
  sub: string;
}

/** A row's bar: solid and rounded, dotted for aggregates, striped for free / unknown space and folders waiting for the scan. */
function barOf(f: RectFrame, i: number, s: PaintSettings): Bar {
  const T = palette().treemap;
  const dpr = f.info.dpr;
  const kind = f.kind[i]!;
  const dim = (f.flags[i]! & Flag.DIM) !== 0;
  const radius = T.geometry.fileRadius * dpr;
  if (kind === Kind.FREE || kind === Kind.UNKNOWN || kind === Kind.PENDING) {
    const S = kind === Kind.FREE ? T.free : kind === Kind.UNKNOWN ? T.unknown : T.pending;
    return {
      fill: rgba(S.stripeA),
      shape: {
        border: rgba(S.border),
        borderWidth: Math.max(1, Math.round(dpr)),
        pattern: kind === Kind.UNKNOWN ? Pattern.STRIPES_45 : Pattern.STRIPES_135,
        patternColor: rgba(S.stripeB),
        patternSize: (kind === Kind.UNKNOWN ? 3 : kind === Kind.PENDING ? 8 : T.geometry.stripeWidth) * dpr,
        radius,
        dashed: kind !== Kind.FREE,
        alpha: dim ? (kind === Kind.PENDING ? T.dimOpacity : 0.55) : 1,
      },
      text: S.text,
      sub: S.sub,
    };
  }
  const fill = fillFor(f, i, s);
  const aggregate = kind === Kind.SMALL || kind === Kind.MORE;
  const [text, sub0] = labelColors(fill);
  // 「重复」: files keep the treemap's stripes and the kept copy's rim, collapsed folders its tiers
  const dupe = dupeOf(f, i, s.colorMode, !!s.diffTiers);
  const sub = subOn(dupe, text, sub0);
  return {
    fill,
    shape: {
      radius,
      pattern: aggregate ? Pattern.DOTS : (dupe?.pattern ?? Pattern.NONE),
      patternColor: aggregate ? rgba(luminance(fill) > 0.18 ? T.dot.onLight : T.dot.onDark) : (dupe?.patternColor ?? NONE),
      patternSize: dupePatternSize(dupe) * dpr,
      ...(dupe?.rim ? { border: dupe.rim, borderWidth: Math.max(1, Math.round(1.5 * dpr)) } : {}),
      alpha: dim ? T.dimOpacity : 1,
    },
    text,
    sub,
  };
}

/** The selected row: FrameInfo.selected_row when that row carries the selection flag, else -1. */
export function selectedOf(f: RectFrame): number {
  const r = f.info.selected_row;
  return r != null && r >= 0 && r < f.count && (f.flags[r]! & Flag.SELECTED) !== 0 ? r : -1;
}

/** What lights up around a row: its ancestors, nearest first, at most `levels`; the root bar counts
 * as the last ancestor (lit when the row's depth is at most `levels`). Nothing for -1 or HIT_ROOT. */
export function haloOf(f: Pick<VisualFrame, "parent" | "count">, row: number, levels: number): { rows: number[]; root: boolean } {
  const rows: number[] = [];
  if (!(row >= 0 && row < f.count) || !(levels > 0)) return { rows, root: false };
  let p = f.parent[row]!;
  while (p >= 0 && rows.length < levels) {
    rows.push(p);
    p = f.parent[p]!;
  }
  return { rows, root: p < 0 && rows.length < levels };
}

const sizeText = (s: PaintSettings, size: number, delta: number | null): string =>
  s.diffTiers && delta !== null ? s.formatDelta(delta) : s.formatSize(size);

/** The frame's picture: the root bar, then one bar per row (instance 1 + i), with the selection's halo
 * in the fills; labels only where the backend set label bits. `out` is reused: the result's
 * `instances` is its buffer. */
export function paintFlame(f: RectFrame, s: PaintSettings, out: Instances = new Instances(f.count + 1)): Painted {
  const T = palette().treemap;
  const CENTER = center();
  const HALO = haloOverlay();
  const dpr = f.info.dpr;
  const fsc = s.fontScale;
  const b = out.reset(f.count + 1);
  const texts: TextItem[] = [];
  const tags: TagItem[] = [];
  const sel = selectedOf(f);
  const lit = haloOf(f, sel, s.haloLevels);
  const litRows = new Set(lit.rows);
  const radius = T.geometry.fileRadius * dpr;

  const marks = (hit: Hit, tag: number, dstat: number | null, alpha: number) => {
    const p = tag ? flameGeometry.anchor(f, hit, "tag") : null;
    if (p) tags.push({ x: p.x, y: p.y, tag, alpha });
    // 「新增」 / 「已删除」 on bars of at least 70 × 40 (UI-W6-16)
    if (dstat === DStat.NEW || dstat === DStat.DELETED) {
      // the anchor is the badge's centre; the treemap's spot: 6 px in from the left, 21 px up
      const q = flameGeometry.anchor(f, hit, "badge");
      if (q) tags.push({ x: q.x - 15 * dpr, y: q.y - 7.5 * dpr, tag: 0, alpha, badge: dstat === DStat.NEW ? "new" : "deleted" });
    }
  };

  // the zoom root's bar: header style, bold name with the size right after it
  const bar = rootBar(f);
  const root = f.info.root;
  if (bar && root) {
    const alpha = root.flags & Flag.DIM ? T.dimOpacity : 1;
    b.push(bar.x, bar.y, bar.w, bar.h, lit.root ? over(HALO, CENTER.bg) : CENTER.bg, { radius, alpha });
    const title = rowTitle(f, HIT_ROOT, s);
    const px = 11 * fsc * dpr;
    texts.push({
      x: bar.x + 6 * dpr,
      y: bar.y + bar.h / 2 + px * 0.36,
      text: title.line1 ?? "",
      font: `700 ${px}px ${FONT_UI}`,
      color: CENTER.text,
      maxWidth: bar.w - 6 * dpr - (root.tag ? 22 : 5) * dpr,
      sub: { text: title.line2 ?? sizeText(s, root.size, root.delta), font: `400 ${10.5 * fsc * dpr}px ${FONT_MONO}`, color: CENTER.sub },
      alpha,
    });
    marks(HIT_ROOT, root.tag, root.dstat, alpha);
  }

  for (let i = 0; i < f.count; i++) {
    const kind = f.kind[i]!;
    const flags = f.flags[i]!;
    const [x, y, w, h] = [f.x[i]!, f.y[i]!, f.w[i]!, f.h[i]!];
    const r = barOf(f, i, s);
    b.push(x, y, w, h, litRows.has(i) ? over(HALO, r.fill) : r.fill, r.shape);
    const alpha = r.shape.alpha ?? 1;

    // labels (UI-TM-14..17): the backend decided; laid out like the treemap's file blocks
    const lbl = f.label[i]!;
    const title = lbl & Label.NAME ? rowTitle(f, i, s) : null;
    if (title?.line1) {
      const big = (lbl & Label.BIG) !== 0;
      const short = (lbl & Label.SHORT) !== 0;
      const fs = (short ? 10.5 : big ? 13 : 11) * fsc * dpr;
      const weight =
        kind === Kind.FOLDER || kind === Kind.COLLAPSED ? (flags & Flag.FOLDER_HIT ? 700 : 600) : kind === Kind.PENDING ? 600 : kind === Kind.SMALL || kind === Kind.MORE ? 400 : 500;
      const y1 = short ? y + h / 2 + fs * 0.36 : y + (big ? 6 : 4) * fsc * dpr + fs * 0.92;
      texts.push({
        x: x + 6 * dpr,
        y: y1,
        text: title.line1,
        font: `${weight} ${fs}px ${FONT_UI}`,
        color: r.text,
        maxWidth: w - 6 * dpr - (f.tag[i] ? 22 : 5) * dpr,
        alpha,
      });
      // a lit row's size line takes the name's colour: the halo takes the contrast the size line's
      // lighter ink has to spare (UI-TM-29)
      if (!short && lbl & Label.SIZE)
        texts.push({
          x: x + 6 * dpr,
          y: y1 + (big ? 16 : 14) * fsc * dpr,
          text: title.line2 ?? sizeText(s, Number(f.size[i]), f.delta ? Number(f.delta[i]) : null),
          font: `400 ${(big ? 11.5 : 10.5) * fsc * dpr}px ${FONT_MONO}`,
          color: litRows.has(i) ? r.text : r.sub,
          maxWidth: w - 11 * dpr,
          alpha,
        });
    }
    marks(i, f.tag[i]!, f.dstat ? f.dstat[i]! : null, alpha);
  }
  return { instances: b.data, count: b.count, texts, tags };
}

/** The pointer's layer over the picture: the halo over the hovered row's ancestors (those the
 * selection does not light already), the selection's shadow, bar and rings (UI-TM-28: on top of
 * everything), and the hover inset under the rings (UI-TM-30). O(depth). `out` is reused. */
export function paintFlameOverlay(f: RectFrame, s: PaintSettings, hover: Hit, out: Instances = new Instances(16)): Painted {
  const P = palette();
  const T = P.treemap;
  const HALO = haloOverlay();
  const dpr = f.info.dpr;
  const b = out.reset();
  const radius = T.geometry.fileRadius * dpr;
  const sel = selectedOf(f);
  const selLit = haloOf(f, sel, s.haloLevels);
  const lit = haloOf(f, hover, s.haloLevels);
  for (const p of lit.rows) {
    if (selLit.rows.includes(p)) continue;
    b.push(f.x[p]!, f.y[p]!, f.w[p]!, f.h[p]!, HALO, { radius, alpha: f.flags[p]! & Flag.DIM ? T.dimOpacity : 1 });
  }
  const bar = rootBar(f);
  if (lit.root && !selLit.root && bar) b.push(bar.x, bar.y, bar.w, bar.h, HALO, { radius, alpha: f.info.root!.flags & Flag.DIM ? T.dimOpacity : 1 });

  const ring = Math.round(T.geometry.selectRingWidth * dpr);
  const sb = flameGeometry.bounds(f, sel);
  if (sb && s.selectionShadow) {
    b.push(sb.x - 2 * ring, sb.y - 2 * ring + 12 * dpr, sb.w + 4 * ring, sb.h + 4 * ring, rgba(P.treemap.state.selectShadow), {
      pattern: Pattern.SHADOW,
      patternSize: 32 * dpr,
      radius: 7 * dpr,
    });
    // the selected bar again, above its own shadow
    const r = barOf(f, sel, s);
    b.push(sb.x, sb.y, sb.w, sb.h, lit.rows.includes(sel) ? over(HALO, r.fill) : r.fill, r.shape);
  }
  if (s.hoverHighlight) {
    const hb = flameGeometry.bounds(f, hover);
    if (hb) b.push(hb.x, hb.y, hb.w, hb.h, NONE, { border: rgba(T.state.hoverInset), borderWidth: Math.max(1, Math.round(dpr)), radius });
  }
  if (sb) {
    b.push(sb.x - 2 * ring, sb.y - 2 * ring, sb.w + 4 * ring, sb.h + 4 * ring, NONE, { border: rgba(T.state.selectOuter), borderWidth: ring, radius: 7 * dpr });
    b.push(sb.x - ring, sb.y - ring, sb.w + 2 * ring, sb.h + 2 * ring, NONE, { border: rgba(T.state.selectRing), borderWidth: ring, radius: 6 * dpr });
  }
  return { instances: b.data, count: b.count, texts: [], tags: [] };
}

/** An affine of the bars: x·sx + tx, y·sy + ty. */
export interface Xform {
  sx: number;
  sy: number;
  tx: number;
  ty: number;
}

/** The zoom animation at `zoom.t`: identity at 0; at 1 the row's bar lies over the root bar
 * (sx = W/w, sy = 1, tx = −x·sx, ty = −(y_row − y_root)). Null = draw as laid out: no zoom, reduced
 * motion, or a row this frame does not have. */
export function zoomXform(f: RectFrame, zoom: DrawPass["zoom"], reduced: boolean): Xform | null {
  if (!zoom || reduced) return null;
  const g = flameOf(f);
  const row = zoom.row;
  if (!g || !(row >= 0 && row < f.count) || !(f.w[row]! > 0)) return null;
  const t = Math.min(1, Math.max(0, zoom.t));
  const sx = g.root.w / f.w[row]!;
  return { sx: 1 + (sx - 1) * t, sy: 1, tx: (g.root.x - f.x[row]! * sx) * t, ty: (g.root.y - f.y[row]!) * t };
}

/** Labels where the transform takes their anchors; the glyphs keep their size. */
export function moveTexts(items: readonly PreparedText[], m: Xform): PreparedText[] {
  return items.map((it) => {
    const x = it.x * m.sx + m.tx;
    return { ...it, x, y: it.y * m.sy + m.ty, subX: it.sub ? x + (it.subX - it.x) : it.subX };
  });
}

/** Tag markers and badges where the transform takes them, at their own size. */
export function moveTags(tags: readonly TagItem[], m: Xform): TagItem[] {
  return tags.map((tag) => ({ ...tag, x: tag.x * m.sx + m.tx, y: tag.y * m.sy + m.ty }));
}

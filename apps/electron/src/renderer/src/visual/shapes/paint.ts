// shapes.v1 paint: the zoom root's shape (when it has one) and one shape per row as instances of
// gl.ts's program, in the flame graphs' style (solid shapes, open folders on the level tones,
// dotted aggregates, striped free / unknown space and folders waiting for the scan), with their
// labels and markers; and the overlay the pointer changes (halo, hover inset), the selection on
// top. Labels go where the backend's bits say (layout/shapes.py): rectangles left-aligned as in the
// flame graph, round shapes and polygons centred in the band that holds them. Pure: no GL, no DOM.

import { palette } from "@/lib/theme";
import { labelColors, Pattern, type Painted, type RGBA, rgba, type TagItem, type TextItem } from "@/treemap/paint";
import type { PreparedText } from "@/treemap/text";

import { center, haloOverlay } from "../colors";
import { fillFor, haloOf, selectedOf } from "../flame/paint";
import { DStat, Flag, Kind, Label } from "../frame";
import { rowTitle } from "../rowTitle";
import { type DrawPass, HIT_ROOT, type Hit, type PaintSettings, type ShapesFrame } from "../types";
import { MAX_POINTS, SHAPE_STRIDE, SLOT } from "./gl";
import { bandWidth, centreOf, isRound, ONE_LINE, rootShape, type Shape, shapeOf, shapesGeometry, TOP_BAND, TOP_INSET, TWO_LINES } from "./geometry";

const FONT_UI = '"Geist", "Geist Variable", "Noto Sans SC", "Noto Sans SC Variable", system-ui, sans-serif';
const FONT_MONO = '"Geist Mono", "Geist Mono Variable", ui-monospace, "Cascadia Mono", monospace';
const NONE: RGBA = [0, 0, 0, 0];

/** Instances the selection adds to the overlay: shadow, the selected shape again, outer and inner ring. */
export const SELECTION_INSTANCES = 4;

/** Slot options of one instance (see the shaders in gl.ts). */
export interface Look {
  border?: RGBA;
  borderWidth?: number;
  pattern?: number;
  patternColor?: RGBA;
  patternSize?: number;
  dashed?: boolean;
  alpha?: number;
  /** grows the shape by this many px (negative: shrinks it) */
  offset?: number;
}

/** An instance buffer a component instance keeps: refilled by every paint, it only ever grows. */
export class ShapeInstances {
  data: Float32Array;
  count = 0;

  constructor(capacity = 64) {
    this.data = new Float32Array(Math.max(64, capacity) * SHAPE_STRIDE);
  }

  reset(capacity = 0): this {
    if (capacity * SHAPE_STRIDE > this.data.length) this.data = new Float32Array(Math.max(capacity * SHAPE_STRIDE, this.data.length * 2));
    this.count = 0;
    return this;
  }

  push(s: Shape, fill: RGBA, look: Look = {}): void {
    if ((this.count + 1) * SHAPE_STRIDE > this.data.length) {
      const next = new Float32Array(this.data.length * 2);
      next.set(this.data);
      this.data = next;
    }
    const d = this.data;
    const o = this.count++ * SHAPE_STRIDE;
    d.fill(0, o, o + SHAPE_STRIDE);
    d[o + SLOT.rect] = s.x;
    d[o + SLOT.rect + 1] = s.y;
    d[o + SLOT.rect + 2] = s.w;
    d[o + SLOT.rect + 3] = s.h;
    d.set(fill, o + SLOT.fill);
    d.set(look.border ?? NONE, o + SLOT.border);
    d.set(look.patternColor ?? NONE, o + SLOT.pattern);
    d[o + SLOT.shape] = s.r;
    d[o + SLOT.shape + 1] = look.borderWidth ?? 0;
    d[o + SLOT.shape + 2] = look.pattern ?? Pattern.NONE;
    d[o + SLOT.shape + 3] = look.patternSize ?? 0;
    d[o + SLOT.extra] = look.dashed ? 1 : 0;
    d[o + SLOT.extra + 1] = look.alpha ?? 1;
    d[o + SLOT.extra + 2] = look.offset ?? 0;
    const p = s.pts;
    const k = p ? Math.min(MAX_POINTS, p.length >> 1) : 0;
    d[o + SLOT.extra + 3] = k >= 3 ? k : 0;
    if (p && k >= 3) {
      // around the box's centre, as the fragment shader evaluates them
      const cx = s.x + s.w / 2;
      const cy = s.y + s.h / 2;
      for (let i = 0; i < k; i++) {
        d[o + SLOT.points + 2 * i] = p[2 * i]! - cx;
        d[o + SLOT.points + 2 * i + 1] = p[2 * i + 1]! - cy;
      }
    }
  }
}

function luminance([r, g, b]: RGBA): number {
  const f = (x: number) => (x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

/** `top` composited over `under` (straight alpha): the halo baked into a shape's own fill. */
function over(top: RGBA, under: RGBA): RGBA {
  const a = top[3];
  const c = (i: number) => top[i]! * a + under[i]! * (1 - a);
  return [c(0), c(1), c(2), a + under[3] * (1 - a)];
}

interface Styled {
  fill: RGBA;
  look: Look;
  text: string;
  sub: string;
}

/** A row's look: solid, dotted for aggregates, striped for free / unknown space and folders waiting for the scan. */
function styleOf(f: ShapesFrame, i: number, s: PaintSettings): Styled {
  const T = palette().treemap;
  const dpr = f.info.dpr;
  const kind = f.kind[i]!;
  const dim = (f.flags[i]! & Flag.DIM) !== 0;
  if (kind === Kind.FREE || kind === Kind.UNKNOWN || kind === Kind.PENDING) {
    const S = kind === Kind.FREE ? T.free : kind === Kind.UNKNOWN ? T.unknown : T.pending;
    return {
      fill: rgba(S.stripeA),
      look: {
        border: rgba(S.border),
        borderWidth: Math.max(1, Math.round(dpr)),
        pattern: kind === Kind.UNKNOWN ? Pattern.STRIPES_45 : Pattern.STRIPES_135,
        patternColor: rgba(S.stripeB),
        patternSize: (kind === Kind.UNKNOWN ? 3 : kind === Kind.PENDING ? 8 : T.geometry.stripeWidth) * dpr,
        dashed: kind !== Kind.FREE,
        alpha: dim ? (kind === Kind.PENDING ? T.dimOpacity : 0.55) : 1,
      },
      text: S.text,
      sub: S.sub,
    };
  }
  const fill = fillFor(f, i, s);
  const aggregate = kind === Kind.SMALL || kind === Kind.MORE;
  const [text, sub] = labelColors(fill);
  return {
    fill,
    look: {
      pattern: aggregate ? Pattern.DOTS : Pattern.NONE,
      patternColor: aggregate ? rgba(luminance(fill) > 0.18 ? T.dot.onLight : T.dot.onDark) : NONE,
      patternSize: 6 * dpr,
      alpha: dim ? T.dimOpacity : 1,
    },
    text,
    sub,
  };
}

const sizeText = (s: PaintSettings, size: number, delta: number | null): string =>
  s.diffTiers && delta !== null ? s.formatDelta(delta) : s.formatSize(size);

/** A rectangle's labels sit at its top left (as the flame graph's); round shapes' and polygons' are centred. */
const boxy = (sh: Shape) => !sh.pts && !isRound(sh);

interface LabelInk {
  text: string;
  sub: string;
  alpha: number;
  /** a lit row's size line takes the name's colour (UI-TM-29) */
  lit: boolean;
  bold: boolean;
}

/** The label of a shape as the backend's bits say: one or two lines, or a top line. */
export function labelOf(sh: Shape, bits: number, title: { line1: string | null; line2: string | null }, size: string, ink: LabelInk, dpr: number, fsc: number): TextItem[] {
  if (!(bits & Label.NAME) || !title.line1) return [];
  const lu = dpr * fsc;
  const out: TextItem[] = [];
  const left = boxy(sh);
  const c = centreOf(sh);
  const big = (bits & Label.BIG) !== 0;
  const weight = ink.bold ? 600 : 500;
  if (bits & Label.TOP) {
    const fs = 10.5 * lu;
    const t0 = TOP_INSET * lu;
    const width = left ? sh.w - 12 * dpr : bandWidth(sh, t0, t0 + TOP_BAND * lu) - 8 * lu;
    out.push({
      x: left ? sh.x + 6 * dpr : c.x,
      y: sh.y + t0 + fs * 0.92,
      text: title.line1,
      font: `600 ${fs}px ${FONT_UI}`,
      color: ink.text,
      maxWidth: width,
      alpha: ink.alpha,
      ...(left ? {} : { align: "center" as const }),
    });
    return out;
  }
  const short = (bits & Label.SHORT) !== 0;
  const fs = (short ? 10.5 : big ? 13 : 11) * lu;
  const mid = sh.h / 2;
  if (left) {
    const y1 = short ? sh.y + mid + fs * 0.36 : sh.y + (big ? 6 : 4) * lu + fs * 0.92;
    out.push({ x: sh.x + 6 * dpr, y: y1, text: title.line1, font: `${weight} ${fs}px ${FONT_UI}`, color: ink.text, maxWidth: sh.w - 11 * dpr, alpha: ink.alpha });
    if (!short && bits & Label.SIZE)
      out.push({
        x: sh.x + 6 * dpr,
        y: y1 + (big ? 16 : 14) * lu,
        text: title.line2 ?? size,
        font: `400 ${(big ? 11.5 : 10.5) * lu}px ${FONT_MONO}`,
        color: ink.lit ? ink.text : ink.sub,
        maxWidth: sh.w - 11 * dpr,
        alpha: ink.alpha,
      });
    return out;
  }
  const band = (short ? ONE_LINE : big ? 44 : TWO_LINES) * lu;
  const width = bandWidth(sh, mid - band / 2, mid + band / 2) - 8 * lu;
  if (short) {
    out.push({ x: c.x, y: c.y + fs * 0.36, text: title.line1, font: `${weight} ${fs}px ${FONT_UI}`, color: ink.text, maxWidth: width, alpha: ink.alpha, align: "center" });
    return out;
  }
  const sub = (big ? 11.5 : 10.5) * lu;
  out.push({ x: c.x, y: c.y - (big ? 3 : 2) * lu, text: title.line1, font: `${weight} ${fs}px ${FONT_UI}`, color: ink.text, maxWidth: width, alpha: ink.alpha, align: "center" });
  if (bits & Label.SIZE)
    out.push({ x: c.x, y: c.y + sub + (big ? 4 : 3) * lu, text: title.line2 ?? size, font: `400 ${sub}px ${FONT_MONO}`, color: ink.lit ? ink.text : ink.sub, maxWidth: width, alpha: ink.alpha, align: "center" });
  return out;
}

/** The frame's picture: the root's shape, then one shape per row (instance 1 + i with a root), the
 * selection's halo in the fills; labels where the backend set label bits. `out` is reused. */
export function paintShapes(f: ShapesFrame, s: PaintSettings, out: ShapeInstances = new ShapeInstances(f.count + 1)): Painted {
  const CENTER = center();
  const HALO = haloOverlay();
  const T = palette().treemap;
  const dpr = f.info.dpr;
  const fsc = s.fontScale;
  const b = out.reset(f.count + 1);
  const texts: TextItem[] = [];
  const tags: TagItem[] = [];
  const sel = selectedOf(f);
  const lit = haloOf(f, sel, s.haloLevels);
  const litRows = new Set(lit.rows);

  const marks = (hit: Hit, tag: number, dstat: number | null, alpha: number) => {
    const p = tag ? shapesGeometry.anchor(f, hit, "tag") : null;
    if (p) tags.push({ x: p.x, y: p.y, tag, alpha });
    if (dstat === DStat.NEW || dstat === DStat.DELETED) {
      const q = shapesGeometry.anchor(f, hit, "badge");
      if (q) tags.push({ x: q.x - 15 * dpr, y: q.y - 7.5 * dpr, tag: 0, alpha, badge: dstat === DStat.NEW ? "new" : "deleted" });
    }
  };

  // the zoom root's shape: the centre disc's colours, its name and size at the top
  const rs = rootShape(f);
  const root = f.info.root;
  if (rs && root) {
    const alpha = root.flags & Flag.DIM ? T.dimOpacity : 1;
    b.push(rs, lit.root ? over(HALO, CENTER.bg) : CENTER.bg, { alpha });
    const title = rowTitle(f, HIT_ROOT, s);
    if (title.line1) {
      const lu = dpr * fsc;
      const fs = 11 * lu;
      const t0 = TOP_INSET * lu;
      const left = boxy(rs);
      const c = centreOf(rs);
      const width = left ? rs.w - 12 * dpr : bandWidth(rs, t0, t0 + TOP_BAND * lu + 2 * lu) - 8 * lu;
      if (width > 30 * lu)
        texts.push({
          x: left ? rs.x + 6 * dpr : c.x,
          y: rs.y + t0 + fs * 0.92,
          text: title.line1,
          font: `700 ${fs}px ${FONT_UI}`,
          color: CENTER.text,
          maxWidth: width,
          sub: { text: title.line2 ?? sizeText(s, root.size, root.delta), font: `400 ${10.5 * lu}px ${FONT_MONO}`, color: CENTER.sub },
          alpha,
          ...(left ? {} : { align: "center" as const }),
        });
    }
    marks(HIT_ROOT, root.tag, root.dstat, alpha);
  }

  for (let i = 0; i < f.count; i++) {
    const sh = shapeOf(f, i)!;
    const st = styleOf(f, i, s);
    b.push(sh, litRows.has(i) ? over(HALO, st.fill) : st.fill, st.look);
    const alpha = st.look.alpha ?? 1;
    const bits = f.label[i]!;
    if (bits & Label.NAME) {
      const kind = f.kind[i]!;
      const bold = kind === Kind.FOLDER || kind === Kind.COLLAPSED || kind === Kind.PENDING;
      const ink: LabelInk = { text: st.text, sub: st.sub, alpha, lit: litRows.has(i), bold };
      texts.push(...labelOf(sh, bits, rowTitle(f, i, s), sizeText(s, Number(f.size[i]), f.delta ? Number(f.delta[i]) : null), ink, dpr, fsc));
    }
    marks(i, f.tag[i]!, f.dstat ? f.dstat[i]! : null, alpha);
  }
  return { instances: b.data, count: b.count, texts, tags };
}

/** The pointer's layer over the picture: the halo over the hovered row's ancestors (those the
 * selection does not light already), the selection's shadow, shape and rings on top, the hover
 * inset under the rings. O(depth). `out` is reused. */
export function paintShapesOverlay(f: ShapesFrame, s: PaintSettings, hover: Hit, out: ShapeInstances = new ShapeInstances(16)): Painted {
  const P = palette();
  const T = P.treemap;
  const HALO = haloOverlay();
  const dpr = f.info.dpr;
  const b = out.reset();
  const sel = selectedOf(f);
  const selLit = haloOf(f, sel, s.haloLevels);
  const lit = haloOf(f, hover, s.haloLevels);
  for (const p of lit.rows) {
    if (selLit.rows.includes(p)) continue;
    b.push(shapeOf(f, p)!, HALO, { alpha: f.flags[p]! & Flag.DIM ? T.dimOpacity : 1 });
  }
  const rs = rootShape(f);
  if (lit.root && !selLit.root && rs) b.push(rs, HALO, { alpha: f.info.root!.flags & Flag.DIM ? T.dimOpacity : 1 });

  const ring = Math.round(T.geometry.selectRingWidth * dpr);
  const ss = shapeOf(f, sel);
  if (ss && s.selectionShadow) {
    b.push({ ...ss, y: ss.y + 12 * dpr, pts: ss.pts ? shiftY(ss.pts, 12 * dpr) : null }, rgba(T.state.selectShadow), {
      pattern: Pattern.SHADOW,
      patternSize: 32 * dpr,
      offset: 2 * ring,
    });
    const st = styleOf(f, sel, s);
    b.push(ss, lit.rows.includes(sel) ? over(HALO, st.fill) : st.fill, st.look);
  }
  if (s.hoverHighlight) {
    const hs = hover === HIT_ROOT ? rs : shapeOf(f, hover);
    if (hs) b.push(hs, NONE, { border: rgba(T.state.hoverInset), borderWidth: Math.max(1, Math.round(dpr)) });
  }
  if (ss) {
    b.push(ss, NONE, { border: rgba(T.state.selectOuter), borderWidth: ring, offset: 2 * ring });
    b.push(ss, NONE, { border: rgba(T.state.selectRing), borderWidth: ring, offset: ring });
  }
  return { instances: b.data, count: b.count, texts: [], tags: [] };
}

function shiftY(pts: ArrayLike<number>, dy: number): number[] {
  const out = Array.from(pts);
  for (let i = 1; i < out.length; i += 2) out[i]! += dy;
  return out;
}

/** An affine of the picture: x·sx + tx, y·sy + ty. */
export interface Xform {
  sx: number;
  sy: number;
  tx: number;
  ty: number;
}

/** The zoom animation at `zoom.t`: identity at 0; at 1 the row's box lies over the root's shape (or
 * fills the canvas, keeping its proportions: a circle stays round). Null = draw as laid out. */
export function zoomXform(f: ShapesFrame, zoom: DrawPass["zoom"], reduced: boolean): Xform | null {
  if (!zoom || reduced) return null;
  const s = shapeOf(f, zoom.row);
  if (!s || !(s.w > 0 && s.h > 0)) return null;
  const target = rootShape(f) ?? { x: 0, y: 0, w: f.info.width, h: f.info.height };
  const k = Math.min(target.w / s.w, target.h / s.h);
  const tx = target.x + (target.w - s.w * k) / 2 - s.x * k;
  const ty = target.y + (target.h - s.h * k) / 2 - s.y * k;
  const t = Math.min(1, Math.max(0, zoom.t));
  if (t === 0) return { sx: 1, sy: 1, tx: 0, ty: 0 };
  return { sx: 1 + (k - 1) * t, sy: 1 + (k - 1) * t, tx: tx * t, ty: ty * t };
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

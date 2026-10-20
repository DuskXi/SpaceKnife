// Turns a draw frame into GPU instances (shapes) and canvas labels, following the carbon theme
// (docs/UI-SPEC.md T section: colours, geometry, label rules, layering).

import { tokens } from "@design/tokens";

import { diffColor, type Tiers } from "@/lib/diff";
import { t } from "@/lib/i18n";
import { branchFill } from "@/visual/colors";

import { DStat, Flag, Kind, Label, type DrawFrame } from "./frame";

export type ColorMode = "class" | "flat" | "time" | "branch";

/** Floats per instance; see the vertex shader in gl.ts for the meaning of each slot. */
export const STRIDE = 24;
export const Pattern = { NONE: 0, STRIPES_135: 1, STRIPES_45: 2, DOTS: 3, SHADOW: 4 } as const;

const T = tokens.treemap;
const FONT_UI = '"Geist", "Geist Variable", "Noto Sans SC", "Noto Sans SC Variable", system-ui, sans-serif';
const FONT_MONO = '"Geist Mono", "Geist Mono Variable", ui-monospace, "Cascadia Mono", monospace';

export type RGBA = [number, number, number, number];
type Level = { bg: string; border: string; headerBg: string; headerText: string; headerSub: string };

const cache = new Map<string, RGBA>();
export function rgba(hex: string, alpha = 1): RGBA {
  const k = hex + alpha;
  let c = cache.get(k);
  if (!c) {
    const h = hex.replace("#", "");
    const a = h.length >= 8 ? parseInt(h.slice(6, 8), 16) / 255 : 1;
    c = [parseInt(h.slice(0, 2), 16) / 255, parseInt(h.slice(2, 4), 16) / 255, parseInt(h.slice(4, 6), 16) / 255, a * alpha];
    cache.set(k, c);
  }
  return c;
}

function luminance([r, g, b]: RGBA): number {
  const f = (x: number) => (x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}
const contrast = (a: RGBA, b: RGBA) => {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p) as [number, number];
  return (x + 0.05) / (y + 0.05);
};
/** UI-TM-19: white or near-black text, whichever contrasts more with the fill. */
export function labelColors(fill: RGBA): [string, string] {
  return contrast(fill, rgba(T.label.light)) >= contrast(fill, rgba(T.label.dark))
    ? [T.label.light, T.label.lightSub]
    : [T.label.dark, T.label.darkSub];
}

export interface TextItem {
  x: number;
  y: number;
  text: string;
  font: string;
  color: string;
  maxWidth: number;
  /** a header: the size goes right after the name */
  sub?: { text: string; font: string; color: string };
  alpha: number;
}

export interface TagItem {
  x: number;
  y: number;
  tag: number;
  alpha: number;
  /** compare views: a 「新增」 (solid) or 「已删除」 (outline) badge instead of a tag marker (UI-W6-16) */
  badge?: "new" | "deleted";
}

export interface PaintOptions {
  dpr: number;
  colorMode: ColorMode;
  classIds: string[];
  /** hex colour per class index (W7 文件类别: user colours); the theme's by id when missing */
  classColors?: string[];
  hovered: number;
  scanning: boolean;
  formatSize: (bytes: number) => string;
  formatInt: (n: number) => string;
  /** ancestors lit around the hovered / selected block (W7 外观 · 高亮光晕层数); all when omitted */
  haloLevels?: number;
  /** white inset on the hovered block (W7 外观 · 悬停高亮), default on */
  hoverHighlight?: boolean;
  /** drop shadow under the selection (W7 外观 · 选中投影), default on */
  selectionShadow?: boolean;
  /** 「元素字号」: label fonts and the folder header grow with it (the backend lays out to match), 1 = 100 % */
  fontScale?: number;
  /** 「层级对比」: how far deeper folders' tones move from the top level's, 1 = the design */
  levelContrast?: number;
  /** 「边框对比」: strength of block borders, 1 = the design */
  borderContrast?: number;
  /** compare views (W6): colour by change tier, labels show the signed change */
  diff?: { tiers: Tiers; formatDelta: (bytes: number) => string };
}

/** ``a`` moved toward ``b`` by ``t`` (t > 1 goes past ``b``), channels clamped. */
function mix(a: RGBA, b: RGBA, t: number): RGBA {
  const c = (i: number) => Math.min(1, Math.max(0, a[i]! + (b[i]! - a[i]!) * t));
  return [c(0), c(1), c(2), c(3)];
}

export interface Painted {
  instances: Float32Array;
  count: number;
  texts: TextItem[];
  tags: TagItem[];
}

class Builder {
  data: Float32Array;
  n = 0;
  constructor(capacity: number) {
    this.data = new Float32Array(Math.max(64, capacity) * STRIDE);
  }
  push(
    x: number,
    y: number,
    w: number,
    h: number,
    fill: RGBA,
    opts: {
      border?: RGBA;
      borderWidth?: number;
      pattern?: number;
      patternColor?: RGBA;
      patternSize?: number;
      radius?: number;
      topOnly?: boolean;
      dashed?: boolean;
      alpha?: number;
    } = {},
  ): void {
    if (this.n * STRIDE >= this.data.length) {
      const next = new Float32Array(this.data.length * 2);
      next.set(this.data);
      this.data = next;
    }
    const d = this.data;
    const o = this.n++ * STRIDE;
    d[o] = x;
    d[o + 1] = y;
    d[o + 2] = w;
    d[o + 3] = h;
    d.set(fill, o + 4);
    d.set(opts.border ?? [0, 0, 0, 0], o + 8);
    d.set(opts.patternColor ?? [0, 0, 0, 0], o + 12);
    d[o + 16] = opts.radius ?? 0;
    d[o + 17] = opts.topOnly ? 1 : 0;
    d[o + 18] = opts.borderWidth ?? 0;
    d[o + 19] = opts.pattern ?? Pattern.NONE;
    d[o + 20] = opts.patternSize ?? 0;
    d[o + 21] = opts.dashed ? 1 : 0;
    d[o + 22] = opts.alpha ?? 1;
    d[o + 23] = 0;
  }
}

const NONE: RGBA = [0, 0, 0, 0];

/** Fill of a block that is not an open folder. 「顶层分支」 needs the frame's branch and depth columns
 * (a frame without them is neutral). */
export function fillOf(
  f: Pick<DrawFrame, "kind" | "cls" | "tone" | "delta"> & Partial<Pick<DrawFrame, "branch" | "depth">>,
  i: number,
  mode: ColorMode,
  classIds: string[],
  tiers?: Tiers,
  classColors?: string[],
): RGBA {
  const kind = f.kind[i]!;
  if (tiers && f.delta) {
    const d = Number(f.delta[i]);
    return d === 0 ? rgba(T.collapsed) : rgba(diffColor(d, tiers));
  }
  // files, collapsed folders and aggregates take their branch's hue at their depth's tier
  if (mode === "branch") return branchFill(f.branch?.[i] ?? 0, f.depth?.[i] ?? 1);
  if (kind === Kind.COLLAPSED || kind === Kind.MORE) return rgba(T.collapsed);
  if (mode === "flat") return rgba(T.class.other);
  if (mode === "time" && (kind === Kind.FILE || kind === Kind.SMALL)) {
    return rgba((T.time as Record<string, string>)[String(f.tone[i])] ?? T.time[4]);
  }
  const custom = classColors?.[f.cls[i]!];
  if (custom) return rgba(custom);
  const cls = classIds[f.cls[i]!] ?? "other";
  return rgba((T.class as Record<string, string>)[cls] ?? T.class.other);
}

export function paint(f: DrawFrame, o: PaintOptions): Painted {
  const dpr = o.dpr;
  const b = new Builder(f.count * 2 + 8);
  const texts: TextItem[] = [];
  const tags: TagItem[] = [];
  const bw = Math.max(1, Math.round(dpr));
  const fsc = o.fontScale ?? 1;
  const headerH = Math.round(T.geometry.headerHeight * fsc * dpr);
  const levels4 = [0, 1, 2, 3].map((k) => (T.level as unknown as Record<string, Level>)[String(k)]!);
  const base = rgba(levels4[0]!.bg);
  const lc = o.levelContrast ?? 1;
  const levelBg = levels4.map((l) => mix(base, rgba(l.bg), lc));
  const bcon = o.borderContrast ?? 1;
  const border = (hex: string) => {
    const c = rgba(hex);
    return [c[0], c[1], c[2], Math.min(1, c[3] * bcon)] as RGBA;
  };
  // ancestors of the hovered and of the selected block get the halo header (UI-TM-29), up to haloLevels
  const levels = o.haloLevels ?? Infinity;
  const halo = new Set<number>();
  const light = (row: number) => {
    let n = 0;
    for (let p = f.parent[row]!; p >= 0 && n < levels; p = f.parent[p]!, n++) halo.add(p);
  };
  if (o.hovered >= 0) light(o.hovered);
  if (levels !== Infinity) {
    const sel = f.flags.findIndex((fl) => (fl & Flag.SELECTED) !== 0);
    if (sel >= 0) light(sel);
  }
  let selected = -1;

  for (let i = 0; i < f.count; i++) {
    const kind = f.kind[i]!;
    const x = f.x[i]!;
    const y = f.y[i]!;
    const w = f.w[i]!;
    const h = f.h[i]!;
    const flags = f.flags[i]!;
    if (flags & Flag.SELECTED) selected = i;
    const dim = (flags & Flag.DIM) !== 0;
    const size = Number(f.size[i]);
    const name = f.names[i]!;
    const lbl = f.label[i]!;

    if (kind === Kind.FOLDER) {
      const li = Math.min(f.depth[i]!, 4) - 1;
      const lvl = levels4[li]!;
      const alpha = dim ? 0.45 : 1;
      b.push(x, y, w, h, levelBg[li]!, {
        border: border(lvl.border),
        borderWidth: bw,
        radius: T.geometry.folderRadius * dpr,
        alpha,
      });
      const lit = (levels === Infinity && (flags & Flag.ON_PATH) !== 0) || halo.has(i);
      b.push(x + bw, y + bw, w - 2 * bw, headerH, rgba(lit ? T.halo.bg : lvl.headerBg), {
        radius: (T.geometry.folderRadius - 1) * dpr,
        topOnly: true,
        alpha,
      });
      const right = f.tag[i] ? 22 * dpr : 5 * dpr;
      texts.push({
        x: x + 6 * dpr,
        y: y + bw + 13 * dpr,
        text: name,
        font: `${flags & Flag.FOLDER_HIT ? 700 : 600} ${11 * fsc * dpr}px ${FONT_UI}`,
        color: lit ? T.halo.text : lvl.headerText,
        maxWidth: w - 6 * dpr - right,
        sub: {
          text: o.diff && f.delta ? o.diff.formatDelta(Number(f.delta[i])) : o.formatSize(size),
          font: `400 ${10.5 * fsc * dpr}px ${FONT_MONO}`,
          color: lit ? T.halo.sub : lvl.headerSub,
        },
        alpha,
      });
      if (f.tag[i] && w >= 22 * dpr) tags.push({ x: x + w - 18 * dpr, y: y + 2 * dpr, tag: f.tag[i]!, alpha });
      continue;
    }

    let fill: RGBA;
    let text: string;
    let sub: string;
    let line1 = name;
    let line2 = o.diff && f.delta ? o.diff.formatDelta(Number(f.delta[i])) : o.formatSize(size);
    let weight = 500;
    let alpha = dim ? T.dimOpacity : 1;
    if (kind === Kind.FREE || kind === Kind.UNKNOWN || kind === Kind.PENDING) {
      const P = kind === Kind.FREE ? T.free : kind === Kind.UNKNOWN ? T.unknown : T.pending;
      if (dim) alpha = kind === Kind.PENDING ? T.dimOpacity : 0.55;
      fill = rgba(P.stripeA);
      b.push(x, y, w, h, fill, {
        border: rgba(P.border),
        borderWidth: bw,
        pattern: kind === Kind.UNKNOWN ? Pattern.STRIPES_45 : Pattern.STRIPES_135,
        patternColor: rgba(P.stripeB),
        patternSize: (kind === Kind.UNKNOWN ? 3 : kind === Kind.PENDING ? 8 : T.geometry.stripeWidth) * dpr,
        radius: T.geometry.folderRadius * dpr,
        dashed: kind !== Kind.FREE,
        alpha,
      });
      text = P.text;
      sub = P.sub;
      if (kind === Kind.FREE) line1 = t("可用空间");
      if (kind === Kind.UNKNOWN) line1 = o.scanning ? t("未知空间 · 尚未扫描") : t("未知空间");
      if (kind === Kind.PENDING) {
        line2 = t("等待扫描…");
        weight = 600;
      }
    } else {
      fill = fillOf(f, i, o.colorMode, o.classIds, o.diff?.tiers, o.classColors);
      const aggregate = kind === Kind.SMALL || kind === Kind.MORE;
      const radius = (kind === Kind.COLLAPSED ? T.geometry.folderRadius : T.geometry.fileRadius) * dpr;
      b.push(x, y, w, h, fill, {
        radius,
        pattern: aggregate ? Pattern.DOTS : Pattern.NONE,
        patternColor: aggregate ? rgba(luminance(fill) > 0.18 ? T.dot.onLight : T.dot.onDark) : NONE,
        patternSize: 6 * dpr,
        alpha,
      });
      [text, sub] = labelColors(fill);
      if (aggregate) {
        line1 = kind === Kind.SMALL ? t("{n} 个小项", { n: o.formatInt(f.count_[i]!) }) : t("+{n} 项", { n: o.formatInt(f.count_[i]!) });
        weight = 400;
      } else if (kind === Kind.COLLAPSED) {
        weight = flags & Flag.FOLDER_HIT ? 700 : 600;
      }
    }

    // labels (UI-TM-14..17): the backend decided the level; aggregates and special blocks follow
    // the same thresholds with their own text
    const cw = w / dpr;
    const ch = h / dpr;
    const show = lbl & Label.NAME || (kind >= Kind.SMALL && cw >= 30 && ch >= 15);
    if (show) {
      const big = (lbl & Label.BIG) !== 0;
      const short = (lbl & Label.SHORT) !== 0 || ch < 22;
      const fs = short ? 10.5 : big ? 13 : 11;
      const right = (f.tag[i] ? 22 : 5) * dpr;
      const top = (big ? 6 : 4) * dpr;
      const y1 = short ? y + h / 2 + fs * dpr * 0.36 : y + top + fs * dpr * 0.92;
      texts.push({
        x: x + 6 * dpr,
        y: y1,
        text: line1,
        font: `${weight} ${fs * fsc * dpr}px ${FONT_UI}`,
        color: text,
        maxWidth: w - 6 * dpr - right,
        alpha,
      });
      const sizeLine = lbl & Label.SIZE || (kind >= Kind.SMALL && cw >= 42 && ch >= 31);
      if (!short && sizeLine) {
        texts.push({
          x: x + 6 * dpr,
          y: y1 + (big ? 16 : 14) * dpr,
          text: line2,
          font: `400 ${(big ? 11.5 : 10.5) * fsc * dpr}px ${FONT_MONO}`,
          color: sub,
          maxWidth: w - 11 * dpr,
          alpha,
        });
      }
    }
    if (f.tag[i] && w >= 22 * dpr && h >= 18 * dpr) tags.push({ x: x + w - 18 * dpr, y: y + 4 * dpr, tag: f.tag[i]!, alpha });
    // 「新增」 / 「已删除」 on blocks of at least 70 × 40 (UI-W6-16, W6 待定 7)
    const st = f.dstat ? f.dstat[i]! : DStat.SAME;
    if ((st === DStat.NEW || st === DStat.DELETED) && cw >= 70 && ch >= 40)
      tags.push({ x: x + 6 * dpr, y: y + h - 21 * dpr, tag: 0, alpha, badge: st === DStat.NEW ? "new" : "deleted" });
  }

  // state layers: hover inset, then the selection on top (UI-TM-28, UI-TM-30)
  if (o.hovered >= 0 && o.hovered < f.count && o.hoverHighlight !== false) {
    const i = o.hovered;
    b.push(f.x[i]!, f.y[i]!, f.w[i]!, f.h[i]!, NONE, {
      border: rgba(T.state.hoverInset),
      borderWidth: bw,
      radius: (f.kind[i] === Kind.FILE || f.kind[i]! >= Kind.SMALL ? T.geometry.fileRadius : T.geometry.folderRadius) * dpr,
    });
  }
  if (selected >= 0) {
    const i = selected;
    const r = Math.round(T.geometry.selectRingWidth * dpr);
    const [x, y, w, h] = [f.x[i]!, f.y[i]!, f.w[i]!, f.h[i]!];
    if (o.selectionShadow !== false)
      b.push(x - 2 * r, y - 2 * r + 12 * dpr, w + 4 * r, h + 4 * r, [0, 0, 0, 0.65], {
        pattern: Pattern.SHADOW,
        patternSize: 32 * dpr,
        radius: 7 * dpr,
      });
    b.push(x - 2 * r, y - 2 * r, w + 4 * r, h + 4 * r, NONE, { border: rgba(T.state.selectOuter), borderWidth: r, radius: 7 * dpr });
    b.push(x - r, y - r, w + 2 * r, h + 2 * r, NONE, { border: rgba(T.state.selectRing), borderWidth: r, radius: 6 * dpr });
  }
  return { instances: b.data, count: b.n, texts, tags };
}

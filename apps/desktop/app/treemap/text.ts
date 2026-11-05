// Canvas2D label layer: names, sizes, header text and tag markers drawn over the WebGL shapes.

import { themes } from "@design/tokens";

import { t } from "@/lib/i18n";
import { accentHex, onAccentHex, palette } from "@/lib/theme";

import type { TagItem, TextItem } from "./paint";

export interface PreparedText extends TextItem {
  shown: string;
  subShown: string;
  subX: number;
}

/** Truncate once per frame (UI-TM-17); animation frames reuse the result. */
export function prepareTexts(ctx: CanvasRenderingContext2D, items: TextItem[]): PreparedText[] {
  const out: PreparedText[] = [];
  for (const it of items) {
    if (it.maxWidth <= 4) continue;
    ctx.font = it.font;
    if (it.sub) {
      // a header: the size stays whole, the name gives way first (UI-TM-17)
      const nameW = ctx.measureText(it.text).width;
      ctx.font = it.sub.font;
      const subW = ctx.measureText(it.sub.text).width;
      const gap = 6 * (parseFloat(it.sub.font.split(" ")[1] ?? "10") / 10.5);
      ctx.font = it.font;
      let name = it.text;
      if (nameW + gap + subW > it.maxWidth) name = fit(ctx, it.text, Math.max(0, it.maxWidth - gap - subW));
      const nw = ctx.measureText(name).width;
      const showSub = name.length > 0 ? nw + gap + subW <= it.maxWidth : subW <= it.maxWidth;
      out.push({ ...it, shown: name, subShown: showSub ? it.sub.text : "", subX: it.x + (name ? nw + gap : 0) });
    } else {
      out.push({ ...it, shown: fit(ctx, it.text, it.maxWidth), subShown: "", subX: 0 });
    }
  }
  return out;
}

export function fit(ctx: CanvasRenderingContext2D, text: string, max: number): string {
  if (max <= 0) return "";
  if (ctx.measureText(text).width <= max) return text;
  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (ctx.measureText(text.slice(0, mid) + "…").width <= max) lo = mid;
    else hi = mid - 1;
  }
  return lo ? text.slice(0, lo) + "…" : "";
}

const TAG_NAMES = [null, "red", "yellow", "green", "blue"] as const;

export function drawTexts(ctx: CanvasRenderingContext2D, items: PreparedText[], alpha = 1): void {
  ctx.textBaseline = "alphabetic";
  for (const it of items) {
    if (!it.shown && !it.subShown) continue;
    ctx.globalAlpha = it.alpha * alpha;
    if (it.shown) {
      ctx.font = it.font;
      ctx.fillStyle = it.color;
      ctx.fillText(it.shown, it.x, it.y);
    }
    if (it.sub && it.subShown) {
      ctx.font = it.sub.font;
      ctx.fillStyle = it.sub.color;
      ctx.fillText(it.subShown, it.subX, it.y);
    }
  }
  ctx.globalAlpha = 1;
}

/** Tag markers (UI-TM-49): a 14 px dark base with a red ▲, yellow ■, green ● or blue ◆. The base is
 * dark in both themes, so the shapes on it keep the dark theme's tag colours (the light theme's are
 * darkened for light surfaces: the details panel, menus). */
export function drawTags(ctx: CanvasRenderingContext2D, tags: TagItem[], dpr: number, alpha = 1): void {
  const C = palette().color;
  const shapes = themes.dark.color.tag;
  for (const tag of tags) {
    const s = dpr;
    ctx.globalAlpha = tag.alpha * alpha;
    if (tag.badge) {
      drawBadge(ctx, tag, s);
      continue;
    }
    ctx.fillStyle = C.tag.base;
    ctx.beginPath();
    ctx.roundRect(tag.x, tag.y, 14 * s, 14 * s, 4 * s);
    ctx.fill();
    const name = TAG_NAMES[tag.tag];
    ctx.fillStyle = name ? shapes[name] : "#fff";
    const cx = tag.x + 7 * s;
    const cy = tag.y + 7 * s;
    ctx.beginPath();
    if (tag.tag === 1) {
      ctx.moveTo(cx, cy - 4.5 * s);
      ctx.lineTo(cx + 4.5 * s, cy + 3.5 * s);
      ctx.lineTo(cx - 4.5 * s, cy + 3.5 * s);
    } else if (tag.tag === 2) ctx.roundRect(cx - 4 * s, cy - 4 * s, 8 * s, 8 * s, s);
    else if (tag.tag === 3) ctx.arc(cx, cy, 4 * s, 0, Math.PI * 2);
    else {
      ctx.moveTo(cx, cy - 4.95 * s);
      ctx.lineTo(cx + 4.95 * s, cy);
      ctx.lineTo(cx, cy + 4.95 * s);
      ctx.lineTo(cx - 4.95 * s, cy);
    }
    ctx.closePath();
    ctx.fill();
  }
  ctx.globalAlpha = 1;
}

/** 「新增」: a solid accent pill; 「已删除」: an outlined neutral one (UI-W6-16, W6 待定 7). */
function drawBadge(ctx: CanvasRenderingContext2D, tag: TagItem, s: number): void {
  const C = palette().color;
  const text = tag.badge === "new" ? t("新增") : t("已删除");
  ctx.font = `600 ${10 * s}px "Geist", "Noto Sans SC", system-ui, sans-serif`;
  const w = ctx.measureText(text).width + 10 * s;
  const h = 15 * s;
  ctx.beginPath();
  ctx.roundRect(tag.x, tag.y, w, h, 4 * s);
  if (tag.badge === "new") {
    ctx.fillStyle = accentHex();
    ctx.fill();
    ctx.fillStyle = onAccentHex();
  } else {
    ctx.fillStyle = palette().treemap.state.deletedBadgeBg;
    ctx.fill();
    ctx.lineWidth = s;
    ctx.strokeStyle = C.text.secondary;
    ctx.stroke();
    ctx.fillStyle = C.text.primary;
  }
  ctx.textBaseline = "middle";
  ctx.fillText(text, tag.x + 5 * s, tag.y + h / 2 + 0.5 * s);
  ctx.textBaseline = "alphabetic";
}

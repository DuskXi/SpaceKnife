// What the colours of each colour mode mean (UI-MAIN-20 配色): the legend the toolbar's 「类别｜扁平｜
// 时间｜分支」 show when hovered, and the line of the hover card that says what the colour of the
// block under the pointer stands for. The colours are the ones the blocks are painted with
// (treemap/paint.ts fillOf), in the theme on screen.

import type { FilterVocab } from "@/protocol/generated";
import type { ColorModeName } from "@/state/visuals";
import type { SettingsPageId } from "@/state/store";
import type { RGBA } from "@/treemap/paint";
import { branchFill } from "@/visual/colors";
import { Kind } from "@/visual/frame";
import type { VisualFrame } from "@/visual/types";

import { colorOf } from "./classes";
import { t } from "./i18n";
import { palette } from "./theme";

/** 「时间」's bands: how long ago a file was last modified (sk-core view/state.rs TONE_LIMITS: 1, 7,
 * 30, 365 days; a file without a time is in the last). The frame's `tone` is the index. */
export const TIME_BANDS: readonly (() => string)[] = [
  () => t("1 天内"),
  () => t("1 天 – 1 周"),
  () => t("1 周 – 1 个月"),
  () => t("1 个月 – 1 年"),
  () => t("1 年以上"),
];

const bandLabel = (tone: number): string => (TIME_BANDS[tone] ?? TIME_BANDS[TIME_BANDS.length - 1]!)();

/** Branches the legend names; the rest are counted. */
export const LEGEND_BRANCHES = 8;

export interface LegendRow {
  color: string;
  label: string;
  /** a row of its own across the columns (the folded blocks after the classes) */
  wide?: boolean;
}

export interface ColorLegend {
  title: string;
  /** what decides the colour */
  lead: string;
  rows: LegendRow[];
  /** branches in view beyond the ones named */
  more: number;
  notes: string[];
  /** where its colours are set (the right-click on the mode) */
  settings: SettingsPageId;
}

export const cssColor = (c: RGBA): string => `rgb(${c.slice(0, 3).map((x) => Math.round(x * 255)).join(",")})`;

/** The settings page a colour mode's colours belong to: the classes' colours are the user's, the
 * other modes' come from the theme (外观 holds the default colour mode and the contrasts). */
export const legendSettings = (mode: ColorModeName): SettingsPageId => (mode === "class" ? "classes" : "appearance");

/** The first-level rows of a frame that have a branch colour, largest first. */
function branches(f: VisualFrame): { row: number; slot: number }[] {
  const out: { row: number; slot: number; size: number }[] = [];
  if (!f.branch) return out;
  for (let i = 0; i < f.count; i++) {
    const slot = f.branch[i]!;
    if (f.parent[i]! < 0 && f.kind[i]! <= Kind.PENDING && slot >= 1) out.push({ row: i, slot, size: Number(f.size[i]) });
  }
  return out.sort((a, b) => b.size - a.size);
}

/** The legend of a colour mode. `frame`: the view's latest picture (「分支」 names its branches). */
export function colorLegend(mode: ColorModeName, vocab: FilterVocab | null | undefined, frame: VisualFrame | null | undefined): ColorLegend {
  const T = palette().treemap;
  const folded: LegendRow = { color: T.collapsed, label: t("折叠的文件夹、「+N 项」"), wide: true };
  const settings = legendSettings(mode);
  switch (mode) {
    case "class":
      return {
        title: t("类别配色"),
        lead: t("按文件类别着色，类别和颜色在「文件类别」设置里改"),
        rows: [...(vocab?.classes ?? []).map((c) => ({ color: colorOf(c), label: c.name })), folded],
        more: 0,
        notes: [t("「N 个小项」取占一半及以上空间的类别，都不到一半的算「其他」")],
        settings,
      };
    case "flat":
      return {
        title: t("扁平配色"),
        lead: t("所有文件同一种颜色，只比较大小"),
        rows: [{ color: T.flat.file, label: t("文件和「N 个小项」") }, folded],
        more: 0,
        notes: [],
        settings,
      };
    case "time":
      return {
        title: t("时间配色"),
        lead: t("按距上次修改的时间着色"),
        rows: [...TIME_BANDS.map((label, i) => ({ color: (T.time as Record<string, string>)[String(i)]!, label: label() })), folded],
        more: 0,
        notes: [t("没有修改时间的文件算作 1 年以上；「N 个小项」取占空间最多的一档")],
        settings,
      };
    case "branch": {
      const all = frame ? branches(frame) : [];
      return {
        title: t("分支配色"),
        lead: t("当前位置下每个顶层项目一种颜色，里面各层沿用它的颜色，越往里越深"),
        rows: all.slice(0, LEGEND_BRANCHES).map((b) => ({ color: cssColor(branchFill(b.slot, 2)), label: frame!.names[b.row] || "" })),
        more: Math.max(0, all.length - LEGEND_BRANCHES),
        notes: [all.length ? t("颜色共 8 种，分支更多时会重复使用") : t("打开扫描视图后，这里列出当前位置的顶层分支")],
        settings,
      };
    }
  }
}

/** What the colour of `row` stands for in `mode`, for the hover card; null when the card's other
 * lines say it already (a file's class in 「类别」) or the colour means nothing more (「扁平」, a
 * folded block, a comparison's colours). */
export function colorMeaning(mode: ColorModeName, f: VisualFrame, row: number, vocab: FilterVocab | null | undefined): string | null {
  if (row < 0 || row >= f.count || f.delta) return null;
  const kind = f.kind[row]!;
  if (mode === "branch") {
    let top = row;
    while (f.parent[top]! >= 0) top = f.parent[top]!;
    const name = f.names[top];
    return (f.branch?.[row] ?? 0) >= 1 && name ? t("分支：{name}", { name }) : null;
  }
  if (mode === "time") {
    if (kind === Kind.FILE) return t("距上次修改：{band}", { band: bandLabel(f.tone[row]!) });
    if (kind === Kind.SMALL) return t("距上次修改：{band}（占空间最多的一档）", { band: bandLabel(f.tone[row]!) });
    return null;
  }
  if (mode === "class" && kind === Kind.SMALL && vocab) {
    const cls = vocab.classes[f.cls[row]!];
    return cls ? t("大部分是「{cls}」", { cls: cls.name }) : t("多种类别混合，没有一类达到一半");
  }
  return null;
}

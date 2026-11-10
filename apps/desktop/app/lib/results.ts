// The pure parts of the 「匹配结果」 sidebar (shell/Results in the Electron renderer): the virtual
// list's paging, what a result row says, and the filter expression as condition capsules.

import type { FilterNormal, Range, ResultRow } from "@/protocol/generated";
import { fmtInt, fmtSize } from "@/lib/format";
import { attrLabel } from "@/lib/filterText";
import { t } from "@/lib/i18n";

/** Height of a result row (px), rows per page of view.results, rows rendered beyond the visible ones. */
export const ROW_H = 50;
export const PAGE = 100;
export const OVERSCAN = 6;

export type Sort = "size" | "name" | "count";
export const SORTS: readonly Sort[] = ["size", "name", "count"];
// labels are looked up when shown: the interface language can change at any time
export const sortLabel = (k: Sort) => ({ size: t("按大小"), name: t("按名称"), count: t("按项数") })[k];
export const tagColour = (c: string) => (({ red: t("红色"), yellow: t("黄色"), green: t("绿色"), blue: t("蓝色") }) as Record<string, string>)[c] ?? c;
export const tagText = (c: string) => (({ red: t("红色标签"), yellow: t("黄色标签"), green: t("绿色标签"), blue: t("蓝色标签") }) as Record<string, string>)[c] ?? c;

export interface ListRange {
  first: number;
  count: number;
}

/** The visible range of a list scrolled to `scrollTop` with `height` px showing. */
export const rangeAt = (scrollTop: number, height: number): ListRange => ({ first: Math.floor(scrollTop / ROW_H), count: Math.ceil(height / ROW_H) + 1 });

/** The pages under the visible range and its overscan. */
export function pagesFor(range: ListRange): number[] {
  const firstPage = Math.floor(Math.max(0, range.first - OVERSCAN) / PAGE);
  const lastPage = Math.floor((range.first + range.count + OVERSCAN) / PAGE);
  const need: number[] = [];
  for (let p = firstPage; p <= lastPage; p++) need.push(p);
  return need;
}

/** The rows rendered for the range (with overscan), within `total`: [first, last). */
export const renderedRows = (range: ListRange, total: number): [number, number] => [Math.max(0, range.first - OVERSCAN), Math.min(total, range.first + range.count + OVERSCAN)];

/** Every needed page is there (or lies past the end): the list can show the new rows. */
export const pagesReady = (need: readonly number[], pages: ReadonlyMap<number, unknown>, total: number) => need.every((q) => pages.has(q) || q * PAGE >= total);

/** The row at index `i` of the paged list. */
export const rowAt = (pages: ReadonlyMap<number, ResultRow[]> | undefined, i: number): ResultRow | undefined => pages?.get(Math.floor(i / PAGE))?.[i % PAGE];

/** Where row `k` scrolls to so it is in view: the new scrollTop, or null when it is in view. */
export function scrollToRow(k: number, scrollTop: number, height: number): number | null {
  if (k * ROW_H < scrollTop) return k * ROW_H;
  if ((k + 1) * ROW_H > scrollTop + height) return (k + 1) * ROW_H - height;
  return null;
}

/** File extension → class id (the first class that names it). */
export function extClasses(classes: readonly { id: string; extensions: readonly string[] }[] | undefined): Map<string, string> {
  const m = new Map<string, string>();
  for (const c of classes ?? []) for (const e of c.extensions) if (!m.has(e)) m.set(e, c.id);
  return m;
}

/** What one result row shows and says (UI-W3-31): name, path, class, tag, the spoken label. */
export function describeRow(r: ResultRow, rootLabel: string, extClass: ReadonlyMap<string, string>) {
  const ext = r.kind === "file" ? (r.name.includes(".") ? r.name.slice(r.name.lastIndexOf(".") + 1).toLowerCase() : "") : "";
  const cls = r.kind === "file" ? (extClass.get(ext) ?? "other") : null;
  const name =
    r.kind === "group" ? (r.name ? r.name : t("{n} 个小项", { n: fmtInt(r.count) })) : r.kind === "rest" ? t("其余 {n} 项", { n: fmtInt(r.count) }) : r.name.replace(/[\\/]+$/, "") || rootLabel;
  const path = r.path || rootLabel;
  const tag = r.tag ? (r.tag_inherited ? t("{tag}（继承自文件夹）", { tag: tagText(r.tag) }) : tagText(r.tag)) : "";
  const spoken = [name, fmtSize(r.alloc)];
  if (r.kind !== "file") spoken.push(t("{n} 项", { n: fmtInt(r.count) }));
  if (tag) spoken.push(tag);
  spoken.push(t("位于 {path}", { path }));
  return { cls, name, path, tag, spoken: spoken.join(t("，")) };
}

// ------------------------------------------------------------------ conditions (UI-W3-30)

export const pendingLabel = (category: string): string =>
  (({ name: t("名称"), class: t("类别"), folder: t("文件夹"), tag: t("标签"), size: t("大小"), age: t("时间"), attr: t("属性") }) as Record<string, string>)[category] ?? category;

/** 「7 天」: a number with its time unit. */
export function timeSpan(n: string, unit: string): string {
  const fields = { n };
  if (unit === "s") return t("{n} 秒", fields);
  if (unit === "min") return t("{n} 分钟", fields);
  if (unit === "h") return t("{n} 小时", fields);
  if (unit === "d") return t("{n} 天", fields);
  if (unit === "w") return t("{n} 周", fields);
  if (unit === "month") return t("{n} 个月", fields);
  if (unit === "y") return t("{n} 年", fields);
  return `${n} ${unit}`;
}

const ageLabel = (field: string): string => (({ modify: t("修改时间"), create: t("创建时间"), access: t("访问时间") }) as Record<string, string>)[field] ?? t("{field}时间", { field });

export interface Capsule {
  label: string;
  values: { text: string; cls: string }[];
  joiner: string;
}

/** The parsed expression as condition capsules: 「名称 *.mp4 或 *.mkv」「大小 > 1 GB 且 …」. */
export function capsules(n: FilterNormal, className: (id: string) => string): Capsule[] {
  const out: Capsule[] = [];
  const or = t("或");
  const and = t("且");
  const list = (label: string, items: string[], cls: string, joiner = or) => {
    if (items.length) out.push({ label, joiner, values: items.map((text) => ({ text, cls })) });
  };
  const not = (a: string) => t("非{attr}", { attr: attrLabel(a) });
  list(t("名称"), n.masks, "t-mask");
  list(t("排除名称"), n.not_masks, "t-mask");
  list(
    t("文件夹"),
    n.folders.map((f) => `\\${f}`),
    "t-folder",
  );
  list(
    t("排除文件夹"),
    n.not_folders.map((f) => `\\${f}`),
    "t-folder",
  );
  list(t("类别"), n.classes.map(className), "t-value");
  list(t("排除类别"), n.not_classes.map(className), "t-value");
  for (const r of n.sizes) out.push({ label: r.field === "logical" ? t("逻辑大小") : t("大小"), joiner: and, values: sizeRange(r) });
  for (const r of n.ages) out.push({ label: ageLabel(r.field), joiner: and, values: ageRange(r) });
  if (n.tags !== null) list(t("标签"), n.tags.length === 4 ? [t("任意")] : n.tags.map(tagColour), "t-keyword");
  list(t("排除标签"), n.not_tags.map(tagColour), "t-keyword");
  if (n.attrs) list(t("属性"), [...n.attrs.required.map(attrLabel), ...n.attrs.forbidden.map(not)], "t-value", and);
  for (const a of n.not_attrs) list(t("排除属性"), [...a.required.map(attrLabel), ...a.forbidden.map(not)], "t-value", and);
  return out;
}

function sizeRange(r: Range) {
  const side = (b: NonNullable<Range["lower"]>) => ({ text: `${b.op} ${b.number} ${b.unit === "b" ? "B" : b.unit.toUpperCase()}`, cls: "t-compare" });
  return [r.lower && side(r.lower), r.upper && side(r.upper)].filter((x): x is { text: string; cls: string } => !!x);
}

function ageRange(r: Range) {
  const out: { text: string; cls: string }[] = [];
  if (r.upper) {
    const fields = { span: timeSpan(r.upper.number, r.upper.unit) };
    out.push({ text: r.upper.op === "<=" ? t("{span}以内（含）", fields) : t("{span}以内", fields), cls: "t-compare" });
  }
  if (r.lower) {
    const fields = { span: timeSpan(r.lower.number, r.lower.unit) };
    out.push({ text: r.lower.op === ">=" ? t("早于 {span}前（含）", fields) : t("早于 {span}前", fields), cls: "t-compare" });
  }
  return out;
}

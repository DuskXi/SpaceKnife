// Report cells as text (docs/plugins.md 「报告文档」): every unit formatted the way the app's own
// panels do (lib/format.ts), sorting by a column, and the limits a document is cut to. Pure: tested
// under node (report.test.ts).

import type { Report, TableBlock } from "@/protocol/generated";
import { fmtAgo, fmtDateTime, fmtDuration, fmtInt, fmtSize, middlePath } from "@/lib/format";
import { t } from "@/lib/i18n";

export type Unit = TableBlock["columns"][number]["unit"];
export type Cell = string | number | boolean | null;

/** One value in its unit: size in bytes, count, pct (0–1), date (unix seconds), ago, duration
 * (seconds), bytes_per_s, text, path. A missing value is "—". */
export function formatCell(value: Cell | undefined, unit: Unit): string {
  if (value === null || value === undefined || value === "") return "—";
  if (typeof value === "boolean") return value ? t("是") : t("否");
  const n = typeof value === "number" ? value : Number(value);
  const numeric = typeof value === "number" || (value !== "" && Number.isFinite(n));
  switch (unit) {
    case "size":
      return numeric ? fmtSize(n) : String(value);
    case "count":
      return numeric ? fmtInt(Math.round(n)) : String(value);
    case "pct":
      return numeric ? (Math.abs(n) < 0.1 ? `${(n * 100).toFixed(1)}%` : `${Math.round(n * 100)}%`) : String(value);
    case "date":
      return numeric ? fmtDateTime(n) : String(value);
    case "ago":
      return numeric ? fmtAgo(n) : String(value);
    case "duration":
      return numeric ? fmtDuration(n) : String(value);
    case "bytes_per_s":
      return numeric ? `${fmtSize(n)}/s` : String(value);
    case "path":
      return middlePath(String(value), 72);
    default:
      return String(value);
  }
}

/** Units drawn right-aligned in a monospace column. */
export const numericUnit = (unit: Unit): boolean => unit !== "text" && unit !== "path";

/** Sort key of a cell: numbers as numbers, the rest as text; missing values last. */
function sortValue(v: Cell | undefined): [number, number | string] {
  if (v === null || v === undefined || v === "") return [1, 0];
  if (typeof v === "number") return [0, v];
  if (typeof v === "boolean") return [0, v ? 1 : 0];
  return [0, v.toLowerCase()];
}

/** Rows sorted by `key` ("-key" descending); a stable sort, missing values always last. */
export function sortRows<R extends { cells: Record<string, Cell> }>(rows: readonly R[], sort: string | null): R[] {
  if (!sort) return [...rows];
  const desc = sort.startsWith("-");
  const key = desc ? sort.slice(1) : sort;
  return rows
    .map((r, i) => ({ r, i, k: sortValue(r.cells[key]) }))
    .sort((a, b) => {
      if (a.k[0] !== b.k[0]) return a.k[0] - b.k[0];
      const x = a.k[1];
      const y = b.k[1];
      const c = typeof x === "number" && typeof y === "number" ? x - y : String(x).localeCompare(String(y));
      return c === 0 ? a.i - b.i : desc ? -c : c;
    })
    .map((x) => x.r);
}

/** The next sort of a column header click: ascending, descending, back to the document's order. */
export function nextSort(current: string | null, key: string, numeric: boolean): string | null {
  // numbers start with the largest, text with A
  const first = numeric ? `-${key}` : key;
  const second = numeric ? key : `-${key}`;
  if (current === first) return second;
  if (current === second) return null;
  return first;
}

export const PAGE_MAX = 50;

/** Rows `page` (from 0) of `size` (at most PAGE_MAX), and the number of pages. */
export function pageOf<T>(rows: readonly T[], page: number, size: number): { rows: T[]; pages: number; page: number } {
  const per = Math.max(1, Math.min(PAGE_MAX, size));
  const pages = Math.max(1, Math.ceil(rows.length / per));
  const at = Math.min(Math.max(0, page), pages - 1);
  return { rows: rows.slice(at * per, at * per + per), pages, page: at };
}

/** A share 0–1 of `value` against `max` (bars, meters), clamped. */
export const share = (value: number, max: number): number => (max > 0 && Number.isFinite(value) ? Math.max(0, Math.min(1, value / max)) : 0);

/** Whether a document has anything to show (else its `empty` text, or a default). */
export const isEmptyReport = (r: Pick<Report, "blocks">): boolean => r.blocks.length === 0;

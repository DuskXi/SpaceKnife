// Number, size and time formatting shared by every screen (UI-MAIN-59, UI-MAIN-60, G13).

import { byLang, t } from "./i18n";

/** English takes the singular for 1 ("1 item"), Chinese has one form: `text` is the translated
 * message, `one` its English singular with the same placeholders, filled from `fields`. */
export function plural(n: number, text: string, one: string, fields: Record<string, string | number> = {}): string {
  if (n !== 1) return text;
  return byLang(text, one.replace(/\{(\w+)\}/g, (m, k: string) => (k in fields ? String(fields[k]) : m)));
}

/** Size unit base: Windows and Linux 1024, macOS 1000 (G13); settings may override. */
let unitBase: 1000 | 1024 = typeof window !== "undefined" && window.sk?.platform === "darwin" ? 1000 : 1024;

export function setUnitBase(base: 1000 | 1024): void {
  unitBase = base;
}

export const getUnitBase = (): 1000 | 1024 => unitBase;

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

/** Split a size into number and unit: 63.6 / "GB". Whole numbers stay whole when `whole` (961 GB). */
export function sizeParts(bytes: number | bigint, whole = false): [string, string] {
  let v = Number(bytes);
  let i = 0;
  while (Math.abs(v) >= unitBase && i < UNITS.length - 1) {
    v /= unitBase;
    i++;
  }
  if (i === 0) return [String(Math.round(v)), "B"];
  // the unit is final only after rounding: 1048575 B is "1.0 MB", not "1024.0 KB"
  if (Math.abs(Number(v.toFixed(1))) >= unitBase && i < UNITS.length - 1) {
    v /= unitBase;
    i++;
  }
  const text = whole && Math.abs(v - Math.round(v)) < 0.05 ? String(Math.round(v)) : v.toFixed(1);
  return [text, UNITS[i]!];
}

export function fmtSize(bytes: number | bigint, whole = false): string {
  const [n, u] = sizeParts(bytes, whole);
  return `${n} ${u}`;
}

/** Size in the unit chosen for `ref` (UI-W1-17: used, free and capacity of one card share a unit). */
export function fmtSizeAs(bytes: number, ref: number): string {
  let i = 0;
  let r = Math.abs(ref);
  while (r >= unitBase && i < UNITS.length - 1) {
    r /= unitBase;
    i++;
  }
  if (i === 0) return `${Math.round(bytes)} B`;
  const v = bytes / unitBase ** i;
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(v >= 10 ? 1 : 2)} ${UNITS[i]}`;
}

/** Signed size with a true minus sign: "+14.8 GB", "−5.7 GB". */
export function fmtDelta(bytes: number): string {
  const [n, u] = sizeParts(Math.abs(bytes));
  return `${bytes < 0 ? "−" : "+"}${n} ${u}`;
}

export function fmtInt(n: number | bigint): string {
  return Number(n).toLocaleString("en-US");
}

/** A count for tight columns: below 10,000 in full, then short (250,752 → 25.1万 / 250.8K). */
export function fmtCount(n: number): string {
  if (Math.abs(n) < 10_000) return fmtInt(n);
  const locale = byLang("zh-CN", "en-US");
  return new Intl.NumberFormat(locale, { notation: "compact", maximumFractionDigits: 1 }).format(n);
}

/** Share: < 10 % with one decimal (8.2%), otherwise whole (54%). */
export function fmtShare(part: number, whole: number): string {
  if (!whole) return "0%";
  const pct = (part / whole) * 100;
  return Math.round(pct * 10) / 10 < 10 ? `${pct.toFixed(1)}%` : `${Math.round(pct)}%`;
}

/** List percentages are always whole (96%, 4%). */
export function fmtPct(part: number, whole: number): string {
  return whole ? `${Math.round((part / whole) * 100)}%` : "0%";
}

export function fmtDuration(seconds: number): string {
  if (Number(seconds.toFixed(1)) < 60) return t("{s} 秒", { s: seconds.toFixed(1) });
  const total = Math.round(seconds); // round first, then split: 119.6 s is 2 分 0 秒
  const m = Math.floor(total / 60);
  return m < 60 ? t("{m} 分 {s} 秒", { m, s: total % 60 }) : t("{h} 小时 {m} 分", { h: Math.floor(m / 60), m: m % 60 });
}

const pad = (n: number) => String(n).padStart(2, "0");

/** YYYY-MM-DD HH:mm in local time; "—" for unknown (0). */
export function fmtDateTime(unixSeconds: number): string {
  if (!unixSeconds) return "—";
  const d = new Date(unixSeconds * 1000);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** "2 小时前", "3 天前", "2 个月前"; "刚刚" under a minute; "未知" for 0. */
export function fmtAgo(unixSeconds: number, now = Date.now() / 1000): string {
  if (!unixSeconds) return t("未知");
  const s = Math.max(0, now - unixSeconds);
  if (s < 60) return t("刚刚");
  const [min, hour, day] = [Math.floor(s / 60), Math.floor(s / 3600), Math.floor(s / 86400)];
  if (s < 3600) return plural(min, t("{n} 分钟前", { n: min }), "1 minute ago");
  if (s < 86400) return plural(hour, t("{n} 小时前", { n: hour }), "1 hour ago");
  if (s < 30 * 86400) return plural(day, t("{n} 天前", { n: day }), "1 day ago");
  const month = Math.floor(s / (30 * 86400));
  if (s < 365 * 86400) return plural(month, t("{n} 个月前", { n: month }), "1 month ago");
  const year = Math.floor(s / (365 * 86400));
  return plural(year, t("{n} 年前", { n: year }), "1 year ago");
}

// ------------------------------------------------------------------ keys and paths

const MAC_KEYS: Record<string, string> = { Ctrl: "⌘", Alt: "⌥", Shift: "⇧" };
const ARROWS: Record<string, string> = { Left: "←", Right: "→", Up: "↑", Down: "↓" };

/** Neutral key notation -> platform notation (UI-W4-22): Ctrl+Shift+F5, or ⌘⇧F5 on macOS. */
export function fmtKeys(keys: string): string {
  const parts = keys.split("+").map((k) => ARROWS[k] ?? k);
  if (window.sk.platform === "darwin") return parts.map((k) => MAC_KEYS[k] ?? k).join("");
  return parts.join("+");
}

/** Long parent paths keep the root and the last two folders (W4 待定 12): C:\…\wsl\disk. */
export function middlePath(path: string, max = 52): string {
  if (path.length <= max) return path;
  const sep = path.includes("\\") ? "\\" : "/";
  const parts = path.split(sep);
  // the root: a drive (C:), "/x" on POSIX, or the server and share of a UNC path
  const rootLen = path.startsWith("\\\\") ? 4 : parts[0] === "" ? 2 : 1;
  const rest = parts.slice(rootLen);
  if (rest.length <= 3) return path;
  return [parts.slice(0, rootLen).join(sep), "…", ...rest.slice(-2)].join(sep);
}

/** "2.5 MB", "1mb", "300 KiB", "1048576" → bytes (KB, MB … in the app's unit base; KiB … 1024); null when it is not a size. */
export function parseSizeText(text: string, base: 1000 | 1024 = getUnitBase()): number | null {
  const m = /^\s*(\d+(?:[.,]\d+)?)\s*([kmgtp]?)(i?)(b?)\s*$/i.exec(text);
  if (!m) return null;
  const n = Number(m[1]!.replace(",", "."));
  const power = " kmgtp".indexOf((m[2] || " ").toLowerCase());
  const unit = (m[3] ? 1024 : base) ** Math.max(0, power);
  const v = Math.round(n * unit);
  return Number.isFinite(v) ? v : null;
}

// Interface languages (M7.5): Simplified Chinese (the source text) and English.
//
// The Chinese text in the code is the message id, gettext style: t("开始扫描") returns it as is in
// Chinese and looks it up in the English catalog otherwise. The catalog is the JSON files under
// locales/en/ (one per area, merged): {"中文原文": "English"}. Placeholders are {name} and stay
// the same in both languages: t("已导出 {n} 项", { n: 3 }).
//
// The language comes from the backend's settings (state.language: 「界面语言」 resolved, 跟随系统
// included); until it answers, the browser's. Components re-render through the store's `lang`.

export type Lang = "zh-CN" | "en";

const files = import.meta.glob<Record<string, string>>("../locales/en/*.json", { eager: true, import: "default" });
const EN: Record<string, string> = Object.assign({}, ...Object.keys(files).sort().map((k) => files[k]));

function guess(): Lang {
  const tag = typeof navigator !== "undefined" ? navigator.language : "zh-CN";
  return tag.toLowerCase().startsWith("zh") ? "zh-CN" : "en";
}

let lang: Lang = guess();

export const getLang = (): Lang => lang;

export function setLang(value: Lang): void {
  lang = value === "en" ? "en" : "zh-CN";
}

/** The text in the current language, placeholders filled in. */
export function t(zh: string, fields?: Record<string, string | number>): string {
  const text = lang === "en" ? (EN[zh] ?? zh) : zh;
  if (!fields) return text;
  return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in fields ? String(fields[k]) : m));
}

/** Pick per language when the two texts differ in structure, not just words. */
export const byLang = <T,>(zh: T, en: T): T => (lang === "en" ? en : zh);

/** For tests and the checker: the merged English catalog. */
export const englishCatalog = (): Readonly<Record<string, string>> => EN;

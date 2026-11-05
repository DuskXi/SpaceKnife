// The user documentation, bundled at build time: the Markdown pages the website builds
// (site/src/content/docs, written by docs/docs-writing.md) and its table of contents
// (site/src/docs-nav.json). English pages are docs/<id>.md, Chinese ones zh/docs/<id>.md.

import { type Heading, outline } from "./markdown";
import type { DocLang } from "./site";

export { type DocLang, OVERVIEW, SITE, siteUrl } from "./site";

export interface DocPage {
  id: string;
  lang: DocLang;
  title: string;
  description: string;
  /** the Markdown after the frontmatter */
  body: string;
}

export interface NavGroup {
  id: string;
  label: Record<DocLang, string>;
  items: string[];
}

interface NavFile {
  groups: NavGroup[];
}

const EN_FILES = import.meta.glob<string>("@site/content/docs/docs/*.md", { query: "?raw", import: "default", eager: true });
const ZH_FILES = import.meta.glob<string>("@site/content/docs/zh/docs/*.md", { query: "?raw", import: "default", eager: true });
const NAV_FILE = import.meta.glob<NavFile>("@site/docs-nav.json", { import: "default", eager: true });

/** The table of contents, in order. */
export const NAV: readonly NavGroup[] = Object.values(NAV_FILE)[0]?.groups ?? [];

/** Every page id of the table of contents, in reading order (previous / next). */
export const ORDER: readonly string[] = NAV.flatMap((g) => g.items);

/** A page of the table of contents (the help shows nothing else). */
export const isPage = (id: string): boolean => ORDER.includes(id);

/** The id of a page from its file name: ".../docs/filtering.md" is "filtering". */
const idOf = (path: string) => path.replace(/^.*[\\/]/, "").replace(/\.md$/, "");

function load(files: Record<string, string>, lang: DocLang): Map<string, DocPage> {
  const out = new Map<string, DocPage>();
  for (const [path, raw] of Object.entries(files)) {
    const id = idOf(path);
    const { data, body } = frontmatter(raw);
    out.set(id, { id, lang, title: data.title || id, description: data.description ?? "", body });
  }
  return out;
}

export const PAGES: Readonly<Record<DocLang, ReadonlyMap<string, DocPage>>> = {
  en: load(EN_FILES, "en"),
  zh: load(ZH_FILES, "zh"),
};

/** The page in this language; the other language's when it has none (a page still being written). */
export function page(lang: DocLang, id: string): DocPage | undefined {
  return PAGES[lang].get(id) ?? PAGES[lang === "zh" ? "en" : "zh"].get(id);
}

/** The group of the table of contents a page is listed in. */
export const groupOf = (id: string): NavGroup | undefined => NAV.find((g) => g.items.includes(id));

/** The language of the help pages for an interface language. */
export const docLang = (lang: string): DocLang => (lang === "en" ? "en" : "zh");

/** The page after (dir 1) or before (dir -1) this one in reading order. */
export function neighbour(id: string, dir: 1 | -1): string | null {
  const at = ORDER.indexOf(id);
  return at < 0 ? null : (ORDER[at + dir] ?? null);
}

// ------------------------------------------------------------------ frontmatter

/** The frontmatter's top-level `key: value` lines (title, description) and the Markdown after it.
 * Values may be quoted as in YAML; nested keys and lists are not needed here and are skipped. */
export function frontmatter(src: string): { data: Record<string, string>; body: string } {
  const m = /^﻿?---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/.exec(src);
  if (!m) return { data: {}, body: src.replace(/^﻿/, "") };
  const data: Record<string, string> = {};
  for (const line of m[1]!.split(/\r?\n/)) {
    const kv = /^([A-Za-z_][\w-]*):[ \t]*(.*)$/.exec(line);
    if (kv) data[kv[1]!] = scalar(kv[2]!.trim());
  }
  return { data, body: src.slice(m[0].length) };
}

function scalar(value: string): string {
  if (value.length >= 2 && value.startsWith('"') && value.endsWith('"')) {
    try {
      return JSON.parse(value) as string; // YAML's double-quoted escapes are JSON's, near enough
    } catch {
      return value.slice(1, -1);
    }
  }
  if (value.length >= 2 && value.startsWith("'") && value.endsWith("'")) return value.slice(1, -1).replaceAll("''", "'");
  return value.replace(/\s+#.*$/, ""); // a plain scalar ends before a comment
}

// ------------------------------------------------------------------ search

export interface SearchHit {
  page: DocPage;
  /** headings of the page that match, in page order */
  headings: Heading[];
  score: number;
}

const outlines = new Map<string, Heading[]>();

/** The headings of a page (cached: the pages never change while the app runs). */
export function headingsOf(p: DocPage): Heading[] {
  const key = `${p.lang}/${p.id}`;
  let hit = outlines.get(key);
  if (!hit) outlines.set(key, (hit = outline(p.body, p.lang, p.id)));
  return hit;
}

/** Pages whose title, description or headings contain every word of the query (any case), best
 * first: a word in the title counts most, then in a heading, then in the description. */
export function search(lang: DocLang, query: string): SearchHit[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return [];
  const hits: SearchHit[] = [];
  ORDER.forEach((id, order) => {
    const p = page(lang, id);
    if (!p) return;
    const title = p.title.toLowerCase();
    const description = p.description.toLowerCase();
    const heads = headingsOf(p);
    const lowered = heads.map((h) => h.text.toLowerCase());
    let score = 0;
    for (const w of words) {
      if (title.includes(w)) score += 100;
      else if (lowered.some((h) => h.includes(w))) score += 10;
      else if (description.includes(w)) score += 5;
      else return;
    }
    const headings = heads.filter((_h, i) => words.some((w) => lowered[i]!.includes(w)));
    hits.push({ page: p, headings, score: score - order / 1000 }); // ties keep the reading order
  });
  return hits.sort((a, b) => b.score - a.score);
}

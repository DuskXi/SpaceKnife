// Markdown of the help pages → HTML, the subset docs/docs-writing.md allows: standard Markdown with
// tables, the asides :::note / :::tip / :::caution / :::danger (with an optional [title]), <kbd>…</kbd>
// as the only HTML, and HTML comments (the generated-section markers), which vanish. Raw HTML is off:
// anything else in angle brackets shows as text, so the output holds only the elements made here.
//
// Heading ids follow the website (Starlight: github-slugger over the heading's text, one slugger per
// page), so `../filtering/#大小` lands on the same heading in both.

import { icons, iconStrokeWidth, iconViewBox } from "@design/icons";
import GithubSlugger from "github-slugger";
import MarkdownIt from "markdown-it";
import type { Renderer, RendererRule, StateBlock, StateCore, StateInline, Token } from "markdown-it";

import { type DocLang, type LinkTarget, resolveLink } from "./site";

export interface Heading {
  /** 2 for ##, 3 for ###, … */
  depth: number;
  id: string;
  text: string;
}

export interface DocLink {
  /** as written in the page */
  href: string;
  target: LinkTarget;
}

export interface Rendered {
  html: string;
  headings: Heading[];
  links: DocLink[];
}

export const ASIDES = ["note", "tip", "caution", "danger"] as const;
export type Aside = (typeof ASIDES)[number];

/** An aside's title when it names none: the website's (Starlight's) words, in the page's language
 * (not the interface's: these belong to the page, like its text). */
export const ASIDE_TITLES: Record<DocLang, Record<Aside, string>> = {
  en: { note: "Note", tip: "Tip", caution: "Caution", danger: "Danger" },
  zh: { note: "注意", tip: "提示", caution: "警告", danger: "危险" }, // i18n-ignore
};
const ASIDE_ICONS: Record<Aside, keyof typeof icons> = { note: "info", tip: "bolt", caution: "warning", danger: "warning" };

interface Env {
  lang: DocLang;
  page: string;
  copyLabel: string;
  slugger: GithubSlugger;
  headings: Heading[];
  links: DocLink[];
}

const md = new MarkdownIt({ html: false, linkify: true, typographer: false });
// bare https:// addresses become links (as GitHub-flavoured Markdown does), "example.com" does not
md.linkify.set({ fuzzyLink: false, fuzzyEmail: false });
md.block.ruler.before("fence", "sk_aside", aside, { alt: ["paragraph", "reference", "blockquote", "list"] });
md.block.ruler.before("html_block", "sk_comment", commentBlock, { alt: ["paragraph", "reference", "blockquote"] });
md.inline.ruler.before("html_inline", "sk_html", inlineHtml);
md.core.ruler.push("sk_kbd", balanceKbd);
md.core.ruler.push("sk_headings", headingIds);
md.core.ruler.push("sk_links", links);
renderRules(md.renderer);

/** Render a page's Markdown (the body, after the frontmatter). */
export function render(body: string, lang: DocLang, page: string, copyLabel = "Copy"): Rendered {
  const env = newEnv(lang, page, copyLabel);
  const html = md.render(body, env as unknown as Record<string, unknown>);
  return { html, headings: env.headings, links: env.links };
}

/** The headings of a page without rendering it (search, the page's contents). */
export function outline(body: string, lang: DocLang = "en", page = "index"): Heading[] {
  const env = newEnv(lang, page, "");
  md.parse(body, env as unknown as Record<string, unknown>);
  return env.headings;
}

function newEnv(lang: DocLang, page: string, copyLabel: string): Env {
  return { lang, page, copyLabel, slugger: new GithubSlugger(), headings: [], links: [] };
}

// ------------------------------------------------------------------ block rules

const ASIDE_OPEN = /^(:{3,})(note|tip|caution|danger)(?:\[(.*)\])?[ \t]*$/;
const ASIDE_CLOSE = /^(:{3,})[ \t]*$/;

/** `:::tip[Title]` … `:::` (a fence of more colons closes only with as many: nested asides). */
function aside(state: StateBlock, startLine: number, endLine: number, silent: boolean): boolean {
  if (state.sCount[startLine]! - state.blkIndent >= 4) return false;
  const start = state.bMarks[startLine]! + state.tShift[startLine]!;
  if (state.src.charCodeAt(start) !== 0x3a /* : */) return false;
  const open = ASIDE_OPEN.exec(state.src.slice(start, state.eMarks[startLine]));
  if (!open) return false;
  if (silent) return true;
  const fence = open[1]!.length;
  let next = startLine;
  let closed = false;
  while (++next < endLine) {
    const s = state.bMarks[next]! + state.tShift[next]!;
    const e = state.eMarks[next]!;
    if (s < e && state.sCount[next]! < state.blkIndent) break; // the list item or quote around it ended
    if (state.sCount[next]! - state.blkIndent >= 4) continue;
    const close = ASIDE_CLOSE.exec(state.src.slice(s, e));
    if (close && close[1]!.length >= fence) {
      closed = true;
      break;
    }
  }
  const env = state.env as unknown as Env;
  const kind = open[2] as Aside;
  const title = open[3]?.trim() || ASIDE_TITLES[env.lang][kind];
  const parent = state.parentType;
  const lineMax = state.lineMax;
  state.parentType = "aside" as typeof state.parentType;
  state.lineMax = next;
  const token = state.push("aside_open", "aside", 1);
  token.block = true;
  token.info = kind;
  token.markup = open[1]!;
  token.map = [startLine, next];
  token.meta = { title };
  state.push("aside_title_open", "p", 1);
  const label = state.push("inline", "", 0);
  label.content = title;
  label.map = [startLine, startLine + 1];
  label.children = [];
  state.push("aside_title_close", "p", -1);
  state.push("aside_body_open", "div", 1);
  state.md.block.tokenize(state, startLine + 1, next);
  state.push("aside_body_close", "div", -1);
  state.push("aside_close", "aside", -1);
  state.parentType = parent;
  state.lineMax = lineMax;
  state.line = next + (closed ? 1 : 0);
  return true;
}

/** An HTML comment on its own lines vanishes, with the rest of the line that ends it (CommonMark). */
function commentBlock(state: StateBlock, startLine: number, endLine: number, silent: boolean): boolean {
  if (state.sCount[startLine]! - state.blkIndent >= 4) return false;
  const start = state.bMarks[startLine]! + state.tShift[startLine]!;
  if (!state.src.startsWith("<!--", start)) return false;
  if (silent) return true;
  const end = state.src.indexOf("-->", start + 4);
  let line = startLine;
  if (end < 0) line = endLine - 1;
  else while (line < endLine - 1 && state.eMarks[line]! < end + 3) line++;
  state.line = line + 1;
  return true;
}

// ------------------------------------------------------------------ inline rules

const KBD = /^<(\/?)kbd[ \t]*>/i;

/** `<kbd>` / `</kbd>` become key caps; an inline `<!-- … -->` vanishes; other tags stay text. */
function inlineHtml(state: StateInline, silent: boolean): boolean {
  const pos = state.pos;
  if (state.src.charCodeAt(pos) !== 0x3c /* < */) return false;
  if (state.src.startsWith("<!--", pos)) {
    const end = state.src.indexOf("-->", pos + 4);
    if (end < 0 || end + 3 > state.posMax) return false;
    state.pos = end + 3;
    return true;
  }
  const m = KBD.exec(state.src.slice(pos, Math.min(state.posMax, pos + 16)));
  if (!m) return false;
  if (!silent) {
    // nesting 0: the key cap doesn't open a level (unpaired tags are made text again, balanceKbd)
    const token = state.push(m[1] ? "kbd_close" : "kbd_open", "kbd", 0);
    token.markup = m[0];
  }
  state.pos += m[0].length;
  return true;
}

// ------------------------------------------------------------------ core rules

function eachInline(state: StateCore, fn: (children: Token[]) => void): void {
  for (const token of state.tokens) if (token.type === "inline" && token.children) fn(token.children);
}

/** A `<kbd>` without its `</kbd>` (or the other way round) is shown as written. */
function balanceKbd(state: StateCore): void {
  eachInline(state, (children) => {
    const open: Token[] = [];
    for (const c of children) {
      if (c.type === "kbd_open") open.push(c);
      else if (c.type === "kbd_close" && !open.pop()) asText(c);
    }
    for (const c of open) asText(c);
  });
}

function asText(token: Token): void {
  token.type = "text";
  token.tag = "";
  token.content = token.markup;
}

/** The heading's text as the website reads it: what is shown, without markup. */
function plainText(children: Token[]): string {
  let out = "";
  for (const c of children) {
    if (c.type === "text" || c.type === "code_inline" || c.type === "text_special") out += c.content;
    else if (c.type === "softbreak" || c.type === "hardbreak") out += "\n";
    else if (c.children) out += plainText(c.children);
  }
  return out;
}

function headingIds(state: StateCore): void {
  const env = state.env as unknown as Env;
  const tokens = state.tokens;
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i]!;
    if (token.type !== "heading_open") continue;
    const text = plainText(tokens[i + 1]?.children ?? []);
    const id = env.slugger.slug(text);
    token.attrSet("id", id);
    env.headings.push({ depth: Number(token.tag.slice(1)), id, text: text.trim() });
  }
}

/** Every link says where it goes: a help page (data-doc, data-anchor) or the browser (data-external). */
function links(state: StateCore): void {
  const env = state.env as unknown as Env;
  eachInline(state, (children) => {
    for (const c of children) {
      if (c.type !== "link_open") continue;
      const href = String(c.attrGet("href") ?? "");
      const target = resolveLink(href, env.lang, env.page);
      env.links.push({ href, target });
      if (target.kind === "doc") {
        c.attrSet("href", target.url);
        c.attrSet("data-doc", target.id);
        if (target.anchor) c.attrSet("data-anchor", target.anchor);
      } else if (target.kind === "external") {
        c.attrSet("href", target.url);
        c.attrSet("data-external", "");
        c.attrSet("title", target.url);
      } else c.attrSet("data-invalid", "");
    }
  });
}

// ------------------------------------------------------------------ rendering

function svg(name: keyof typeof icons): string {
  return `<svg width="16" height="16" viewBox="${iconViewBox}" fill="none" stroke="currentColor" stroke-width="${iconStrokeWidth}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${icons[name]}</svg>`;
}

function renderRules(r: Renderer): void {
  const esc = md.utils.escapeHtml;
  const fence = r.rules.fence!;
  const codeBlock = r.rules.code_block!;
  const withCopy =
    (inner: RendererRule): RendererRule =>
    (tokens, idx, options, env, self) => {
      const label = esc((env as unknown as Env | undefined)?.copyLabel || "Copy");
      const code = inner(tokens, idx, options, env, self);
      return `<div class="help-code">${code}<button type="button" class="help-copy" data-copy aria-label="${label}" title="${label}">${svg("copy")}</button></div>\n`;
    };
  r.rules.fence = withCopy(fence);
  r.rules.code_block = withCopy(codeBlock);
  r.rules.kbd_open = () => '<kbd class="kbd">';
  r.rules.kbd_close = () => "</kbd>";
  r.rules.table_open = () => '<div class="help-table"><table>\n';
  r.rules.table_close = () => "</table></div>\n";
  // no images in the pages yet (docs/docs-writing.md): one shows its description
  r.rules.image = (tokens, idx) => `<span class="help-image">${esc(tokens[idx]!.content)}</span>`;
  r.rules.aside_open = (tokens, idx) => {
    const t = tokens[idx]!;
    const kind = t.info as Aside;
    return `<aside class="help-aside ${kind}" aria-label="${esc(String(t.meta?.title ?? ""))}">`;
  };
  r.rules.aside_title_open = (tokens, idx) => {
    const kind = tokens[idx - 1]!.info as Aside;
    return `<p class="help-aside-title" aria-hidden="true">${svg(ASIDE_ICONS[kind] ?? "info")}<span>`;
  };
  r.rules.aside_title_close = () => "</span></p>\n";
  r.rules.aside_body_open = () => '<div class="help-aside-body">\n';
  r.rules.aside_body_close = () => "</div>\n";
  r.rules.aside_close = () => "</aside>\n";
}

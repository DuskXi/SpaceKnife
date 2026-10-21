// Every page of the documentation, in both languages, as the help page shows it: it renders, holds
// no HTML but key caps, and its links land on a page (and heading) that exists. The pages are the
// website's (site/src/content/docs); docs/docs-writing.md says how they are written.

import { describe, expect, it } from "vitest";

import { headingsOf, NAV, ORDER, PAGES, page, search } from "./docs";
import { render } from "./markdown";
import { type DocLang, siteUrl } from "./site";

const LANGS: DocLang[] = ["en", "zh"];
/** The files in site/src/assets, as paths below it ("visualizations/treemap.webp"). */
const ASSETS = new Set(
  Object.keys(import.meta.glob("@site/assets/**/*")).map((k) => /(?:^|\/)site\/src\/assets\/(.+)$/.exec(k)?.[1] ?? k),
);

/** The elements the renderer makes; anything else would be HTML that came through. */
const ELEMENTS = new Set([
  "p", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "hr", "br", "pre", "code", "em", "strong", "s", "a",
  "kbd", "div", "span", "table", "thead", "tbody", "tr", "th", "td", "aside", "button", "svg", "path", "circle", "rect", "line", "polyline",
]);

/** The output without the code (inline and blocks): what is left was written as prose. */
const prose = (html: string) => html.replace(/<pre>[\s\S]*?<\/pre>/g, "").replace(/<code>[\s\S]*?<\/code>/g, "");

describe("the documentation's table of contents", () => {
  it("lists every page once, and every page has both languages", () => {
    expect(ORDER.length).toBeGreaterThan(0);
    expect(new Set(ORDER).size).toBe(ORDER.length);
    for (const g of NAV) {
      expect(g.label.en, g.id).toBeTruthy();
      expect(g.label.zh, g.id).toBeTruthy();
    }
    for (const lang of LANGS) {
      expect([...PAGES[lang].keys()].filter((id) => !ORDER.includes(id)), `${lang}: pages not in docs-nav.json`).toEqual([]);
      expect(ORDER.filter((id) => !PAGES[lang].has(id)), `${lang}: pages missing`).toEqual([]);
    }
  });
});

describe.each(LANGS)("every %s page", (lang) => {
  it.each(ORDER)("%s renders, with a title, a description and only the allowed Markdown", (id) => {
    const p = PAGES[lang].get(id);
    expect(p, `${lang}/${id}.md`).toBeDefined();
    if (!p) return;
    expect(p.title.trim(), "frontmatter title").not.toBe("");
    expect(p.description.trim(), "frontmatter description").not.toBe("");
    const r = render(p.body, lang, id);
    // headings start at ## (the title is the page's #)
    expect(r.headings.filter((h) => h.depth < 2).map((h) => h.text), "# headings").toEqual([]);
    const tags = [...r.html.matchAll(/<\/?([a-zA-Z][\w-]*)/g)].map((m) => m[1]!.toLowerCase());
    expect([...new Set(tags.filter((t) => !ELEMENTS.has(t)))], "elements").toEqual([]);
    const text = prose(r.html);
    // raw HTML (other than <kbd> and comments) shows as text: none may be left
    expect(text.match(/&lt;[a-zA-Z!/][^\n]{0,40}/g) ?? [], "raw HTML").toEqual([]);
    // an aside that didn't open (a misspelt kind, a missing blank line) is left as ":::" text
    expect(text.match(/(^|>)\s*:::[^\n]{0,30}/gm) ?? [], "asides").toEqual([]);
    // pictures: the help shows their description instead, so each has one; the file is in
    // site/src/assets, found from the page's folder (site/src/content/docs/[zh/]docs) as the website finds it
    const folder = `file:///site/src/content/docs/${lang === "zh" ? "zh/" : ""}docs/`;
    for (const [, alt, src] of p.body.matchAll(/!\[([^\]]*)\]\(([^)\s]+)\)/g)) {
      expect(alt!.trim(), `${src}: description`).not.toBe("");
      const file = new URL(src!, folder).pathname;
      expect(file.startsWith("/site/src/assets/"), `${src}: not in site/src/assets`).toBe(true);
      expect(ASSETS.has(file.slice("/site/src/assets/".length)), `${src}: no such file`).toBe(true);
    }
  });

  it.each(ORDER)("%s links only to pages and headings that exist", (id) => {
    const p = PAGES[lang].get(id);
    if (!p) return;
    const broken: string[] = [];
    for (const link of render(p.body, lang, id).links) {
      const { target } = link;
      const href = readable(link.href); // as written: the renderer percent-encodes addresses
      const relative = !/^[a-z][a-z0-9+.-]*:/i.test(href);
      if (target.kind === "invalid") broken.push(`${href}: not a link the help can open`);
      if (target.kind === "external" && relative) broken.push(`${href}: not a page (relative links end in a slash and stay in the language)`);
      if (target.kind !== "doc") continue;
      const to = PAGES[lang].get(target.id);
      if (!to || !ORDER.includes(target.id)) {
        broken.push(`${href}: no page ${target.id}`);
        continue;
      }
      // the address the website has for it (the overview is /docs/, not /docs/index/)
      if (target.url.replace(/#.*$/, "") !== siteUrl(lang, target.id)) broken.push(`${href}: the website's address is ${siteUrl(lang, target.id)}`);
      if (target.anchor && !headingsOf(to).some((h) => h.id === target.anchor)) broken.push(`${href}: no heading #${target.anchor} on ${target.id}`);
    }
    expect(broken).toEqual([]);
  });
});

describe("the two languages", () => {
  it.each(ORDER)("%s has the same heading levels in the same order", (id) => {
    const en = PAGES.en.get(id);
    const zh = PAGES.zh.get(id);
    if (!en || !zh) return;
    expect(headingsOf(zh).map((h) => h.depth), "zh vs en").toEqual(headingsOf(en).map((h) => h.depth));
  });
});

describe("search", () => {
  it("finds pages by title, description and headings, title hits first", () => {
    for (const lang of LANGS) {
      const first = page(lang, ORDER[0]!)!;
      expect(search(lang, first.title)[0]?.page.id).toBe(first.id);
      expect(search(lang, "")).toEqual([]);
      expect(search(lang, "zzzz-no-such-words-zzzz")).toEqual([]);
      for (const id of ORDER) {
        const p = page(lang, id)!;
        const h = headingsOf(p)[0];
        if (!h) continue;
        const hit = search(lang, h.text).find((x) => x.page.id === id);
        expect(hit?.headings.map((x) => x.id), `${lang}/${id}: ${h.text}`).toContain(h.id);
      }
    }
  });
});

function readable(href: string): string {
  try {
    return decodeURI(href);
  } catch {
    return href;
  }
}

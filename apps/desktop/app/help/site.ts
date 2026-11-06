// Where the help pages live on the website, and what a link written in a page points to. A page
// links to another one the way the website resolves it (docs/docs-writing.md: relative, with the
// trailing slash, `../filtering/#大小`), so the help resolves links against the page's own address.

export type DocLang = "en" | "zh";

export const SITE = "https://spaceknife.kaltsit.dev";

/** The overview: /docs/ on the website, docs/index.md in the sources. */
export const OVERVIEW = "index";

/** The page on the website: /docs/<id>/ (the overview is /docs/), /zh/docs/… in Chinese. */
export function siteUrl(lang: DocLang, id: string): string {
  return `${SITE}${lang === "zh" ? "/zh" : ""}/docs/${id === OVERVIEW ? "" : `${id}/`}`;
}

export type LinkTarget =
  /** a help page of the same language; `url` is its address on the website */
  | { kind: "doc"; id: string; anchor: string | null; url: string }
  /** anything else on the web: opened in the browser */
  | { kind: "external"; url: string }
  | { kind: "invalid" };

const DOC_PATH = /^\/(?:(zh)\/)?docs\/(?:([^/]+)\/)?$/;

/** What `href`, written in page `from`, points to. */
export function resolveLink(href: string, lang: DocLang, from: string): LinkTarget {
  let url: URL;
  try {
    url = new URL(href, siteUrl(lang, from));
  } catch {
    return { kind: "invalid" };
  }
  if (url.origin === SITE) {
    const m = DOC_PATH.exec(url.pathname);
    if (m && (m[1] ? "zh" : "en") === lang && !url.search) {
      const id = m[2] ? decode(m[2]) : OVERVIEW;
      const anchor = url.hash.length > 1 ? decode(url.hash.slice(1)) : null;
      return { kind: "doc", id, anchor, url: url.href };
    }
  }
  if (url.protocol === "https:" || url.protocol === "http:") return { kind: "external", url: url.href };
  return { kind: "invalid" };
}

function decode(part: string): string {
  try {
    return decodeURIComponent(part);
  } catch {
    return part;
  }
}

// The pictures the help pages show: the ones the pages name (site/src/assets), bundled with their size
// at build time by vite-plugins/help-pictures.ts, so an <img> takes its place before it loads.

import PICTURES from "virtual:help-pictures";

import type { DocLang } from "./site";

export interface Picture {
  /** the bundled file's address */
  url: string;
  width: number;
  height: number;
}

/** The picture a page names: `src` as written in the page, found from the page's folder the way the
 * website finds it (en: site/src/content/docs/docs/, zh: …/zh/docs/). Undefined for anything else,
 * such as a web address: the help shows the picture's description then. */
export function picture(src: string, lang: DocLang): Picture | undefined {
  let path: string;
  try {
    path = decodeURI(new URL(src, `file:///site/src/content/docs/${lang === "zh" ? "zh/" : ""}docs/`).pathname);
  } catch {
    return undefined;
  }
  return path.startsWith("/site/src/") ? PICTURES[path.slice("/site/src/".length)] : undefined;
}

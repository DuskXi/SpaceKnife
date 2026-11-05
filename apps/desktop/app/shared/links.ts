// The web pages the app opens in the browser: the project's pages on GitHub (releases, issues, the
// repository) and the website (the help page's links, 「在网站上查看」). Main opens nothing else
// (main/index.ts sk:openExternal); the help page checks the same list before it asks.

/** The project's pages on GitHub. */
export const PROJECT_PAGES = "https://github.com/DuskXi/SpaceKnife/";
/** The website, with the documentation under /docs/ and /zh/docs/. */
export const WEBSITE = "https://spaceknife.kaltsit.dev/";

const REPOSITORY = PROJECT_PAGES.slice(0, -1);

/** The address to open when `url` is one of the project's pages (normalised), else null. */
export function projectPage(url: unknown): string | null {
  if (typeof url !== "string") return null;
  let href: string;
  try {
    href = new URL(url).href; // resolves "..", lowercases the host
  } catch {
    return null;
  }
  if (href.startsWith(PROJECT_PAGES) || href.startsWith(WEBSITE)) return href;
  // the repository itself, as written in links: …/SpaceKnife and …/SpaceKnife#readme
  return href === REPOSITORY || href.startsWith(`${REPOSITORY}#`) ? href : null;
}

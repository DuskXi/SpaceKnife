// The help tab (pages/HelpPage): a singleton like the settings tab, with a page history of its own
// (links, back / forward). F1, the palette's 「帮助」 and the title bar's button open it on the page
// that explains where the user is.

import { isPage, OVERVIEW } from "@/help/docs";
import { t } from "@/lib/i18n";

import { activeTab, type AppState, get, type HelpLoc, newTabId, set, type SettingsPageId, type Tab, writeText } from "./store";

/** The element that scrolls the help page (its position is kept in the history). */
export const HELP_SCROLL_ID = "sk-help-scroll";

/** Settings pages with a page of their own in the documentation; the others are 「设置」's. */
const SETTINGS_TOPICS: Partial<Record<SettingsPageId, string>> = {
  keys: "shortcuts",
  classes: "tags-classes",
  tags: "tags-classes",
  scan: "performance",
};

/** The page that plainly explains where the user is; the overview when none does. An element
 * around the focus may name one (data-help: the filter box names 「过滤」's page). */
export function helpTopic(s: AppState = get(), focus: Element | null = null): string {
  const tab = activeTab(s);
  const named = focus?.closest("[data-help]")?.getAttribute("data-help");
  let id = OVERVIEW;
  if (named) id = named;
  else if (tab.kind === "help") id = s.help.page;
  else if (tab.kind === "settings") id = SETTINGS_TOPICS[s.settingsPage] ?? "settings";
  else if (tab.kind === "start") id = "first-scan";
  else if (tab.kind === "machine") id = "disks";
  else if (tab.compare) id = "snapshots-compare";
  else if (s.basketOpen) id = "cleanup";
  return isPage(id) ? id : OVERVIEW;
}

/** F1, 「帮助」, the title bar's button: the help, on the page for where the user is (in the help
 * already: nothing changes). */
export function openHelp(): void {
  if (activeTab().kind === "help") return;
  openHelpTab(helpTopic(get(), typeof document === "undefined" ? null : document.activeElement));
}

/** Show the help tab (one at most), on `page` when given, else where it was. */
export function openHelpTab(page?: string, anchor: string | null = null): void {
  const s = get();
  const existing = s.tabs.find((x) => x.kind === "help");
  if (existing) set({ active: existing.id });
  else {
    const tab: Tab = { id: newTabId(), kind: "help", viewIds: [], focusedPane: 0, title: t("帮助") };
    set((x) => ({ tabs: [...x.tabs, tab], active: tab.id }));
  }
  if (typeof page === "string") helpGo(page, anchor);
  else if (!isPage(get().help.page)) set({ help: { page: OVERVIEW, anchor: null } });
}

/** Go to a page, and to a heading on it; the place left is recorded in the history. */
export function helpGo(page: string, anchor: string | null = null): void {
  const s = get();
  const to: HelpLoc = { page: isPage(page) ? page : OVERVIEW, anchor };
  const same = s.help.page === to.page && s.help.anchor === to.anchor;
  set({
    help: to,
    helpBack: same ? s.helpBack : [...s.helpBack, leaving(s.help)].slice(-100),
    helpForward: same ? s.helpForward : [],
    helpQuery: "",
    helpNonce: s.helpNonce + 1,
  });
  writeText("sk.helpPage", to.page);
}

/** Back (-1) or forward (1) in the help's history; the page comes back where it was scrolled to. */
export function helpHistory(dir: -1 | 1): void {
  const s = get();
  const from = dir < 0 ? s.helpBack : s.helpForward;
  const loc = from[from.length - 1];
  if (!loc) return;
  const here = leaving(s.help);
  if (dir < 0) set({ help: loc, helpBack: from.slice(0, -1), helpForward: [...s.helpForward, here], helpQuery: "", helpNonce: s.helpNonce + 1 });
  else set({ help: loc, helpForward: from.slice(0, -1), helpBack: [...s.helpBack, here], helpQuery: "", helpNonce: s.helpNonce + 1 });
  writeText("sk.helpPage", loc.page);
}

/** The place being left, with how far it was scrolled. */
function leaving(loc: HelpLoc): HelpLoc {
  const el = typeof document === "undefined" ? null : document.getElementById(HELP_SCROLL_ID);
  return el ? { ...loc, scroll: el.scrollTop } : loc;
}

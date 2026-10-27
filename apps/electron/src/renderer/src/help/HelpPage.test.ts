// The help tab renders (server-side, no DOM): its table of contents in the interface language, the
// page with its links, previous / next and the website link; the search shows its results.

import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HelpPage } from "@/pages/HelpPage";
import { helpTopic } from "@/state/help";
import { get, set } from "@/state/store";

import { NAV, ORDER, page } from "./docs";

// a server render reads a store's initial state: these tests read the current one
vi.mock(import("@/state/store"), async (original) => {
  const store = await original();
  return { ...store, useApp: Object.assign(<T>(pick: (s: ReturnType<typeof store.get>) => T) => pick(store.get()), store.useApp) as typeof store.useApp };
});

const html = () => renderToString(createElement(HelpPage));

afterEach(() => set({ lang: "zh-CN", help: { page: "index", anchor: null }, helpQuery: "" }));

describe("HelpPage", () => {
  it.each([
    ["zh-CN", "zh"],
    ["en", "en"],
  ] as const)("shows every page of the table of contents in %s, and the page asked for", (lang, doc) => {
    const id = ORDER[1] ?? ORDER[0]!;
    set({ lang, help: { page: id, anchor: null }, helpQuery: "" });
    const out = html();
    for (const g of NAV) expect(out).toContain(g.label[doc]);
    for (const x of ORDER) expect(out).toContain(escape(page(doc, x)!.title));
    expect(out).toContain(`<h1>${escape(page(doc, id)!.title)}</h1>`);
    expect(out).toContain('aria-current="page"');
    expect(out).toContain("help-pager-link");
  });

  it("falls back to the overview for a page it doesn't have", () => {
    set({ help: { page: "no-such-page", anchor: null } });
    expect(html()).toContain(`<h1>${escape(page("zh", "index")!.title)}</h1>`);
  });

  it("shows search results instead of the page while there is a query", () => {
    const first = page("zh", ORDER[0]!)!;
    set({ helpQuery: first.title });
    const out = html();
    expect(out).toContain("help-result");
    expect(out).not.toContain("help-pager");
  });

  it("opens on the page for where the user is", () => {
    expect(helpTopic(get())).toBe("first-scan"); // the first tab is a start tab
    set({ settingsPage: "keys", tabs: [{ id: "s", kind: "settings", viewIds: [], focusedPane: 0, title: "" }], active: "s" });
    expect(helpTopic(get())).toBe("shortcuts");
  });
});

function escape(text: string): string {
  return text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#x27;");
}

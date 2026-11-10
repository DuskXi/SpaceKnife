// @vitest-environment happy-dom
// The help tab renders its table of contents in the interface language, the page with its links,
// previous / next and the website link; the search shows its results. (help/HelpPage.test.ts in the
// Electron renderer, which rendered the React page on the server.)

import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { nextTick } from "vue";

import { NAV, ORDER, page } from "@/help/docs";
import { setLang } from "@/lib/i18n";
import type { SkApi } from "@/shared/api";
import { helpTopic } from "@/state/help";
import { get, resetState, set } from "@/state/store";

import HelpPage from "./HelpPage.vue";

const html = () => {
  const w = mount(HelpPage);
  const out = w.html();
  w.unmount();
  return out;
};

beforeEach(() => {
  resetState();
  window.sk = { platform: "win32", host: "app", shell: { openExternal: () => {}, copyText: () => {} } } as unknown as SkApi;
});
afterEach(() => {
  setLang("zh-CN");
  set({ lang: "zh-CN", help: { page: "index", anchor: null }, helpQuery: "" });
});

describe("HelpPage", () => {
  it.each([
    ["zh-CN", "zh"],
    ["en", "en"],
  ] as const)("shows every page of the table of contents in %s, and the page asked for", (lang, doc) => {
    const id = ORDER[1] ?? ORDER[0]!;
    setLang(lang);
    set({ lang, help: { page: id, anchor: null }, helpQuery: "" });
    const w = mount(HelpPage);
    const text = w.text();
    for (const g of NAV) expect(text).toContain(g.label[doc]);
    for (const x of ORDER) expect(text).toContain(page(doc, x)!.title);
    expect(w.find("h1").text()).toBe(page(doc, id)!.title);
    expect(w.find('[aria-current="page"]').exists()).toBe(true);
    expect(w.find(".help-pager-link").exists()).toBe(true);
    w.unmount();
  });

  it("falls back to the overview for a page it doesn't have", () => {
    set({ help: { page: "no-such-page", anchor: null } });
    const w = mount(HelpPage);
    expect(w.find("h1").text()).toBe(page("zh", "index")!.title);
    w.unmount();
  });

  it("shows search results instead of the page while there is a query", () => {
    const first = page("zh", ORDER[0]!)!;
    set({ helpQuery: first.title });
    const out = html();
    expect(out).toContain("help-result");
    expect(out).not.toContain("help-pager");
  });

  it("follows the query without a remount", async () => {
    const w = mount(HelpPage);
    expect(w.find(".help-pager").exists()).toBe(true);
    set({ helpQuery: page("zh", ORDER[0]!)!.title });
    await nextTick();
    expect(w.find(".help-results").exists()).toBe(true);
    w.unmount();
  });

  it("opens on the page for where the user is", () => {
    expect(helpTopic(get())).toBe("first-scan"); // the first tab is a start tab
    set({ settingsPage: "keys", tabs: [{ id: "s", kind: "settings", viewIds: [], focusedPane: 0, title: "" }], active: "s" });
    expect(helpTopic(get())).toBe("shortcuts");
  });
});

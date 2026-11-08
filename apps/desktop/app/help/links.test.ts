// The help tab's links: project pages open, others are copied; titles and marked search words.

import { afterEach, describe, expect, it, vi } from "vitest";

import { get, resetState } from "@/state/store";

import { OVERVIEW, page } from "./docs";
import { helpTitle, markWords, openLink } from "./links";

afterEach(() => {
  vi.unstubAllGlobals();
  resetState();
});

describe("help links", () => {
  it("opens the project's pages and copies other links", () => {
    const shell = { openExternal: vi.fn(), copyText: vi.fn() };
    vi.stubGlobal("window", { sk: { shell } });
    openLink("https://github.com/DuskXi/SpaceKnife/issues");
    expect(shell.openExternal).toHaveBeenCalledWith("https://github.com/DuskXi/SpaceKnife/issues");
    openLink("https://example.com/x");
    expect(shell.copyText).toHaveBeenCalledWith("https://example.com/x");
    expect(get().toast).toContain("https://example.com/x");
  });

  it("titles a page, the overview's title for an unknown one", () => {
    expect(helpTitle("zh-CN", "no-such-page")).toBe(page("zh", OVERVIEW)!.title);
    expect(helpTitle("en", OVERVIEW)).toBe(page("en", OVERVIEW)!.title);
  });

  it("marks every search word, merging overlaps", () => {
    expect(markWords("Scan a drive", ["scan", "drive"])).toEqual([
      { text: "Scan", mark: true },
      { text: " a ", mark: false },
      { text: "drive", mark: true },
    ]);
    expect(markWords("abcabc", ["abc", "bc"])).toEqual([
      { text: "abc", mark: true },
      { text: "abc", mark: true },
    ]);
    expect(markWords("plain", ["x"])).toEqual([{ text: "plain", mark: false }]);
  });
});

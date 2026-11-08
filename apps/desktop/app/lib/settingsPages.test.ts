// The settings pages, their groups and the search.

import { describe, expect, it } from "vitest";

import type { SettingMeta } from "@/protocol/generated";
import { setLang } from "@/lib/i18n";
import { SETTINGS_PAGES } from "@/state/store";

import { dependsOk, GROUPS, groupBy, markOnce, PAGES, pageName, searchSettings } from "./settingsPages";

const meta = (p: Partial<SettingMeta>): SettingMeta => ({
  applies: "now",
  category: "general",
  control: "switch",
  depends: null,
  description: "",
  enabled: true,
  group: "G",
  group_note: "",
  key: "k",
  max: null,
  min: null,
  name: "",
  options: [],
  reason: "",
  step: null,
  synonyms: [],
  unit: "",
  ...p,
});

describe("settings pages", () => {
  it("has a page for every settings page id, each in one nav group", () => {
    expect(PAGES.map((p) => p.id).sort()).toEqual([...SETTINGS_PAGES].sort());
    expect(GROUPS.flat().sort()).toEqual([...SETTINGS_PAGES].sort());
  });

  it("names pages in the interface language", () => {
    expect(pageName("general")).toBe("常规");
    setLang("en");
    expect(pageName("general")).not.toBe("常规");
    setLang("zh-CN");
    expect(pageName("nope")).toBe("nope");
  });

  it("groups rows in the order their groups appear", () => {
    const rows = [meta({ key: "a", group: "X" }), meta({ key: "b", group: "Y" }), meta({ key: "c", group: "X" })];
    expect(groupBy(rows).map(([g, r]) => [g, r.map((x) => x.key)])).toEqual([
      ["X", ["a", "c"]],
      ["Y", ["b"]],
    ]);
  });

  it("checks a row's dependency", () => {
    expect(dependsOk(null, { a: false })).toBe(true);
    expect(dependsOk("a", { a: false })).toBe(false);
    expect(dependsOk("a", { a: true })).toBe(true);
    expect(dependsOk("m=fast", { m: "fast" })).toBe(true);
    expect(dependsOk("m=fast", { m: "slow" })).toBe(false);
  });

  it("searches names, synonyms, options and page names", () => {
    const rows = [
      meta({ key: "a", name: "语言", synonyms: ["language"] }),
      meta({ key: "b", name: "主题", options: [{ label: "深色", value: "dark", reason: "" }] as SettingMeta["options"] }),
      meta({ key: "c", name: "线程", category: "scan" }),
    ];
    expect(searchSettings(rows, "LANG").map((m) => m.key)).toEqual(["a"]);
    expect(searchSettings(rows, "dark").map((m) => m.key)).toEqual(["b"]);
    expect(searchSettings(rows, "扫描与").map((m) => m.key)).toEqual(["c"]);
  });

  it("marks the first match of the query", () => {
    expect(markOnce("Hello world", "WOR")).toEqual(["Hello ", "wor", "ld"]);
    expect(markOnce("Hello", "x")).toEqual(["Hello", "", ""]);
    expect(markOnce("Hello")).toEqual(["Hello", "", ""]);
  });
});

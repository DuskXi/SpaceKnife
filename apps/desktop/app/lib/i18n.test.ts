import { afterEach, describe, expect, it } from "vitest";

import { byLang, englishCatalog, getLang, setLang, t } from "./i18n";

afterEach(() => setLang("zh-CN"));

describe("i18n", () => {
  it("returns the Chinese source text by default (tests run in Chinese)", () => {
    expect(getLang()).toBe("zh-CN");
    expect(t("可用空间")).toBe("可用空间");
    expect(t("已导出 {n} 项", { n: 3 })).toBe("已导出 3 项");
  });

  it("switches to English and fills placeholders", () => {
    const en = englishCatalog();
    expect(Object.keys(en).length).toBeGreaterThan(500); // every area's catalog was loaded
    const [zh, english] = Object.entries(en).find(([k]) => /\{\w+\}/.test(k)) ?? Object.entries(en)[0] ?? ["", ""];
    setLang("en");
    if (zh) {
      const fields = Object.fromEntries([...zh.matchAll(/\{(\w+)\}/g)].map((m) => [m[1]!, "X"]));
      expect(t(zh, fields)).toBe(english.replace(/\{(\w+)\}/g, "X"));
    }
    expect(t("这一句没有英文")).toBe("这一句没有英文"); // missing: the source text
    expect(byLang("中", "en")).toBe("en");
  });

  it("has no Chinese in any English entry and keeps the placeholders", () => {
    for (const [zh, en] of Object.entries(englishCatalog())) {
      expect(en, zh).not.toMatch(/[㐀-鿿]/);
      expect([...en.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort(), zh).toEqual([...zh.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort());
    }
  });
});

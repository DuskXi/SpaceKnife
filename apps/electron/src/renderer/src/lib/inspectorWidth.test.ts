import { afterEach, describe, expect, it, vi } from "vitest";

import { tokens } from "@design/tokens";

import { fmtCount } from "./format";
import { setLang } from "./i18n";
import { clampInspector, INSPECTOR_DEFAULT, INSPECTOR_MAX, INSPECTOR_MIN, storedInspectorWidth, storeInspectorWidth } from "./inspectorWidth";

afterEach(() => {
  vi.unstubAllGlobals();
  setLang("zh-CN");
});

function fakeStorage(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
    removeItem: (k: string) => void data.delete(k),
  };
}

describe("details column width (UI-MAIN-64)", () => {
  it("comes from the layout tokens", () => {
    expect([INSPECTOR_MIN, INSPECTOR_DEFAULT, INSPECTOR_MAX]).toEqual([tokens.layout.inspectorMin, tokens.layout.inspector, tokens.layout.inspectorMax]);
    expect(INSPECTOR_MIN).toBeLessThan(INSPECTOR_DEFAULT);
    expect(INSPECTOR_DEFAULT).toBeLessThan(INSPECTOR_MAX);
  });

  it("stays within its range and leaves the map its minimum", () => {
    expect(clampInspector(100)).toBe(INSPECTOR_MIN);
    expect(clampInspector(5000)).toBe(INSPECTOR_MAX);
    expect(clampInspector(411.6)).toBe(412);
    expect(clampInspector(600, 800)).toBe(800 - tokens.layout.mapMin);
    // a window too narrow for both: the column keeps its minimum
    expect(clampInspector(600, 500)).toBe(INSPECTOR_MIN);
  });

  it("is remembered per viewer, and forgotten at the default", () => {
    const ls = fakeStorage();
    vi.stubGlobal("localStorage", ls);
    expect(storedInspectorWidth()).toBe(INSPECTOR_DEFAULT);
    storeInspectorWidth(480);
    expect(ls.data.get("sk.inspectorWidth")).toBe("480");
    expect(storedInspectorWidth()).toBe(480);
    storeInspectorWidth(INSPECTOR_DEFAULT);
    expect(ls.data.has("sk.inspectorWidth")).toBe(false);
  });

  it("reads bad or out-of-range values safely", () => {
    vi.stubGlobal("localStorage", fakeStorage({ "sk.inspectorWidth": "9999" }));
    expect(storedInspectorWidth()).toBe(INSPECTOR_MAX);
    vi.stubGlobal("localStorage", fakeStorage({ "sk.inspectorWidth": "wide" }));
    expect(storedInspectorWidth()).toBe(INSPECTOR_DEFAULT);
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("denied");
      },
    });
    expect(storedInspectorWidth()).toBe(INSPECTOR_DEFAULT);
  });
});

describe("fmtCount (class statistics in a narrow column)", () => {
  it("keeps small counts whole and shortens large ones per language", () => {
    expect(fmtCount(9_999)).toBe("9,999");
    expect(fmtCount(250_752)).toBe("25.1万");
    expect(fmtCount(2_357_524)).toBe("235.8万");
    setLang("en");
    expect(fmtCount(250_752)).toBe("250.8K");
    expect(fmtCount(2_357_524)).toBe("2.4M");
  });
});

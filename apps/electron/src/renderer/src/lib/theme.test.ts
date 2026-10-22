import { afterEach, describe, expect, it, vi } from "vitest";

import { themes, tokens } from "@design/tokens";

import boot from "../../public/theme-boot.js?raw";
import indexHtml from "../../index.html?raw";
import { isThemeSetting, parseThemeFile, themeFileText, WINDOW_BG } from "../../../shared/theme";
import { accentHex, accentOf, accentOfSetting, accentSlot, applyAccent, applyTheme, inkOnAccent, onAccentHex, onThemeChange, palette, resolveTheme, THEME_KEY, themeName } from "./theme";

// test-setup.ts starts every file in the dark theme
afterEach(() => {
  vi.unstubAllGlobals();
  applyTheme("dark");
  applyAccent(tokens.color.accent.default);
});

/** A localStorage stand-in. */
function fakeStorage(init: Record<string, string> = {}) {
  const m = new Map(Object.entries(init));
  return {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, String(v)),
    removeItem: (k: string) => void m.delete(k),
    map: m,
  };
}

describe("theme", () => {
  it("follows the choice, else the system, light where nothing tells", () => {
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("system", true)).toBe("dark");
    // the AppImage review: dark only when the system says so
    expect(resolveTheme("system", null)).toBe("light");
    // unit tests run without a window: 「跟随系统」 is light there
    expect(applyTheme("system")).toBe(true);
    expect(themeName()).toBe("light");
    expect(palette()).toBe(themes.light);
  });

  it("switches what the canvases paint with, and says so once", () => {
    expect(themeName()).toBe("dark");
    expect(palette()).toBe(tokens);
    const seen: string[] = [];
    const off = onThemeChange(() => seen.push(themeName()));
    expect(applyTheme("light")).toBe(true);
    expect(palette()).toBe(themes.light);
    expect(palette().treemap.ground).toBe("#FFFFFF");
    expect(applyTheme("light")).toBe(false);
    expect(applyTheme("system")).toBe(false); // no system theme here: light, as it was
    expect(applyTheme("dark")).toBe(true);
    off();
    expect(applyTheme("light")).toBe(true);
    expect(seen).toEqual(["light", "dark"]);
  });

  it("keeps a preset accent's slot across themes", () => {
    expect(accentSlot("#5ec8b8")).toBe("teal");
    expect(accentSlot("#8B9CFF")).toBe("default");
    expect(accentSlot("#123456")).toBeNull();
    expect(applyAccent("#5EC8B8")).toBe(true);
    expect(applyAccent("#5ec8b8")).toBe(false);
    expect(accentHex()).toBe(themes.dark.color.accent.teal);
    expect(onAccentHex()).toBe(themes.dark.color.text.onAccent);
    applyTheme("light");
    expect(accentHex()).toBe(themes.light.color.accent.teal);
    expect(onAccentHex()).toBe(themes.light.color.text.onAccent);
    expect(accentOf("#D4FF3A")).toBe(themes.light.color.accent.lime);
  });

  it("keeps a colour of the user's as it is, in both themes, with readable text on it", () => {
    // the setting takes any #RRGGBB (settings.py): not one of the presets, so no slot
    expect(accentOfSetting("#123456")).toEqual({ hex: "#123456" });
    expect(accentOfSetting("#abcdef")).toEqual({ hex: "#ABCDEF" });
    expect(accentOfSetting("")).toEqual({ slot: "default" });
    expect(accentOfSetting("teal")).toEqual({ slot: "default" });
    expect(applyAccent("#123456")).toBe(true);
    expect(accentHex()).toBe("#123456");
    expect(onAccentHex()).toBe("#FFFFFF"); // dark blue: white text
    applyTheme("light");
    expect(accentHex()).toBe("#123456");
    expect(accentOf("#123456")).toBe("#123456");
    expect(inkOnAccent("#FFE680")).toBe(themes.dark.color.text.onAccent); // pale yellow: dark text
    // back to a preset: its slot again
    expect(applyAccent("#F2A65A")).toBe(true);
    expect(accentHex()).toBe(themes.light.color.accent.orange);
  });

  it("gives every theme the same token tree", () => {
    const shape = (v: unknown): unknown =>
      v && typeof v === "object" && !Array.isArray(v) ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, shape(x)])) : typeof v;
    expect(shape(themes.light)).toEqual(shape(themes.dark));
    // only colours change: geometry, type and motion are the same
    expect(themes.light.treemap.geometry).toEqual(themes.dark.treemap.geometry);
    expect(themes.light.layout).toEqual(themes.dark.layout);
  });
});

describe("the first frame in a browser", () => {
  // headless mode: no nativeTheme, and the settings come with the backend; the choice is kept in
  // localStorage and public/theme-boot.js puts it on the page before the styles apply
  const runBoot = (storage: unknown) => {
    const html = { dataset: {} as Record<string, string> };
    new Function("localStorage", "document", boot)(storage, { documentElement: html });
    return html.dataset.theme;
  };

  it("keeps a choice, forgets 「跟随系统」", () => {
    const ls = fakeStorage();
    vi.stubGlobal("localStorage", ls);
    applyTheme("light");
    expect(ls.map.get(THEME_KEY)).toBe("light");
    applyTheme("dark");
    expect(ls.map.get(THEME_KEY)).toBe("dark");
    applyTheme("system");
    expect(ls.map.has(THEME_KEY)).toBe(false);
    // no storage: nothing kept, nothing thrown
    vi.stubGlobal("localStorage", {
      setItem: () => {
        throw new Error("denied");
      },
    });
    expect(() => applyTheme("light")).not.toThrow();
  });

  it("sets data-theme from what lib/theme.ts kept, before anything else", () => {
    expect(runBoot(fakeStorage({ [THEME_KEY]: "dark" }))).toBe("dark");
    expect(runBoot(fakeStorage({ [THEME_KEY]: "light" }))).toBe("light");
    expect(runBoot(fakeStorage())).toBeUndefined();
    expect(runBoot(fakeStorage({ [THEME_KEY]: "sepia" }))).toBeUndefined();
    expect(runBoot(undefined)).toBeUndefined(); // no storage at all: no error either
    // a classic script in <head>, ahead of the bundle (and of the styles it brings)
    const head = indexHtml.slice(0, indexHtml.indexOf("</head>"));
    expect(head).toMatch(/<script src="\/theme-boot\.js"><\/script>/);
    expect(indexHtml.indexOf("theme-boot.js")).toBeLessThan(indexHtml.indexOf('type="module"'));
  });

  it("follows the system's theme while 「跟随系统」", () => {
    const listeners: (() => void)[] = [];
    const mq = { matches: false, addEventListener: (_: string, cb: () => void) => listeners.push(cb) };
    vi.stubGlobal("window", { matchMedia: () => mq });
    applyTheme("system");
    expect(themeName()).toBe("light");
    const seen: string[] = [];
    const off = onThemeChange(() => seen.push(themeName()));
    mq.matches = true;
    for (const cb of listeners) cb();
    expect(themeName()).toBe("dark");
    // a choice does not follow the system
    applyTheme("light");
    mq.matches = false;
    for (const cb of listeners) cb();
    mq.matches = true;
    for (const cb of listeners) cb();
    expect(themeName()).toBe("light");
    off();
    expect(seen).toEqual(["dark", "light"]);
    expect(listeners).toHaveLength(1); // watched once
  });
});

describe("window theme (main)", () => {
  it("paints the window in each theme's window colour", () => {
    expect(WINDOW_BG).toEqual({ dark: themes.dark.color.bg.window, light: themes.light.color.bg.window });
  });

  it("reads back what it saved, and 「跟随系统」 from anything else", () => {
    for (const theme of ["system", "light", "dark"] as const) expect(parseThemeFile(themeFileText(theme))).toBe(theme);
    for (const text of [null, "", "{", "null", "[]", '{"theme":"sepia"}', '{"theme":1}']) expect(parseThemeFile(text)).toBe("system");
  });

  it("takes only the three settings over IPC", () => {
    expect(["system", "light", "dark"].every(isThemeSetting)).toBe(true);
    expect([undefined, null, "", "Light", 0, {}].some(isThemeSetting)).toBe(false);
  });
});

describe("palette reads", () => {
  // what is painted takes its colours from palette() at paint time: a colour read from the base
  // tokens into a constant would stay dark in the light theme (geometry and timing are shared)
  const sources = import.meta.glob(["../**/*.ts", "../**/*.tsx", "!../**/*.test.ts", "!./theme.ts", "!../test-setup.ts"], { query: "?raw", import: "default", eager: true }) as Record<string, string>;

  it("leaves the base tokens' colours to lib/theme.ts", () => {
    expect(Object.keys(sources).length).toBeGreaterThan(50);
    const bad = Object.entries(sources).flatMap(([file, text]) =>
      [...text.matchAll(/tokens\.(color|treemap|visual)\b(?!\.(geometry|dimOpacity|flame|flamePolar)\b)/g)].map((m) => `${file}: ${m[0]}`),
    );
    expect(bad).toEqual([]);
  });
});

import { afterEach, describe, expect, it } from "vitest";

import { themes, tokens } from "@design/tokens";

import { isThemeSetting, parseThemeFile, themeFileText, WINDOW_BG } from "../../../shared/theme";
import { accentHex, accentOf, accentOfSetting, accentSlot, applyAccent, applyTheme, inkOnAccent, onAccentHex, onThemeChange, palette, resolveTheme, themeName } from "./theme";

// test-setup.ts starts every file in the dark theme
afterEach(() => {
  applyTheme("dark");
  applyAccent(tokens.color.accent.default);
});

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

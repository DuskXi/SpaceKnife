// The interface's light / dark theme (UI-W7-31 ①, W7 待定 20) and the accent's slot in it.
//
// The CSS follows by itself: tokens.css is light on :root and dark under prefers-color-scheme: dark
// (in the app, main sets nativeTheme.themeSource, which drives it) unless data-theme="light", and
// dark anywhere with data-theme="dark". So the first frame is right before any script runs. What
// the canvases paint reads palette() here, which follows the same rule; the store mirrors the
// theme's name (AppState.theme) so the views repaint when it changes.

import { type Theme, type ThemeName, themes } from "@design/tokens";

import type { ThemeSetting } from "../../../shared/theme";

const DARK_QUERY = "(prefers-color-scheme: dark)";
const media = (): MediaQueryList | null => (typeof window !== "undefined" && window.matchMedia ? window.matchMedia(DARK_QUERY) : null);

/** The theme a setting comes to: the one chosen, else the system's; light where nothing tells, as
 * tokens.css (only a system that says dark gets dark). */
export function resolveTheme(choice: ThemeSetting, systemDark: boolean | null = media()?.matches ?? null): ThemeName {
  if (choice === "light" || choice === "dark") return choice;
  return systemDark === true ? "dark" : "light";
}

let choice: ThemeSetting = "system";
let current: ThemeName = resolveTheme(choice);
const listeners = new Set<() => void>();

/** The theme on screen. */
export const themeName = (): ThemeName => current;

/** The tokens of the theme on screen: what the canvases paint with (the CSS has its own). */
export const palette = (): Theme => themes[current];

/** Called after the theme on screen changed (a setting, or the system's while 「跟随系统」). */
export function onThemeChange(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

function follow(): boolean {
  const next = resolveTheme(choice);
  if (next === current) return false;
  current = next;
  for (const cb of listeners) cb();
  return true;
}

let watching = false;

/** Apply the setting appearance.theme: data-theme on the page for a choice, none for 「跟随系统」 (the
 * media query decides); the system's changes are followed from then on. True when the theme on
 * screen changed. */
export function applyTheme(setting: ThemeSetting): boolean {
  choice = setting;
  if (typeof document !== "undefined") {
    if (setting === "system") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = setting;
  }
  const mq = media();
  if (mq && !watching) {
    watching = true;
    mq.addEventListener("change", () => void follow());
  }
  return follow();
}

// ------------------------------------------------------------------ accent (UI-W7-31 ②)

export const ACCENT_SLOTS = ["default", "lime", "teal", "orange"] as const;
export type AccentSlot = (typeof ACCENT_SLOTS)[number];

const HEX = /^#[0-9A-Fa-f]{6}$/;

/** The slot of a stored accent: appearance.accent keeps the dark preset's hex (settings.py ACCENTS),
 * and each theme has its own colour for the slot (color.accent.*). Null for a colour of the user's,
 * which is the same in both themes. */
export function accentSlot(hex: string): AccentSlot | null {
  const h = hex.toUpperCase();
  return ACCENT_SLOTS.find((k) => themes.dark.color.accent[k].toUpperCase() === h) ?? null;
}

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  }) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

const contrast = (a: string, b: string): number => {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p) as [number, number];
  return (x + 0.05) / (y + 0.05);
};

/** What goes on a colour of the user's as the accent: white or the dark theme's text on the accent,
 * whichever contrasts more (UI-W7-31 ②: ≥ 4.5:1 by itself, as far as any text can). */
export function inkOnAccent(hex: string): string {
  const white = themes.light.color.text.onAccent;
  const dark = themes.dark.color.text.onAccent;
  return contrast(hex, white) >= contrast(hex, dark) ? white : dark;
}

/** The accent in use: a preset's slot, or a colour of the user's. */
export type Accent = { slot: AccentSlot } | { hex: string };

let accent: Accent = { slot: "default" };

/** The accent a stored appearance.accent stands for: a preset's slot, a colour of the user's as it
 * is, the default for anything else. */
export function accentOfSetting(stored: string): Accent {
  const slot = accentSlot(stored);
  if (slot) return { slot };
  return HEX.test(stored) ? { hex: stored.toUpperCase() } : { slot: "default" };
}

const sameAccent = (a: Accent, b: Accent): boolean => ("slot" in a ? "slot" in b && a.slot === b.slot : "hex" in b && a.hex === b.hex);

/** Use the stored accent (W7 外观 · 强调色); true when it changed. CSS: a preset points
 * --sk-color-accent-default at the slot's variable, so a theme change brings the slot's colour of
 * that theme with it (the default slot is the token itself: pointing it at itself would be a
 * cycle). A colour of the user's is set as it is, in both themes, with the text on it chosen by
 * contrast. */
export function applyAccent(stored: string): boolean {
  const next = accentOfSetting(stored);
  if (sameAccent(next, accent)) return false;
  accent = next;
  if (typeof document === "undefined") return true;
  const style = document.documentElement.style;
  if ("hex" in next) {
    style.setProperty("--sk-color-accent-default", next.hex);
    style.setProperty("--sk-color-text-on-accent", inkOnAccent(next.hex));
    return true;
  }
  style.removeProperty("--sk-color-text-on-accent");
  if (next.slot === "default") style.removeProperty("--sk-color-accent-default");
  else style.setProperty("--sk-color-accent-default", `var(--sk-color-accent-${next.slot})`);
  return true;
}

/** The accent on screen, for what the canvases paint (the breathing outline, the 「新增」 badge). */
export const accentHex = (): string => ("hex" in accent ? accent.hex : palette().color.accent[accent.slot]);

/** The text on the accent on screen (the 「新增」 badge). */
export const onAccentHex = (): string => ("hex" in accent ? inkOnAccent(accent.hex) : palette().color.text.onAccent);

/** A stored accent's colour in the theme on screen, for a swatch (settings). */
export const accentOf = (stored: string): string => {
  const a = accentOfSetting(stored);
  return "hex" in a ? a.hex : palette().color.accent[a.slot];
};

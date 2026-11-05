// The interface theme (UI-W7-31 ①, setting appearance.theme), shared by main and the renderer.

/** 「跟随系统」 (the default), or one chosen in the settings. */
export type ThemeSetting = "system" | "light" | "dark";

export const isThemeSetting = (v: unknown): v is ThemeSetting => v === "system" || v === "light" || v === "dark";

/** The window's own background before the page paints and while it resizes: each theme's
 * color.bg.window (design/tokens; main has no @design alias, renderer/src/lib/theme.test.ts keeps
 * them equal). */
export const WINDOW_BG = { dark: "#08090B", light: "#E9EBEF" } as const;

/** main keeps the last choice in <userData>/theme.json: the next start is in that theme from its
 * first frame, before the backend's settings arrive. Anything unreadable is 「跟随系统」. */
export function parseThemeFile(text: string | null): ThemeSetting {
  if (!text) return "system";
  try {
    const theme = (JSON.parse(text) as { theme?: unknown } | null)?.theme;
    return isThemeSetting(theme) ? theme : "system";
  } catch {
    return "system";
  }
}

export const themeFileText = (theme: ThemeSetting): string => `${JSON.stringify({ theme })}\n`;

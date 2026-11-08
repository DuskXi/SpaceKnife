// The theme in main (W7 外观 · 主题): nativeTheme.themeSource decides the page's prefers-color-scheme
// (tokens.css is light unless it says dark), Electron's own UI and, on macOS, the window's. 「跟随系统」
// follows Windows' default app mode, macOS's appearance, and on Linux the desktop's appearance setting
// (xdg-desktop-portal) or the GTK theme, light when neither says. The last choice is kept in
// <userData>/theme.json so a start is in the right theme from its first frame, before the backend's
// settings arrive. Electron and the file system come in through ThemeHost (index.ts), so this is
// tested without them.

import { isThemeSetting, parseThemeFile, themeFileText, type ThemeSetting, WINDOW_BG } from "../shared/theme";

export interface ThemeHost {
  /** nativeTheme */
  native: { themeSource: ThemeSetting; readonly shouldUseDarkColors: boolean };
  /** the window's own background, when there is a window */
  setBackground(color: string): void;
  /** replace theme.json with this text (index.ts writes a temporary file and renames it over) */
  write(text: string): Promise<void>;
  /** a write that failed: the next start waits for the settings */
  warn(message: string): void;
}

export class ThemeKeeper {
  /** what theme.json holds (null: no file, or a write failed) */
  private saved: ThemeSetting | null;
  /** the writes, one at a time in the order of the changes: the file ends with the last choice */
  private writes: Promise<void> = Promise.resolve();

  /** Restore the theme of theme.json's text (null: no file, 「跟随系统」); before the window exists. */
  constructor(
    private readonly host: ThemeHost,
    fileText: string | null,
  ) {
    const theme = parseThemeFile(fileText);
    this.saved = fileText == null ? null : theme;
    host.native.themeSource = theme;
  }

  /** The window's background for the theme on screen: each theme's color.bg.window. */
  background(): string {
    return this.host.native.shouldUseDarkColors ? WINDOW_BG.dark : WINDOW_BG.light;
  }

  /** sk:setTheme: the renderer applied the setting to the page; here Electron, the window and the file. */
  set(theme: unknown): void {
    if (!isThemeSetting(theme)) return;
    this.host.native.themeSource = theme;
    this.host.setBackground(this.background());
    if (theme === this.saved) return;
    this.saved = theme;
    this.writes = this.writes.then(() =>
      this.host.write(themeFileText(theme)).catch((err: unknown) => {
        if (this.saved === theme) this.saved = null; // written again at the next change
        this.host.warn(`[theme] not saved: ${String(err)}`);
      }),
    );
  }

  /** nativeTheme's "updated": the system's theme changed (or themeSource did); the window follows. */
  updated(): void {
    this.host.setBackground(this.background());
  }

  /** The writes so far, settled (tests). */
  settled(): Promise<void> {
    return this.writes;
  }
}

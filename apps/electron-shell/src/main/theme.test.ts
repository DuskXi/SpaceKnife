import { describe, expect, it } from "vitest";

import { type ThemeSetting, WINDOW_BG } from "../shared/theme";
import { type ThemeHost, ThemeKeeper } from "./theme";

/** nativeTheme as Electron behaves: dark colours for 「深色」, for 「浅色」 never, else the system's. */
function fakeHost(systemDark = false, write?: (text: string) => Promise<void>) {
  const log: string[] = [];
  const files: string[] = [];
  const native = {
    themeSource: "system" as ThemeSetting,
    get shouldUseDarkColors() {
      return this.themeSource === "dark" || (this.themeSource === "system" && systemDark);
    },
  };
  const host: ThemeHost = {
    native,
    setBackground: (c) => log.push(`bg ${c}`),
    write:
      write ??
      (async (text) => {
        files.push(text);
      }),
    warn: (m) => log.push(m),
  };
  return { host, native, log, files, setSystem: (dark: boolean) => (systemDark = dark) };
}

describe("ThemeKeeper", () => {
  it("restores the file's choice before the window, 「跟随系统」 without one", () => {
    const a = fakeHost();
    const keeper = new ThemeKeeper(a.host, '{"theme":"dark"}\n');
    expect(a.native.themeSource).toBe("dark");
    expect(keeper.background()).toBe(WINDOW_BG.dark);
    const b = fakeHost();
    expect(new ThemeKeeper(b.host, null).background()).toBe(WINDOW_BG.light);
    expect(b.native.themeSource).toBe("system");
    const c = fakeHost(true);
    expect(new ThemeKeeper(c.host, "not json").background()).toBe(WINDOW_BG.dark);
    expect(c.native.themeSource).toBe("system");
  });

  it("takes the renderer's setting: Electron, the window's background, the file once per change", async () => {
    const h = fakeHost();
    const keeper = new ThemeKeeper(h.host, '{"theme":"light"}\n');
    keeper.set("light"); // what the file already says: not written again
    keeper.set("dark");
    keeper.set("dark");
    keeper.set("bogus"); // not a setting: ignored
    keeper.set("system");
    await keeper.settled();
    expect(h.native.themeSource).toBe("system");
    expect(h.log).toEqual([`bg ${WINDOW_BG.light}`, `bg ${WINDOW_BG.dark}`, `bg ${WINDOW_BG.dark}`, `bg ${WINDOW_BG.light}`]);
    expect(h.files).toEqual(['{"theme":"dark"}\n', '{"theme":"system"}\n']);
  });

  it("writes one file at a time, in order: quick switches end with the last choice", async () => {
    const files: string[] = [];
    let open = 0;
    let most = 0;
    const slow = async (text: string) => {
      open++;
      most = Math.max(most, open);
      await new Promise((r) => setTimeout(r, text.includes("dark") ? 20 : 1)); // the first write is the slowest
      files.push(text);
      open--;
    };
    const h = fakeHost(false, slow);
    const keeper = new ThemeKeeper(h.host, null);
    keeper.set("dark");
    keeper.set("light");
    keeper.set("system");
    await keeper.settled();
    expect(most).toBe(1);
    expect(files.at(-1)).toBe('{"theme":"system"}\n');
  });

  it("writes again after a write that failed", async () => {
    let fail = true;
    const files: string[] = [];
    const h = fakeHost(false, async (text) => {
      if (fail) throw new Error("disk full");
      files.push(text);
    });
    const keeper = new ThemeKeeper(h.host, null);
    keeper.set("dark");
    await keeper.settled();
    expect(h.log.at(-1)).toMatch(/^\[theme\] not saved: Error: disk full$/);
    fail = false;
    keeper.set("dark"); // the same choice: the file does not hold it yet
    await keeper.settled();
    expect(files).toEqual(['{"theme":"dark"}\n']);
  });

  it("follows the system's theme while 「跟随系统」", () => {
    const h = fakeHost(false);
    const keeper = new ThemeKeeper(h.host, null);
    h.setSystem(true);
    keeper.updated();
    expect(h.log).toEqual([`bg ${WINDOW_BG.dark}`]);
  });
});

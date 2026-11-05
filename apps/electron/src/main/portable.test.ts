import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { appBundle, isTranslocated, portableHome, untranslocate } from "./portable";

describe("portableHome", () => {
  const dirs: string[] = [];
  const appDir = (withData: "folder" | "file" | "none"): string => {
    const dir = mkdtempSync(join(tmpdir(), "sk-portable-"));
    dirs.push(dir);
    if (withData === "folder") mkdirSync(join(dir, "data"));
    if (withData === "file") writeFileSync(join(dir, "data"), "");
    return dir;
  };
  afterEach(() => {
    for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
  });

  it("uses a data folder next to the executable", () => {
    const dir = appDir("folder");
    expect(portableHome(join(dir, "SpaceKnife.exe"), "win32", true, {})).toBe(join(dir, "data"));
    expect(portableHome(join(dir, "spaceknife"), "linux", true, {})).toBe(join(dir, "data"));
  });

  it("is off without the folder, in development and with an explicit home", () => {
    expect(portableHome(join(appDir("none"), "SpaceKnife.exe"), "win32", true, {})).toBeNull();
    expect(portableHome(join(appDir("file"), "SpaceKnife.exe"), "win32", true, {})).toBeNull();
    const dir = appDir("folder");
    expect(portableHome(join(dir, "electron.exe"), "win32", false, {})).toBeNull();
    expect(portableHome(join(dir, "SpaceKnife.exe"), "win32", true, { SPACEKNIFE_HOME: "D:\\elsewhere" })).toBeNull();
  });

  it("on macOS uses a data folder next to the app bundle", () => {
    const dir = appDir("folder");
    const exe = join(dir, "SpaceKnife.app", "Contents", "MacOS", "SpaceKnife");
    expect(portableHome(exe, "darwin", true, {})).toBe(join(dir, "data"));
    // an executable that is not in a bundle, and a bundle with no data folder beside it (a dmg install)
    expect(portableHome(join(dir, "SpaceKnife"), "darwin", true, {})).toBeNull();
    expect(portableHome(join(appDir("none"), "SpaceKnife.app", "Contents", "MacOS", "SpaceKnife"), "darwin", true, {})).toBeNull();
  });

  it("on macOS finds the folder next to the original of a translocated copy", () => {
    const dir = appDir("folder");
    const moved = "/private/var/folders/x1/abc/T/AppTranslocation/6E1F2B3C/d/SpaceKnife.app";
    const original = (path: string) => (path === moved ? join(dir, "SpaceKnife.app") : null);
    expect(portableHome(`${moved}/Contents/MacOS/SpaceKnife`, "darwin", true, {}, original)).toBe(join(dir, "data"));
    // where the original can't be found it runs as an installed app does
    expect(portableHome(`${moved}/Contents/MacOS/SpaceKnife`, "darwin", true, {}, () => null)).toBeNull();
  });
});

describe("macOS bundles and App Translocation", () => {
  it("finds the bundle of the executable only inside Contents/MacOS", () => {
    expect(appBundle("/Users/lenny/Apps/SpaceKnife.app/Contents/MacOS/SpaceKnife")).toBe("/Users/lenny/Apps/SpaceKnife.app");
    expect(appBundle("/Users/lenny/Apps/SpaceKnife/MacOS/SpaceKnife")).toBeNull();
    expect(appBundle("/Users/lenny/Apps/spaceknife")).toBeNull();
  });

  it("maps a translocated path back through the mount table", () => {
    const table = [
      "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)",
      "/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled, nobrowse, protect)",
      "/Users/lenny/Downloads/SpaceKnife-0.4.0-mac-arm64-portable/SpaceKnife.app on /private/var/folders/x1/abc/T/AppTranslocation/6E1F2B3C/d/SpaceKnife.app (nullfs, local, nodev, nosuid, read-only, nobrowse, mounted by lenny)",
      "/Users/lenny/My Apps/Other.app on /private/var/folders/x1/abc/T/AppTranslocation/99AA/d/Other.app (nullfs, local, read-only)",
    ].join("\n");
    const moved = "/private/var/folders/x1/abc/T/AppTranslocation/6E1F2B3C/d/SpaceKnife.app";
    const original = "/Users/lenny/Downloads/SpaceKnife-0.4.0-mac-arm64-portable/SpaceKnife.app";
    expect(isTranslocated(moved)).toBe(true);
    expect(isTranslocated("/Applications/SpaceKnife.app")).toBe(false);
    expect(untranslocate(moved, table)).toBe(original);
    expect(untranslocate(`${moved}/Contents/MacOS/SpaceKnife`, table)).toBe(`${original}/Contents/MacOS/SpaceKnife`);
    expect(untranslocate("/private/var/folders/x1/abc/T/AppTranslocation/99AA/d/Other.app", table)).toBe("/Users/lenny/My Apps/Other.app");
    expect(untranslocate("/private/var/folders/x1/abc/T/AppTranslocation/77/d/Gone.app", table)).toBeNull();
  });
});

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { portableHome } from "./portable";

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

  it("is off without the folder, in development, on macOS and with an explicit home", () => {
    expect(portableHome(join(appDir("none"), "SpaceKnife.exe"), "win32", true, {})).toBeNull();
    expect(portableHome(join(appDir("file"), "SpaceKnife.exe"), "win32", true, {})).toBeNull();
    const dir = appDir("folder");
    expect(portableHome(join(dir, "electron.exe"), "win32", false, {})).toBeNull();
    expect(portableHome(join(dir, "SpaceKnife"), "darwin", true, {})).toBeNull();
    expect(portableHome(join(dir, "SpaceKnife.exe"), "win32", true, { SPACEKNIFE_HOME: "D:\\elsewhere" })).toBeNull();
  });
});

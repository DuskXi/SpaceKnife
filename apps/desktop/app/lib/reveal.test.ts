import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SkApi } from "@/shared/api";

import { folderOf, handOff, openableByPath, textNamesIt } from "./reveal";

beforeEach(() => vi.stubGlobal("window", { sk: { platform: "win32" } as unknown as SkApi }));
afterEach(() => vi.unstubAllGlobals());

describe("handing a path to the system", () => {
  it("is fine for ordinary paths", () => {
    expect(openableByPath("C:\\Users\\lenny\\setup.exe", true)).toBe(true);
    expect(openableByPath("C:\\a.b\\c d\\.git", true)).toBe(true);
    expect(openableByPath("\\\\nas\\share\\x", true)).toBe(true);
    expect(openableByPath("/home/lenny/odd.", false)).toBe(true); // POSIX keeps a trailing dot
  });

  it("refuses names Win32 would rewrite: setup. opens the setup beside it", () => {
    expect(openableByPath("C:\\Users\\lenny\\setup.", true)).toBe(false);
    expect(openableByPath("C:\\dir \\file.txt", true)).toBe(false);
    expect(openableByPath("C:/dots./x", true)).toBe(false);
  });

  it("opens a terminal in the item's folder", () => {
    expect(folderOf("C:\\x\\setup.\\a.txt", false)).toBe("C:\\x\\setup.");
    expect(openableByPath(folderOf("C:\\x\\setup.\\a.txt", false), true)).toBe(false);
    expect(folderOf("/home/lenny/dir", true)).toBe("/home/lenny/dir");
  });

  it("knows from path.forms' URI whether its text names the item", () => {
    // a lone surrogate (Windows, WTF-8 in the URI), a byte that is not UTF-8 (POSIX): the text
    // forms write them escaped, which names nothing (or another item)
    const odd = { raw: "C:\\x\\a\\udc80b", uri: "file:///C:/x/a%ED%B2%80b" };
    expect(textNamesIt(odd)).toBe(false);
    expect(handOff(odd)).toBeNull();
    expect(textNamesIt({ uri: "file:///home/lenny/a%FFb" })).toBe(false);
    // a name that really holds U+FFFD, or that text, is a name like any other
    const plain = { raw: "C:\\x\\a\uFFFD\\udc80.txt", uri: "file:///C:/x/a%EF%BF%BD%5Cudc80.txt" };
    expect(textNamesIt(plain)).toBe(true);
    expect(handOff(plain)).toBe(plain.raw);
    // and the rest of the rules still hold
    expect(handOff({ raw: "C:\\x\\setup.", uri: "file:///C:/x/setup." })).toBeNull();
  });
});

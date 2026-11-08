import { describe, expect, it } from "vitest";

import { folderOf, openableByPath } from "./reveal";

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

  it("refuses names shown with U+FFFD (not valid Unicode on disk)", () => {
    expect(openableByPath("C:\\x\\a\uFFFD.txt", true)).toBe(false);
    expect(openableByPath("/home/lenny/a\uFFFD", false)).toBe(false);
  });
});

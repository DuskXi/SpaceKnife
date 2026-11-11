import { describe, expect, it } from "vitest";

import { addFolders, folderLines, isFullPath, splitPath } from "./folderList";

describe("a folders setting's value", () => {
  it("reads one folder a line, each once, as the backend keeps them", () => {
    expect(folderLines(" /a \n\n/b\r\n/a\n")).toEqual(["/a", "/b"]);
    expect(folderLines("")).toEqual([]);
    expect(folderLines(undefined)).toEqual([]);
  });

  it("adds folders after the ones it has, and says whether any was new", () => {
    expect(addFolders("/a", ["/b", " /a ", "/b"])).toEqual({ value: "/a\n/b", added: true });
    expect(addFolders("/a\n/b", ["/b"])).toEqual({ value: "/a\n/b", added: false });
    expect(addFolders("", ["C:\\Users\\lenny\\Music"])).toEqual({ value: "C:\\Users\\lenny\\Music", added: true });
  });

  it("shows a folder by its name, then where it is", () => {
    expect(splitPath("C:\\Users\\lenny\\Music")).toEqual({ name: "Music", dir: "C:\\Users\\lenny\\" });
    expect(splitPath("/home/lenny/Games/")).toEqual({ name: "Games", dir: "/home/lenny/" });
    expect(splitPath("\\\\nas\\share\\Movies")).toEqual({ name: "Movies", dir: "\\\\nas\\share\\" });
    expect(splitPath("D:\\")).toEqual({ name: "D:\\", dir: "" });
    expect(splitPath("/")).toEqual({ name: "/", dir: "" });
  });

  it("takes only full paths", () => {
    expect(["C:\\Users\\lenny\\Music", "d:/Games", "\\\\nas\\share\\Movies"].map((p) => isFullPath(p, true))).toEqual([true, true, true]);
    expect(["Music", "~/Music", "/home/lenny", "C:Music", "\\\\nas"].map((p) => isFullPath(p, true))).toEqual([false, false, false, false, false]);
    expect(["/home/lenny/Music", "/Volumes/USB"].map((p) => isFullPath(p, false))).toEqual([true, true]);
    expect(["~/Music", "Music", "C:\\Users"].map((p) => isFullPath(p, false))).toEqual([false, false, false]);
  });
});

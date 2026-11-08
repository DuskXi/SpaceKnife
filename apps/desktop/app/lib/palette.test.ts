import { describe, expect, it } from "vitest";

import type { PaletteCommand, PaletteDirect, PalettePaths, PaletteRecent, PaletteResult } from "@/protocol/generated";

import { firstRunnable, hitParts, paletteLines, withSep } from "./palette";

const direct = (patch: Partial<PaletteDirect>): PaletteDirect => ({ alloc: null, complete: null, diagnostics: [], is_dir: true, kind: "path", node: null, state: null, text: "x", tokens: [], valid: true, ...patch });
const recent = (patch: Partial<PaletteRecent>): PaletteRecent => ({ alloc: null, exists: true, hits: [], is_dir: true, kind: "jump", node: 1, text: "C:\\a", tokens: [], ...patch });
const cmd = (id: string, enabled = true): PaletteCommand => ({ id, name: id, enabled, hits: [], icon: "", keys: [], more: false, status: "" });
const res = (patch: Partial<PaletteResult>): PaletteResult => ({ text: "", commands: [], direct: [], recent: [], paths: false, seed: null, ...patch });

describe("palette lines", () => {
  it("lays out the groups in order and says which rows run", () => {
    const paths = { items: [{ node: 5 }], total: 1 } as PalettePaths;
    const r = res({
      direct: [direct({ kind: "filter", valid: true }), direct({ state: "outside" }), direct({ state: "in_scan" })],
      recent: [recent({}), recent({ exists: false })],
      commands: [cmd("a"), cmd("b", false)],
      paths: true,
    });
    const scan = paletteLines(r, paths, true);
    expect(scan.map((l) => `${l.group}:${l.row.kind}:${l.enabled}`)).toEqual([
      "direct:filter:true",
      "direct:goto:true",
      "direct:goto:true",
      "recent:recent:true",
      "recent:recent:false",
      "command:command:true",
      "command:command:false",
      "path:path:true",
    ]);
    // outside a scan tab only a path outside a scan and the enabled commands run
    expect(paletteLines(r, null, false).filter((l) => l.enabled).map((l) => l.row.kind)).toEqual(["goto", "command"]);
    expect(paletteLines(null, null, true)).toEqual([]);
  });

  it("runs the first runnable direct row, recent entry or command", () => {
    expect(firstRunnable(res({ direct: [direct({ kind: "filter", valid: false })], commands: [cmd("a", false), cmd("b")] }), true)?.row).toEqual({ kind: "command", c: cmd("b") });
    expect(firstRunnable(res({ recent: [recent({ text: "r" })], commands: [cmd("b")] }), true)?.row.kind).toBe("recent");
    expect(firstRunnable(res({ recent: [recent({})] }), false)).toBeNull();
  });

  it("splits text at its hits", () => {
    expect(hitParts("abc", [])).toEqual([{ text: "abc", hit: false }]);
    expect(
      hitParts("Documents", [
        { start: 0, end: 1 },
        { start: 3, end: 5 },
      ]),
    ).toEqual([
      { text: "D", hit: true },
      { text: "oc", hit: false },
      { text: "um", hit: true },
      { text: "ents", hit: false },
    ]);
  });

  it("ends folders with their separator", () => {
    expect(withSep("C:\\a")).toBe("C:\\a\\");
    expect(withSep("C:")).toBe("C:\\");
    expect(withSep("/home/a/")).toBe("/home/a/");
    expect(withSep("/home")).toBe("/home/");
  });
});

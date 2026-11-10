// The file class rows: extensions, duplicates, adding and moving (「其他」 stays last).

import { describe, expect, it } from "vitest";

import { addRow, type ClassRow, duplicates, moveRow, rowAt, rowChanged, splitExts, toRow } from "./classesEditor";

const row = (key: string, exts = "", id: string | null = key): ClassRow => ({ key, id, name: key, exts, color: "", color_light: "" });

describe("classes editor rows", () => {
  it("splits extensions", () => {
    expect(splitExts(" .MP4; mkv,  ..avi  ")).toEqual(["mp4", "mkv", "avi"]);
  });

  it("notes extensions an earlier class takes", () => {
    const d = duplicates([row("video", "mp4; mkv"), row("y", "MKV; zip"), row("x", "zip")]);
    expect(d.get("y")).toEqual(["mkv（video）"]);
    expect(d.get("x")).toEqual(["zip（y）"]);
    expect(d.has("video")).toBe(false);
  });

  it("tells a changed row", () => {
    const info = { id: "a", name: "A", extensions: ["mp4", "mkv"], color: "", color_light: "" } as unknown as Parameters<typeof toRow>[0];
    const r = toRow(info);
    expect(r.exts).toBe("mp4; mkv");
    expect(rowChanged(r, info)).toBe(false);
    expect(rowChanged({ ...r, exts: ".MP4 mkv" }, info)).toBe(false);
    expect(rowChanged({ ...r, name: "B" }, info)).toBe(true);
    expect(rowChanged(r, undefined)).toBe(true);
  });

  it("adds a class before 「其他」", () => {
    const rows = addRow([row("a"), row("other")]);
    expect(rows.map((r) => r.id)).toEqual(["a", null, "other"]);
    expect(rows[1]!.color).toBe(rows[1]!.color_light);
  });

  it("moves a row before another and keeps 「其他」 last", () => {
    const rows = [row("a"), row("b"), row("c"), row("other")];
    expect(moveRow(rows, "c", "a")!.map((r) => r.key)).toEqual(["c", "a", "b", "other"]);
    expect(moveRow(rows, "a", "other")!.map((r) => r.key)).toEqual(["b", "c", "a", "other"]);
    expect(moveRow(rows, "a", "a")).toBeNull();
  });

  it("finds the row under the pointer", () => {
    const boxes = [
      { key: "a", top: 0, bottom: 10 },
      { key: "b", top: 10, bottom: 20 },
    ];
    expect(rowAt(boxes, 15)).toBe("b");
    expect(rowAt(boxes, -5)).toBe("a");
    expect(rowAt(boxes, 50)).toBe("b");
    expect(rowAt([], 1)).toBeNull();
  });
});

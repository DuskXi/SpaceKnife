import { describe, expect, it } from "vitest";

import { decodeFrame } from "@/protocol/rpc";
import type { DrawFrame } from "@/treemap/frame";

import { ancestorsOf, decodeVisualFrame, GEOMETRY_COLUMNS, isKnownLayout, isPolarFrame, isRectFrame, isSynthetic, isWithin, Kind } from "./frame";
import { encodeBinary, encodeFrame, FREE_KEY, frameInfo, polarScene, rectScene, rowColumns, SMALL_KEY, TREEMAP_ROWS } from "./testing/fixtures";

describe("GEOMETRY_COLUMNS", () => {
  it("names the geometry of each layout", () => {
    expect(GEOMETRY_COLUMNS["drawlist.v1"]).toEqual(["x", "y", "w", "h"]);
    expect(GEOMETRY_COLUMNS["flame.v1"]).toEqual(["x", "y", "w", "h"]);
    expect(GEOMETRY_COLUMNS["polar.v1"]).toEqual(["a0", "a1", "r0", "r1"]);
  });

  it("knows exactly those layouts", () => {
    expect(isKnownLayout("drawlist.v1")).toBe(true);
    expect(isKnownLayout("flame.v1")).toBe(true);
    expect(isKnownLayout("polar.v1")).toBe(true);
    expect(isKnownLayout("flame.v2")).toBe(false);
    expect(isKnownLayout("toString")).toBe(false);
    expect(isKnownLayout("")).toBe(false);
  });
});

describe("decodeVisualFrame", () => {
  it("decodes every shared column of a drawlist.v1 frame", () => {
    const { bin, info, frame: f } = rectScene();
    expect(bin.header.layout).toBe("drawlist.v1");
    expect(f.layout).toBe("drawlist.v1");
    expect(f.info).toBe(info);
    expect(f.count).toBe(TREEMAP_ROWS.length);
    expect(Array.from(f.key)).toEqual(TREEMAP_ROWS.map((r) => r.key));
    expect(Array.from(f.node)).toEqual(TREEMAP_ROWS.map((r) => r.node));
    expect(Array.from(f.parent)).toEqual(TREEMAP_ROWS.map((r) => r.parent));
    expect(Array.from(f.kind)).toEqual(TREEMAP_ROWS.map((r) => r.kind));
    expect(Array.from(f.depth)).toEqual(TREEMAP_ROWS.map((r) => r.depth));
    expect(Array.from(f.cls)).toEqual(TREEMAP_ROWS.map((r) => r.cls ?? 0));
    expect(Array.from(f.label)).toEqual(TREEMAP_ROWS.map((r) => r.label ?? 0));
    expect(Array.from(f.size)).toEqual(TREEMAP_ROWS.map((r) => r.size));
    expect(Array.from(f.count_)).toEqual(TREEMAP_ROWS.map((r) => r.count ?? 0));
    expect(Array.from(f.flags)).toEqual(TREEMAP_ROWS.map((r) => r.flags ?? 0));
    expect(Array.from(f.tag)).toEqual(TREEMAP_ROWS.map((r) => r.tag ?? 0));
    expect(Array.from(f.tone)).toEqual(TREEMAP_ROWS.map((r) => r.tone ?? 0));
    expect(Array.from(f.branch!)).toEqual(TREEMAP_ROWS.map((r) => r.branch ?? 0));
    expect(f.delta).toBeNull();
    expect(f.dstat).toBeNull();
  });

  it("puts the rectangle columns on the frame and in geometry, the same arrays", () => {
    const { frame: f } = rectScene();
    expect(isRectFrame(f)).toBe(true);
    expect(isPolarFrame(f)).toBe(false);
    expect(Object.keys(f.geometry)).toEqual(["x", "y", "w", "h"]);
    for (const k of ["x", "y", "w", "h"] as const) expect(f[k]).toBe(f.geometry[k]);
    expect(Array.from(f.x)).toEqual(TREEMAP_ROWS.map((r) => r.geo[0]));
    expect(Array.from(f.h)).toEqual(TREEMAP_ROWS.map((r) => r.geo[3]));
    // what the treemap's paint and hit test take
    const asDraw: DrawFrame = f;
    expect(asDraw.w[1]).toBe(100);
  });

  it("puts the sector columns on a polar.v1 frame", () => {
    const { frame: f } = polarScene();
    expect(f.layout).toBe("polar.v1");
    expect(isPolarFrame(f)).toBe(true);
    expect(isRectFrame(f)).toBe(false);
    expect(Object.keys(f.geometry)).toEqual(["a0", "a1", "r0", "r1"]);
    for (const k of ["a0", "a1", "r0", "r1"] as const) {
      expect(f[k]).toBe(f.geometry[k]);
      expect(f[k]).toBeInstanceOf(Int32Array);
      expect(f[k].length).toBe(f.count);
    }
    expect("x" in f).toBe(false);
  });

  it("decodes flame.v1 with the rectangle columns", () => {
    const { frame: f } = rectScene({ layout: "flame.v1" });
    expect(f.layout).toBe("flame.v1");
    expect(f.info.visual).toBe("flame");
    expect(isRectFrame(f)).toBe(true);
    expect(f.count).toBeGreaterThan(0);
  });

  it("views the received buffer without copying", () => {
    const { buf, frame: f } = polarScene();
    for (const col of [f.key, f.node, f.kind, f.size, f.flags, f.branch!, f.a0, f.r1]) expect(col.buffer).toBe(buf);
  });

  it("is null for a layout it does not know, without reading the columns", () => {
    const { columns, names } = rowColumns("drawlist.v1", TREEMAP_ROWS);
    const bin = decodeFrame(encodeBinary("treemap3d.v1", columns, { names }));
    expect(decodeVisualFrame(bin, frameInfo({ layout: "treemap3d.v1" }))).toBeNull();
  });

  it("is null when a geometry column is missing or not <i4", () => {
    const { columns, names } = rowColumns("polar.v1", polarScene().rows);
    delete columns.r1;
    expect(decodeVisualFrame(decodeFrame(encodeBinary("polar.v1", columns, { names })), frameInfo())).toBeNull();
    const rect = rowColumns("flame.v1", TREEMAP_ROWS);
    rect.columns.w = Float32Array.from(rect.columns.w as Int32Array);
    expect(decodeVisualFrame(decodeFrame(encodeBinary("flame.v1", rect.columns, { names: rect.names })), frameInfo())).toBeNull();
  });

  it("decodes the layout of the binary frame, not the one the JSON announced", () => {
    const { columns, names } = rowColumns("drawlist.v1", TREEMAP_ROWS);
    const f = decodeVisualFrame(decodeFrame(encodeBinary("drawlist.v1", columns, { names })), frameInfo({ layout: "flame.v1" }));
    expect(f?.layout).toBe("drawlist.v1");
  });

  it("has no branch column from a server before visuals.v1", () => {
    const { columns, names } = rowColumns("drawlist.v1", TREEMAP_ROWS);
    delete columns.branch;
    const f = decodeVisualFrame(decodeFrame(encodeBinary("drawlist.v1", columns, { names })), frameInfo());
    expect(f).not.toBeNull();
    expect(f!.branch).toBeNull();
  });

  it("decodes the compare columns when present", () => {
    const rows = TREEMAP_ROWS.map((r, i) => ({ ...r, delta: BigInt(i * 100 - 200), dstat: i % 4 }));
    const { frame: f } = encodeFrame("drawlist.v1", rows);
    expect(Array.from(f.delta!)).toEqual(rows.map((r) => r.delta));
    expect(Array.from(f.dstat!)).toEqual(rows.map((r) => r.dstat));
  });

  it("splits the names blob by byte length (UTF-8), empty for name_len 0", () => {
    const { frame: f } = rectScene();
    expect(f.names).toEqual(["root", "a.mp4", "sub", "中文.txt", "", ""]);
  });

  it("tolerates a frame without a names blob when every name is empty", () => {
    const rows = TREEMAP_ROWS.map((r) => ({ ...r, name: "" }));
    const { columns } = rowColumns("drawlist.v1", rows);
    const f = decodeVisualFrame(decodeFrame(encodeBinary("drawlist.v1", columns)), frameInfo());
    expect(f!.names).toEqual(rows.map(() => ""));
  });

  it("decodes empty frames of every layout", () => {
    for (const layout of Object.keys(GEOMETRY_COLUMNS)) {
      const { frame: f } = encodeFrame(layout, []);
      expect(f.count).toBe(0);
      expect(f.names).toEqual([]);
      expect(f.rowOf(1)).toBe(-1);
      for (const name of GEOMETRY_COLUMNS[layout]!) expect(f.geometry[name]!.length).toBe(0);
    }
  });

  it("maps keys to rows, including synthetic keys, and -1 for unknown keys", () => {
    const { frame: f } = rectScene();
    TREEMAP_ROWS.forEach((r, i) => expect(f.rowOf(r.key)).toBe(i));
    expect(f.rowOf(SMALL_KEY)).toBe(4);
    expect(f.rowOf(FREE_KEY)).toBe(5);
    expect(f.rowOf(999)).toBe(-1);
  });
});

describe("tree helpers", () => {
  const { frame: f } = rectScene();

  it("ancestorsOf walks up to the top, nearest parent first, excluding the row", () => {
    expect(ancestorsOf(f, 3)).toEqual([2, 0]);
    expect(ancestorsOf(f, 4)).toEqual([2, 0]);
    expect(ancestorsOf(f, 1)).toEqual([0]);
    expect(ancestorsOf(f, 0)).toEqual([]);
    expect(ancestorsOf(f, 5)).toEqual([]);
  });

  it("isWithin is true for the row itself and its descendants only", () => {
    expect(isWithin(f, 3, 3)).toBe(true);
    expect(isWithin(f, 3, 2)).toBe(true);
    expect(isWithin(f, 3, 0)).toBe(true);
    expect(isWithin(f, 3, 1)).toBe(false);
    expect(isWithin(f, 0, 3)).toBe(false);
    expect(isWithin(f, 5, 0)).toBe(false);
    expect(isWithin(f, -1, 0)).toBe(false);
  });

  it("isSynthetic is true exactly when one of the top two key bits is set", () => {
    expect(isSynthetic(1)).toBe(false);
    expect(isSynthetic(0x3fffffff)).toBe(false);
    expect(isSynthetic(0x40000000)).toBe(true);
    expect(isSynthetic(0xffffffff)).toBe(true);
    expect(Array.from(f.key, isSynthetic)).toEqual([false, false, false, false, true, true]);
  });

  it("uses the protocol's kind numbers", () => {
    expect(Kind).toEqual({ FILE: 0, FOLDER: 1, COLLAPSED: 2, PENDING: 3, SMALL: 4, MORE: 5, FREE: 6, UNKNOWN: 7 });
  });
});

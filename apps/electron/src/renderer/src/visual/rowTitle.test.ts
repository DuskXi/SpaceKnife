import { describe, expect, it } from "vitest";

import { Kind } from "./frame";
import { rootTitle, rowTitle } from "./rowTitle";
import { encodeFrame, polarScene, rectScene, SMALL_KEY, TREEMAP_ROWS } from "./testing/fixtures";
import { HIT_NONE, HIT_ROOT } from "./types";

const fmtInt = (n: number) => `#${n}`;
const o = { scanning: false, formatInt: fmtInt };

describe("rowTitle", () => {
  const { frame: f } = rectScene();

  it("names files and folders by the frame's names", () => {
    expect(rowTitle(f, 1, o)).toEqual({ line1: "a.mp4", line2: null });
    expect(rowTitle(f, 2, o)).toEqual({ line1: "sub", line2: null });
    expect(rowTitle(f, 3, o)).toEqual({ line1: "中文.txt", line2: null });
  });

  it("writes what aggregates and free space are", () => {
    expect(rowTitle(f, f.rowOf(SMALL_KEY), o)).toEqual({ line1: "#12 个小项", line2: null });
    expect(rowTitle(f, 5, o)).toEqual({ line1: "可用空间", line2: null });
  });

  it("covers the other synthetic and waiting rows", () => {
    const rows = [
      { ...TREEMAP_ROWS[4]!, parent: -1, kind: Kind.MORE, count: 1234 },
      { ...TREEMAP_ROWS[5]!, kind: Kind.UNKNOWN, key: 0xc0000002 },
      { ...TREEMAP_ROWS[2]!, kind: Kind.PENDING, parent: -1, name: "cache" },
      { ...TREEMAP_ROWS[1]!, parent: -1, label: 0, name: "" },
    ];
    const g = encodeFrame("drawlist.v1", rows).frame;
    expect(rowTitle(g, 0, o)).toEqual({ line1: "+#1234 项", line2: null });
    expect(rowTitle(g, 1, o)).toEqual({ line1: "未知空间", line2: null });
    expect(rowTitle(g, 1, { ...o, scanning: true })).toEqual({ line1: "未知空间 · 尚未扫描", line2: null });
    expect(rowTitle(g, 2, o)).toEqual({ line1: "cache", line2: "等待扫描…" });
    // a row the backend sent no name for
    expect(rowTitle(g, 3, o)).toEqual({ line1: null, line2: null });
  });

  it("is empty for no row", () => {
    expect(rowTitle(f, HIT_NONE, o)).toEqual({ line1: null, line2: null });
    expect(rowTitle(f, f.count, o)).toEqual({ line1: null, line2: null });
  });

  it("names the zoom root by its crumb", () => {
    const p = polarScene().frame;
    expect(rowTitle(p, HIT_ROOT, o)).toEqual({ line1: "root", line2: null });
    const deep = polarScene({
      info: {
        breadcrumb: [
          { key: 1, label: "/home/lenny/" },
          { key: 3, label: "projects" },
        ],
      },
    }).frame;
    expect(rowTitle(deep, HIT_ROOT, o).line1).toBe("projects");
    const top = polarScene({ info: { breadcrumb: [{ key: 1, label: "C:\\" }] } }).frame;
    expect(rowTitle(top, HIT_ROOT, o).line1).toBe("C:");
    const slash = polarScene({ info: { breadcrumb: [{ key: 1, label: "/" }] } }).frame;
    expect(rowTitle(slash, HIT_ROOT, o).line1).toBe("/");
  });

  it("names a scanned folder by its own name where the label is narrow", () => {
    const at = (label: string) => rootTitle(polarScene({ info: { breadcrumb: [{ key: 1, label }] } }).frame, fmtInt, true).line1;
    expect(at("C:\\Users\\lenny\\Downloads\\")).toBe("Downloads");
    expect(at("/home/lenny/projects")).toBe("projects");
    expect(at("C:\\")).toBe("C:");
    expect(at("/")).toBe("/");
    // the whole path otherwise (the flame graph's root bar has the room)
    expect(rootTitle(polarScene({ info: { breadcrumb: [{ key: 1, label: "/home/lenny/" }] } }).frame, fmtInt).line1).toBe("/home/lenny");
  });

  it("writes an aggregate zoomed into, and a root waiting for the scan", () => {
    const small = polarScene({ info: { breadcrumb: [{ key: 1, label: "root" }, { key: SMALL_KEY, label: "small:37" }] } }).frame;
    expect(rowTitle(small, HIT_ROOT, o)).toEqual({ line1: "#37 个小项", line2: null });
    const more = polarScene({ info: { breadcrumb: [{ key: 1, label: "root" }, { key: 0x80000003, label: "more:5" }] } }).frame;
    expect(rowTitle(more, HIT_ROOT, o).line1).toBe("+#5 项");
    const waiting = polarScene();
    const f2 = { ...waiting.frame, info: { ...waiting.info, root: { ...waiting.info.root!, kind: Kind.PENDING } } };
    expect(rowTitle(f2, HIT_ROOT, o)).toEqual({ line1: "root", line2: "等待扫描…" });
  });

  it("is empty for the root of a frame without crumbs", () => {
    const p = polarScene({ info: { breadcrumb: [] } }).frame;
    expect(rowTitle(p, HIT_ROOT, o)).toEqual({ line1: null, line2: null });
  });
});

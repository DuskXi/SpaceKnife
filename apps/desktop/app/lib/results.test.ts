import { describe, expect, it } from "vitest";

import type { FilterNormal, ResultRow } from "@/protocol/generated";

import { capsules, describeRow, extClasses, OVERSCAN, PAGE, pagesFor, pagesReady, rangeAt, renderedRows, ROW_H, rowAt, scrollToRow, timeSpan } from "./results";

const row = (patch: Partial<ResultRow> = {}): ResultRow => ({ alloc: 1024, count: 1, key: 7, kind: "file", name: "a.MP4", node: 3, path: "Videos", size: 1000, tag: null, tag_inherited: false, ...patch });

describe("result list paging", () => {
  it("turns a scroll position into the visible rows", () => {
    expect(rangeAt(0, 500)).toEqual({ first: 0, count: Math.ceil(500 / ROW_H) + 1 });
    expect(rangeAt(ROW_H * 120 + 10, ROW_H * 4)).toEqual({ first: 120, count: 5 });
  });

  it("asks for the pages under the range and its overscan", () => {
    expect(pagesFor({ first: 0, count: 20 })).toEqual([0]);
    // the overscan reaches back into the previous page and on into the next
    expect(pagesFor({ first: PAGE + 2, count: 10 })).toEqual([0, 1]);
    expect(pagesFor({ first: 2 * PAGE - 10, count: 10 })).toEqual([1, 2]);
  });

  it("renders the range with overscan, within the total", () => {
    expect(renderedRows({ first: 0, count: 10 }, 250)).toEqual([0, 10 + OVERSCAN]);
    expect(renderedRows({ first: 50, count: 10 }, 55)).toEqual([50 - OVERSCAN, 55]);
    expect(renderedRows({ first: 0, count: 10 }, 0)).toEqual([0, 0]);
  });

  it("publishes once every needed page is there or past the end", () => {
    const pages = new Map([[0, [row()]]]);
    expect(pagesReady([0, 1], pages, 250)).toBe(false);
    expect(pagesReady([0, 1], pages, 100)).toBe(true); // page 1 starts at the end
    expect(rowAt(pages, 0)?.key).toBe(7);
    expect(rowAt(pages, 1)).toBeUndefined();
    expect(rowAt(undefined, 0)).toBeUndefined();
  });

  it("scrolls a row into view only when it is out of view", () => {
    expect(scrollToRow(5, 0, 10 * ROW_H)).toBeNull();
    expect(scrollToRow(2, 5 * ROW_H, 10 * ROW_H)).toBe(2 * ROW_H);
    expect(scrollToRow(20, 0, 10 * ROW_H)).toBe(21 * ROW_H - 10 * ROW_H);
  });
});

describe("result rows", () => {
  const ext = extClasses([
    { id: "video", extensions: ["mp4", "mkv"] },
    { id: "other-video", extensions: ["mp4"] },
  ]);

  it("maps an extension to the first class naming it", () => {
    expect(ext.get("mp4")).toBe("video");
    expect(extClasses(undefined).size).toBe(0);
  });

  it("describes a file by its class and a group by its count", () => {
    const f = describeRow(row(), "C:", ext);
    expect(f).toMatchObject({ cls: "video", name: "a.MP4", path: "Videos" });
    expect(f.spoken).toContain("位于 Videos");
    const g = describeRow(row({ kind: "group", name: "", count: 12 }), "C:", ext);
    expect(g.cls).toBeNull();
    expect(g.name).toBe("12 个小项");
    expect(describeRow(row({ kind: "rest", count: 3 }), "C:", ext).name).toBe("其余 3 项");
    // the scan root is named by the breadcrumb's first label
    expect(describeRow(row({ kind: "folder", name: "\\", path: "" }), "C:", ext)).toMatchObject({ name: "C:", path: "C:" });
  });

  it("says where a tag comes from", () => {
    expect(describeRow(row({ tag: "red" }), "", ext).tag).toBe("红色标签");
    expect(describeRow(row({ tag: "blue", tag_inherited: true }), "", ext).tag).toBe("蓝色标签（继承自文件夹）");
  });
});

describe("condition capsules", () => {
  const empty: FilterNormal = {
    masks: [],
    not_masks: [],
    folders: [],
    not_folders: [],
    classes: [],
    not_classes: [],
    sizes: [],
    ages: [],
    tags: null,
    not_tags: [],
    attrs: null,
    not_attrs: [],
  } as unknown as FilterNormal;

  it("lists names with 或 and sizes with 且", () => {
    const caps = capsules({ ...empty, masks: ["*.mp4", "*.mkv"], classes: ["video"], sizes: [{ field: "alloc", lower: { op: ">", number: "1", unit: "gb" }, upper: null }] } as unknown as FilterNormal, (id) => (id === "video" ? "视频" : id));
    expect(caps).toEqual([
      { label: "名称", joiner: "或", values: [{ text: "*.mp4", cls: "t-mask" }, { text: "*.mkv", cls: "t-mask" }] },
      { label: "类别", joiner: "或", values: [{ text: "视频", cls: "t-value" }] },
      { label: "大小", joiner: "且", values: [{ text: "> 1 GB", cls: "t-compare" }] },
    ]);
  });

  it("says ages in words and all four tags as 任意", () => {
    const caps = capsules({ ...empty, ages: [{ field: "modify", lower: null, upper: { op: "<=", number: "7", unit: "d" } }], tags: ["red", "yellow", "green", "blue"] } as unknown as FilterNormal, (id) => id);
    expect(caps[0]).toEqual({ label: "修改时间", joiner: "且", values: [{ text: "7 天以内（含）", cls: "t-compare" }] });
    expect(caps[1]!.values).toEqual([{ text: "任意", cls: "t-keyword" }]);
    expect(timeSpan("3", "month")).toBe("3 个月");
    expect(timeSpan("3", "fortnight")).toBe("3 fortnight");
  });
});

import { afterEach, describe, expect, it } from "vitest";

import type { FilterVocab } from "@/protocol/generated";
import { branchFill } from "@/visual/colors";
import { Kind } from "@/visual/frame";
import { encodeFrame, type FixtureRow } from "@/visual/testing/fixtures";
import { HIT_ROOT } from "@/visual/types";

import { colorOf } from "./classes";
import { colorLegend, colorMeaning, cssColor, LEGEND_BRANCHES, legendSettings, TIME_BANDS } from "./colorLegend";
import { setLang } from "./i18n";
import { palette } from "./theme";

afterEach(() => setLang("zh-CN"));

const vocab = {
  classes: [
    { id: "image", name: "图片", color: "", color_light: "" },
    { id: "video", name: "视频", color: "#123456", color_light: "#654321" },
    { id: "other", name: "其他", color: "", color_light: "" },
  ],
} as unknown as FilterVocab;

const row = (r: Partial<FixtureRow> & Pick<FixtureRow, "key" | "parent" | "kind">): FixtureRow => ({ node: r.key, depth: 1, geo: [0, 0, 10, 10], size: 10n, ...r });

/** A picture with ten first-level folders (branch slots 1–8, two used twice), a first-level
 * aggregate, and inside 「照片」 a file, an aggregate of mostly images and one of mixed classes. */
const scene = () =>
  encodeFrame("drawlist.v1", [
    ...Array.from({ length: 10 }, (_, i) => row({ key: i + 1, parent: -1, kind: Kind.FOLDER, size: BigInt(1000 - i), branch: (i % 8) + 1, name: i === 0 ? "照片" : `d${i}` })),
    row({ key: 11, parent: -1, kind: Kind.SMALL, size: 5n, count: 30 }),
    row({ key: 12, parent: 0, kind: Kind.FILE, depth: 2, cls: 0, tone: 2, branch: 1, name: "a.jpg" }),
    row({ key: 13, parent: 0, kind: Kind.SMALL, depth: 2, cls: 0, tone: 4, branch: 1, count: 466 }),
    row({ key: 14, parent: 0, kind: Kind.SMALL, depth: 2, cls: 255, tone: 0, branch: 1, count: 3 }),
  ]).frame;

describe("colour legend", () => {
  it("lists every class in its colour, and the folded blocks", () => {
    const l = colorLegend("class", vocab, null);
    expect(l.rows.map((r) => r.label)).toEqual(["图片", "视频", "其他", "折叠的文件夹、「+N 项」"]);
    expect(l.rows.slice(0, 3).map((r) => r.color)).toEqual(vocab.classes.map(colorOf));
    expect(l.rows[3]!.color).toBe(palette().treemap.collapsed);
    expect(l.settings).toBe("classes");
  });

  it("gives the time bands their colours, as many bands as the theme has colours", () => {
    const T = palette().treemap.time as Record<string, string>;
    expect(TIME_BANDS).toHaveLength(Object.keys(T).filter((k) => /^\d+$/.test(k)).length);
    const l = colorLegend("time", vocab, null);
    expect(l.rows.slice(0, 5)).toEqual(TIME_BANDS.map((b, i) => ({ color: T[String(i)], label: b() })));
    expect(l.rows.map((r) => r.label)).toEqual(["1 天内", "1 天 – 1 周", "1 周 – 1 个月", "1 个月 – 1 年", "1 年以上", "折叠的文件夹、「+N 项」"]);
    expect(l.settings).toBe("appearance");
  });

  it("says flat colours only compare sizes", () => {
    const l = colorLegend("flat", vocab, null);
    expect(l.rows[0]!.color).toBe(palette().treemap.flat.file);
    expect(l.lead).toBe("所有文件同一种颜色，只比较大小");
  });

  it("names the largest branches in view in their colours and counts the rest", () => {
    const l = colorLegend("branch", vocab, scene());
    expect(l.rows).toHaveLength(LEGEND_BRANCHES);
    expect(l.rows[0]).toEqual({ color: cssColor(branchFill(1, 2)), label: "照片" });
    expect(l.rows.map((r) => r.label)).toEqual(["照片", "d1", "d2", "d3", "d4", "d5", "d6", "d7"]);
    expect(l.more).toBe(2);
    // without a picture: nothing to name yet
    const empty = colorLegend("branch", vocab, null);
    expect([empty.rows, empty.more]).toEqual([[], 0]);
    expect(empty.notes[0]).toContain("顶层分支");
  });

  it("sends a right-click to where the mode's colours are set", () => {
    expect(["class", "flat", "time", "branch"].map((m) => legendSettings(m as "class"))).toEqual(["classes", "appearance", "appearance", "appearance"]);
  });
});

describe("what a block's colour means", () => {
  const f = scene();

  it("names the class of an aggregate, or says it is mixed", () => {
    expect(colorMeaning("class", f, 12, vocab)).toBe("大部分是「图片」");
    expect(colorMeaning("class", f, 13, vocab)).toBe("多种类别混合，没有一类达到一半");
    // a file's class is on its own line already; without the classes nothing is said
    expect(colorMeaning("class", f, 11, vocab)).toBeNull();
    expect(colorMeaning("class", f, 12, null)).toBeNull();
  });

  it("gives the time band of a file, and of an aggregate by most of its space", () => {
    expect(colorMeaning("time", f, 11, vocab)).toBe("距上次修改：1 周 – 1 个月");
    expect(colorMeaning("time", f, 12, vocab)).toBe("距上次修改：1 年以上（占空间最多的一档）");
    expect(colorMeaning("time", f, 0, vocab)).toBeNull();
    setLang("en");
    expect(colorMeaning("time", f, 11, vocab)).toBe("Since last modified: 1 week – 1 month");
  });

  it("names the branch a block belongs to", () => {
    expect(colorMeaning("branch", f, 11, vocab)).toBe("分支：照片");
    expect(colorMeaning("branch", f, 3, vocab)).toBe("分支：d3");
    expect(colorMeaning("branch", f, 10, vocab)).toBeNull(); // a first-level aggregate is neutral
  });

  it("says nothing for flat colours, the zoom root, or a comparison", () => {
    expect([11, 12, 0].map((r) => colorMeaning("flat", f, r, vocab))).toEqual([null, null, null]);
    expect(colorMeaning("time", f, HIT_ROOT, vocab)).toBeNull();
    const cmp = encodeFrame("drawlist.v1", [row({ key: 1, parent: -1, kind: Kind.FILE, tone: 1, delta: 5n, dstat: 3 })]).frame;
    expect(colorMeaning("time", cmp, 0, vocab)).toBeNull();
  });
});

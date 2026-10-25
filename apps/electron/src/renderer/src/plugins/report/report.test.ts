// Report documents' pure parts: every unit formatted as the app's own panels do, sorting and paging
// tables, shares for bars, the refresh rules, the Markdown subset (no raw HTML, no images, no links
// that navigate).

import { describe, expect, it } from "vitest";

import { formatCell, isEmptyReport, nextSort, numericUnit, PAGE_MAX, pageOf, share, sortRows } from "./format";
import { renderReportMd } from "./md";
import { intervalOf, settingsKey, statesOf } from "./useReport";

describe("formatCell", () => {
  it("formats each unit", () => {
    expect(formatCell(1536, "size")).toBe("1.5 KB");
    expect(formatCell(1234567, "count")).toBe("1,234,567");
    expect(formatCell(0.256, "pct")).toBe("26%");
    expect(formatCell(0.0123, "pct")).toBe("1.2%");
    expect(formatCell(90, "duration")).toMatch(/1/);
    expect(formatCell(2048, "bytes_per_s")).toBe("2.0 KB/s");
    expect(formatCell("a/b", "text")).toBe("a/b");
  });

  it("shows a missing value as a dash, booleans in words, text it can't read as is", () => {
    expect(formatCell(null, "size")).toBe("—");
    expect(formatCell(undefined, "count")).toBe("—");
    expect(formatCell("", "text")).toBe("—");
    expect(formatCell(true, "text")).toBe("是");
    expect(formatCell("n/a", "size")).toBe("n/a");
    expect(formatCell("2048", "size")).toBe("2.0 KB"); // a number sent as text
  });

  it("shortens long paths in the middle", () => {
    const long = `/home/user/projects/a/b/${"x".repeat(60)}/file.txt`;
    const out = formatCell(long, "path");
    expect(out.length).toBeLessThan(long.length);
    expect(out.endsWith("file.txt")).toBe(true);
  });

  it("right-aligns numbers, not text or paths", () => {
    expect(numericUnit("size")).toBe(true);
    expect(numericUnit("ago")).toBe(true);
    expect(numericUnit("text")).toBe(false);
    expect(numericUnit("path")).toBe(false);
  });
});

describe("tables", () => {
  const rows = [{ cells: { n: 3, s: "b" } }, { cells: { n: null, s: "a" } }, { cells: { n: 10, s: "C" } }, { cells: { n: 3, s: "d" } }];

  it("sorts numbers as numbers and text without case, missing values last, stably", () => {
    expect(sortRows(rows, "n").map((r) => r.cells.s)).toEqual(["b", "d", "C", "a"]);
    expect(sortRows(rows, "-n").map((r) => r.cells.s)).toEqual(["C", "b", "d", "a"]);
    expect(sortRows(rows, "s").map((r) => r.cells.s)).toEqual(["a", "b", "C", "d"]);
    expect(sortRows(rows, null)).toEqual(rows);
  });

  it("cycles a header click: numbers largest first, text A first, then back", () => {
    expect(nextSort(null, "n", true)).toBe("-n");
    expect(nextSort("-n", "n", true)).toBe("n");
    expect(nextSort("n", "n", true)).toBeNull();
    expect(nextSort(null, "s", false)).toBe("s");
    expect(nextSort("-n", "s", false)).toBe("s");
  });

  it("pages at most 50 rows, clamping the page", () => {
    const many = Array.from({ length: 120 }, (_, i) => i);
    expect(pageOf(many, 0, 500).rows).toHaveLength(PAGE_MAX);
    expect(pageOf(many, 2, 50)).toMatchObject({ pages: 3, page: 2 });
    expect(pageOf(many, 2, 50).rows).toEqual(many.slice(100));
    expect(pageOf(many, 9, 50).page).toBe(2);
    expect(pageOf([], 0, 50)).toMatchObject({ pages: 1, page: 0, rows: [] });
  });

  it("bars share their maximum, clamped", () => {
    expect(share(5, 10)).toBe(0.5);
    expect(share(20, 10)).toBe(1);
    expect(share(-1, 10)).toBe(0);
    expect(share(1, 0)).toBe(0);
    expect(share(Number.NaN, 10)).toBe(0);
  });

  it("knows an empty report", () => {
    expect(isEmptyReport({ blocks: [] })).toBe(true);
    expect(isEmptyReport({ blocks: [{ type: "md", text: "x" }] })).toBe(false);
  });
});

describe("refresh", () => {
  it("reads interval:<seconds>, at least five", () => {
    expect(intervalOf(["open", "interval:30"])).toBe(30_000);
    expect(intervalOf(["interval:1"])).toBe(5_000);
    expect(intervalOf(["open"])).toBeNull();
  });

  it("reads the states it follows", () => {
    expect(statesOf(["state:status", "scan", "state:"])).toEqual(["status"]);
  });

  it("follows its own plugin's settings only (refresh = settings)", () => {
    const values = { "plugin.a.b.bands": "7d", "plugin.a.b.keep": 2, "plugin.a.bc.x": 1, "appearance.theme": "dark" };
    const key = settingsKey(values, "a.b");
    expect(key).toBe(JSON.stringify([["plugin.a.b.bands", "7d"], ["plugin.a.b.keep", 2]]));
    expect(settingsKey({ ...values, "appearance.theme": "light", "plugin.a.bc.x": 9 }, "a.b")).toBe(key);
    expect(settingsKey({ ...values, "plugin.a.b.bands": "30d" }, "a.b")).not.toBe(key);
    expect(settingsKey(undefined, "a.b")).toBe("");
  });
});

describe("Markdown", () => {
  it("keeps markup as text, shows no images and no links that navigate", () => {
    const html = renderReportMd('**bold** <script>alert(1)</script> ![x](http://evil/x.png) [site](https://example.invalid/a"b)');
    expect(html).toContain("<strong>bold</strong>");
    expect(html).not.toContain("<script");
    expect(html).toContain("&lt;script&gt;");
    expect(html).not.toContain("<img");
    expect(html).not.toContain("<a ");
    expect(html).toContain('class="md-link"');
  });
});

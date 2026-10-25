// Round 4 (docs/plugins.md 「第三轮之后的改进」): a scan from a plugin's source is known as such in
// every window (its status says so), a report's path cell keeps the file name and gives up folders
// by width, and plugin shot's steps find the page's development hooks.

import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";

import type { ScanStatus } from "@/protocol/generated";
import { set } from "@/state/store";
import { devHooks } from "@/state/devlink";

import { PathText } from "./report/ReportView";
import { scanSourceOf, sourceScanOf } from "./sources";

afterEach(() => set({ scans: {} }));

describe("scan sources", () => {
  it("knows a source's scan from its status, not only the ones this window started", () => {
    const status = { scan_id: "scan-1", state: "done", roots: [], plugin_source: "x.ncdu.import", uri: "/tmp/a.json" };
    set({ scans: { "scan-1": status as unknown as ScanStatus } });
    expect(scanSourceOf("scan-1")).toBe("x.ncdu.import");
    expect(sourceScanOf("scan-1")).toEqual({ source: "x.ncdu.import", uri: "/tmp/a.json" });
    expect(scanSourceOf("scan-2")).toBeNull(); // a disk scan, or one this window never heard of
  });
});

describe("report path cells", () => {
  it("keep the name whole and let the folders give way", () => {
    const html = renderToString(createElement(PathText, { path: "C:\\Users\\lenny\\Pictures\\2021\\holiday.jpg" }));
    expect(html).toContain('class="report-path-head">C:\\Users\\lenny\\Pictures\\2021\\</span>');
    expect(html).toContain('class="report-path-tail">holiday.jpg</span>');
    const bare = renderToString(createElement(PathText, { path: "notes.txt" }));
    expect(bare).not.toContain("report-path-head");
    expect(renderToString(createElement(PathText, { path: "/srv/data/" }))).toContain('report-path-tail">/srv/data/</span>');
  });
});

describe("plugin shot's steps", () => {
  it("have hooks to select, run and open", () => {
    const hooks = devHooks({ scan_id: "scan-1", roots: [{ path: "/data" }] } as unknown as ScanStatus);
    expect(Object.keys(hooks).sort()).toEqual(["open", "run", "select", "where"]);
  });
});

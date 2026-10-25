import { describe, expect, it } from "vitest";

import { joinRoot, parseDevLink } from "./devlink";

describe("the development link's words", () => {
  it("reads theme, language, a node to select, commands and what to open", () => {
    const link = parseDevLink("#ws=x&token=y&scan=s1&theme=light&lang=en&select=logs/app.log&do=view.visual.a.b.w,report.export&open=report:a.b.r");
    expect(link).toEqual({ theme: "light", lang: "en", select: "logs/app.log", run: ["view.visual.a.b.w", "report.export"], open: "report:a.b.r" });
  });

  it("leaves out what it does not know", () => {
    expect(parseDevLink("#ws=x&theme=sepia&lang=zh")).toEqual({ theme: null, lang: "zh-CN", select: null, run: [], open: null });
  });

  it("joins a relative path to the root with the root's separator", () => {
    expect(joinRoot("/home/lenny/data", "logs/app.log")).toBe("/home/lenny/data/logs/app.log");
    expect(joinRoot("C:\\Users\\alice\\data\\", "logs/app.log")).toBe("C:\\Users\\alice\\data\\logs\\app.log");
    expect(joinRoot("/data", "")).toBe("/data");
  });
});

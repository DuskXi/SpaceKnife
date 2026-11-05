import { afterEach, describe, expect, it } from "vitest";

import type { FilterDiagnostic } from "@/protocol/generated";

import { attrLabel, colourSegments, diagnosticText, errorText, unitLabel } from "./filterText";
import { setLang } from "./i18n";

afterEach(() => setLang("zh-CN"));

const diag = (patch: Partial<FilterDiagnostic>): FilterDiagnostic =>
  ({ code: "unknown_unit", severity: "error", start: 0, end: 0, cond_start: 0, cond_end: 0, expected: [], text: "", ...patch }) as FilterDiagnostic;

describe("filter words", () => {
  it("names attributes, units and errors, unknown ones as they are", () => {
    expect(attrLabel("hidden")).toBe("隐藏");
    expect(attrLabel("weird")).toBe("weird");
    expect(unitLabel("gb")).toBe("GB");
    expect(unitLabel("parsec")).toBe("");
    expect(errorText("nope")).toBe("nope");
    setLang("en");
    expect(attrLabel("hidden")).not.toBe("隐藏");
  });

  it("lists up to eight accepted spellings after the message", () => {
    expect(diagnosticText(diag({ expected: [] }))).toBe("无法识别的单位，可用");
    const many = "abcdefghij".split("");
    expect(diagnosticText(diag({ expected: many }))).toBe("无法识别的单位，可用 a/b/c/d/e/f/g/h");
  });
});

describe("colourSegments", () => {
  it("leaves text plain without its own parse", () => {
    expect(colourSegments("", null)).toEqual([]);
    expect(colourSegments("abc", null)).toEqual([{ start: 0, text: "abc", cls: null }]);
    expect(colourSegments("abc", { text: "ab", tokens: [], diagnostics: [] })).toEqual([{ start: 0, text: "abc", cls: null }]);
  });

  it("joins runs of one class and underlines errors", () => {
    const text = "*.mp4; >1zz";
    const segs = colourSegments(text, {
      text,
      tokens: [
        { kind: "mask", start: 0, end: 5 },
        { kind: "separator", start: 5, end: 6 },
        { kind: "compare", start: 7, end: 11 },
      ],
      diagnostics: [diag({ start: 9, end: 11 }), diag({ severity: "warning", start: 0, end: 2 })],
      pending: null,
    });
    expect(segs).toEqual([
      { start: 0, text: "*.mp4", cls: "t-mask" },
      { start: 5, text: ";", cls: "t-separator" },
      { start: 6, text: " ", cls: "" },
      { start: 7, text: ">1", cls: "t-compare" },
      { start: 9, text: "zz", cls: "t-compare t-error" },
    ]);
    expect(segs.map((s) => s.text).join("")).toBe(text);
  });

  it("marks the condition being typed, and an empty error span takes one character", () => {
    const segs = colourSegments("ab", { text: "ab", tokens: [], diagnostics: [diag({ start: 1, end: 1 })], pending: { start: 0, end: 1 } as never });
    expect(segs).toEqual([
      { start: 0, text: "a", cls: " t-pending" },
      { start: 1, text: "b", cls: " t-error" },
    ]);
  });
});

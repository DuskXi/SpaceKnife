// The settings page's list control: new entries at their defaults, moving entries, the numbers a
// field takes (the backend checks every change again).

import { describe, expect, it } from "vitest";

import type { PaletteCommand, SettingField } from "@/protocol/generated";
import { unavailable } from "@/plugins/menuStates";

import { blankEntry, moved, numberProblem } from "./SettingList";

const field = (over: Partial<SettingField>): SettingField => ({ name: "days", title: "天数", description: "", control: "number", default: 30, options: [], min: 1, max: 3650, step: null, unit: "", placeholder: "", max_length: null, ...over });

describe("list settings", () => {
  it("a new entry has every field at its default", () => {
    expect(blankEntry([field({}), field({ name: "glob", control: "text", default: "*" }), field({ name: "on", control: "switch", default: true })])).toEqual({ days: 30, glob: "*", on: true });
  });

  it("moves an entry up or down, not past either end", () => {
    expect(moved(["a", "b", "c"], 1, -1)).toEqual(["b", "a", "c"]);
    expect(moved(["a", "b", "c"], 1, 1)).toEqual(["a", "c", "b"]);
    expect(moved(["a", "b"], 0, -1)).toEqual(["a", "b"]);
    expect(moved(["a", "b"], 1, 1)).toEqual(["a", "b"]);
  });

  it("a number field takes numbers in its range, whole ones when its default is", () => {
    const f = field({});
    expect(numberProblem(f, "7")).toBe("");
    expect(numberProblem(f, "0")).toBe("请输入 1–3650 的整数");
    expect(numberProblem(f, "2.5")).toBe("请输入 1–3650 的整数");
    expect(numberProblem(f, "")).not.toBe("");
    expect(numberProblem(field({ default: 0.5, min: 0, max: 1 }), "0.25")).toBe("");
    expect(numberProblem(field({ default: 0.5, min: 0, max: 1 }), "2")).toBe("请输入 0–1 的数");
  });
});

describe("commands greyed in menus", () => {
  const cmd = (id: string, enabled: boolean, status = ""): PaletteCommand => ({ id, name: id, enabled, status, hits: [], icon: "", keys: [], more: false, action: null, menus: ["node"], plugin: "a.b" });
  it("says why one can't run there; nothing before the answer or when it can", () => {
    const states = new Map([cmd("a.b.only_files", false, "只用于文件"), cmd("a.b.ok", true), cmd("a.b.quiet", false)].map((c) => [c.id, c]));
    expect(unavailable(states, "a.b.only_files")).toBe("只用于文件");
    expect(unavailable(states, "a.b.ok")).toBe("");
    expect(unavailable(states, "a.b.quiet")).toBe("当前不可用");
    expect(unavailable(null, "a.b.only_files")).toBe("");
  });
});

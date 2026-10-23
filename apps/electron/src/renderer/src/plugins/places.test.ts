// Where a plugin's page is and what it is called: a panel's context names its place and params, a
// plugin tab is titled as its report or panel is now (a language switch renames it), labels of a
// plugin's weights are shares, and a plugin's clipboard text is copied only with the permission.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { get, set } from "@/state/store";
import { shareText, weightTotal } from "@/visual/Host";

import { panelContext } from "./panel/PanelFrame";
import { pluginTabTitle } from "./slots";
import { onClipboard } from "./store";

const ctx = { scan_id: "s1", view_id: "v1", node: 3, selection: [3], compare_id: null };

describe("panelContext", () => {
  it("says where the page is and gives it its params", () => {
    expect(panelContext(ctx, { slot: "tab", params: { year: 2020 } }, undefined)).toEqual({ ...ctx, slot: "tab", params: { year: 2020 } });
    expect(panelContext(ctx, { slot: "tab", params: {} }, "inspector")).toMatchObject({ slot: "inspector", params: {} });
  });
});

describe("pluginTabTitle", () => {
  const tab = (kind: "report" | "panel", id: string) => ({ id: "t", kind: "plugin", scanId: null, viewIds: [], focusedPane: 0, title: "旧标题", plugin: { kind, id } }) as never;
  it("follows the registry (its language), a panel's short title first", () => {
    const extensions = {
      reports: [{ id: "a.b.r", title: "Duplicate files" }],
      panels: [
        { id: "a.b.p", title: "Growth between snapshots", short_title: "Growth" },
        { id: "a.b.q", title: "Board", short_title: "" },
      ],
    } as never;
    expect(pluginTabTitle(tab("report", "a.b.r"), { extensions })).toBe("Duplicate files");
    expect(pluginTabTitle(tab("panel", "a.b.p"), { extensions })).toBe("Growth");
    expect(pluginTabTitle(tab("panel", "a.b.q"), { extensions })).toBe("Board");
    expect(pluginTabTitle(tab("report", "a.b.gone"), { extensions })).toBe("旧标题");
    expect(pluginTabTitle(tab("report", "a.b.r"), { extensions: null })).toBe("旧标题");
  });
});

describe("weights", () => {
  it("label a row by its share of the root, not as bytes", () => {
    const frame = { info: { root: { size: 400 } }, size: [100, 3, 0] };
    expect(weightTotal(frame)).toBe(400);
    expect(shareText(100, 400)).toBe("25%");
    expect(shareText(3, 400)).toBe("0.8%");
    expect(shareText(0.1, 400)).toBe("<0.1%");
    expect(shareText(1, 0)).toBe("—");
    expect(weightTotal({ info: { root: null }, size: [5, 9n, 2] })).toBe(9);
  });
});

describe("onClipboard", () => {
  const copyText = vi.fn();
  beforeEach(() => {
    vi.stubGlobal("window", { sk: { shell: { copyText } } });
    set({ toast: null, extensions: null, plugins: { plugins: [{ id: "a.b", name: "A", granted: ["clipboard"] }, { id: "c.d", name: "C", granted: [] }], folder: "", dev_mode: false } as never });
  });
  afterEach(() => {
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });
  it("copies for a plugin with the permission and says who did", () => {
    onClipboard({ plugin: "a.b", text: "hello" });
    expect(copyText).toHaveBeenCalledWith("hello");
    expect(get().toast).toBe("A 复制了文字");
    onClipboard({ plugin: "c.d", text: "no" });
    expect(copyText).toHaveBeenCalledTimes(1);
  });
});

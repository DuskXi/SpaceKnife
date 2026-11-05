// @vitest-environment happy-dom
// The filter box: the colouring mirror, the completion panel under the arrow keys and Tab, and Esc
// closing the panel, then clearing, then leaving the box.

import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";

import type { FilterParse } from "@/protocol/generated";
import { type AppState, resetState, set } from "@/state/store";

const editFilter = vi.fn(async (_view: string, _text: string, _cursor: number | null, _now?: boolean) => {});
vi.mock("@/state/actions", async (orig) => ({
  ...(await orig<typeof import("@/state/actions")>()),
  editFilter: (v: string, text: string, cursor: number | null, now?: boolean) => editFilter(v, text, cursor, now),
}));

const { default: FilterBox } = await import("./FilterBox.vue");

function view(text: string, parse: Partial<FilterParse> | null) {
  const p = parse ? ({ text, tokens: [], diagnostics: [], completion: null, conditions: [], normal: null, pending: null, ...parse } as FilterParse) : null;
  set({ views: { v1: { filterText: text, parse: p } } as unknown as AppState["views"] });
}

describe("FilterBox", () => {
  beforeEach(() => {
    resetState();
    editFilter.mockClear();
  });

  it("colours the text as parsed", async () => {
    view("*.mp4", { tokens: [{ kind: "mask", start: 0, end: 5 }] });
    const w = mount(FilterBox, { props: { viewId: "v1" } });
    await nextTick();
    expect(w.find(".filter-mirror span.t-mask").text()).toBe("*.mp4");
    expect((w.find("input").element as HTMLInputElement).value).toBe("*.mp4");
    w.unmount();
  });

  it("picks a completion with the arrows and Tab, keeping those keys to itself", async () => {
    view(":cl", { completion: { context: "keyword", start: 0, end: 3, typed: ":cl", candidates: [":class:", ":clear"] } as FilterParse["completion"] });
    const w = mount(FilterBox, { props: { viewId: "v1" }, attachTo: document.body });
    const input = w.find("input");
    await input.trigger("focus");
    expect(w.findAll(".panel-row").map((r) => r.find(".cand").text())).toEqual([":class:", ":clear"]);
    const onWindow = vi.fn();
    window.addEventListener("keydown", onWindow);
    await input.trigger("keydown", { key: "ArrowDown" });
    expect(w.find(".panel-row.on .cand").text()).toBe(":clear");
    await input.trigger("keydown", { key: "Tab" });
    expect((input.element as HTMLInputElement).value).toBe(":clear");
    expect(editFilter).toHaveBeenLastCalledWith("v1", ":clear", 6, true);
    window.removeEventListener("keydown", onWindow);
    expect(onWindow).not.toHaveBeenCalled();
    w.unmount();
  });

  it("closes the panel on Esc, then clears the text", async () => {
    view("x", { diagnostics: [{ code: "incomplete", severity: "error", start: 0, end: 1, cond_start: 0, cond_end: 1, expected: [], text: "x" }] });
    const w = mount(FilterBox, { props: { viewId: "v1" }, attachTo: document.body });
    const input = w.find("input");
    await input.trigger("focus");
    expect(w.find(".panel-error").text()).toContain("条件还没写完");
    await input.trigger("keydown", { key: "Escape" });
    expect(w.find(".filter-panel").exists()).toBe(false);
    expect(editFilter).not.toHaveBeenCalled();
    await input.trigger("keydown", { key: "Escape" });
    expect(editFilter).toHaveBeenLastCalledWith("v1", "", 0, true);
    w.unmount();
  });

  it("is disabled without a scan", () => {
    const w = mount(FilterBox, { props: { viewId: null, disabled: true } });
    expect(w.find("input").attributes("disabled")).toBeDefined();
    expect(w.find("input").attributes("placeholder")).toBe("开始扫描后可以过滤");
    w.unmount();
  });
});

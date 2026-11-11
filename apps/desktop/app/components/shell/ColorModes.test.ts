// @vitest-environment happy-dom
// The toolbar's colour modes: a hovered or focused mode shows its legend (after a pause the first
// time, at once when moving on to the next mode), a right-click opens where its colours are set,
// a click picks it.

import { mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";

import type { FilterVocab, ViewState } from "@/protocol/generated";
import { type AppState, resetState, set } from "@/state/store";

const openSettingsTab = vi.fn((_page?: string) => {});
const setViewSettings = vi.fn((_view: string, _patch: object) => {});
vi.mock("@/state/actions", async (orig) => ({
  ...(await orig<typeof import("@/state/actions")>()),
  openSettingsTab: (page?: string) => openSettingsTab(page),
  setViewSettings: (view: string, patch: object) => setViewSettings(view, patch),
}));

const { default: ColorModes } = await import("./ColorModes.vue");

const vocab = { classes: [{ id: "image", name: "图片", color: "", color_light: "" }] } as unknown as FilterVocab;

let w: VueWrapper | null = null;
const mode = (i: number) => w!.findAll("button")[i]!; // 类别 扁平 时间 分支
const tip = () => w!.find(".legend-tip");

describe("ColorModes", () => {
  beforeEach(() => {
    resetState();
    set({ vocab, views: { v1: { frame: null } } as unknown as AppState["views"] });
    openSettingsTab.mockClear();
    setViewSettings.mockClear();
    vi.useFakeTimers();
    w = mount(ColorModes, { props: { viewId: "v1", st: { color_mode: "class" } as ViewState }, attachTo: document.body });
  });

  afterEach(() => {
    w?.unmount();
    vi.useRealTimers();
  });

  it("shows the hovered mode's legend after a pause, follows the pointer to the next mode, and goes", async () => {
    await mode(2).trigger("mouseenter");
    vi.advanceTimersByTime(300);
    await nextTick();
    expect(tip().exists()).toBe(false);
    vi.advanceTimersByTime(60);
    await nextTick();
    expect(tip().find(".lt-title").text()).toBe("时间配色");
    expect(tip().findAll(".lt-row").map((r) => r.text())).toContain("1 周 – 1 个月");
    expect(mode(2).attributes("aria-describedby")).toBe("sk-color-legend");
    // on to the next mode: the legend changes at once
    await mode(2).trigger("mouseleave");
    await mode(0).trigger("mouseenter");
    await nextTick();
    expect(tip().find(".lt-title").text()).toBe("类别配色");
    expect(tip().text()).toContain("右键打开「文件类别」设置");
    // away: gone after a moment
    await mode(0).trigger("mouseleave");
    vi.advanceTimersByTime(200);
    await nextTick();
    expect(tip().exists()).toBe(false);
  });

  it("shows the legend at once for the keyboard, and Esc closes it", async () => {
    await mode(3).trigger("focus");
    expect(tip().find(".lt-title").text()).toBe("分支配色");
    await mode(3).trigger("keydown", { key: "Escape" });
    expect(tip().exists()).toBe(false);
  });

  it("opens where a mode's colours are set on a right-click", async () => {
    await mode(1).trigger("focus");
    await mode(0).trigger("contextmenu");
    expect(openSettingsTab).toHaveBeenLastCalledWith("classes");
    expect(tip().exists()).toBe(false);
    await mode(2).trigger("contextmenu");
    expect(openSettingsTab).toHaveBeenLastCalledWith("appearance");
  });

  it("picks a mode on a click", async () => {
    await mode(2).trigger("click");
    expect(setViewSettings).toHaveBeenCalledWith("v1", { color_mode: "time" });
  });
});

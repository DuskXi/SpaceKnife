// @vitest-environment happy-dom
// The title bar renders the store's tabs and drives the window through window.sk.

import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";

import type { SkApi } from "@/shared/api";
import { setLang } from "@/lib/i18n";
import { get, resetState, set } from "@/state/store";

import TitleBar from "./TitleBar.vue";

function fakeSk(platform: SkApi["platform"] = "win32") {
  let onMax: ((m: boolean) => void) | null = null;
  const sk = {
    platform,
    host: "app",
    window: {
      minimize: vi.fn(),
      toggleMaximize: vi.fn(),
      close: vi.fn(),
      isMaximized: vi.fn(async () => false),
      onMaximized: (cb: (m: boolean) => void) => ((onMax = cb), () => (onMax = null)),
    },
  };
  return { sk: sk as unknown as SkApi, maximize: (m: boolean) => onMax?.(m) };
}

describe("TitleBar", () => {
  beforeEach(() => resetState());
  afterEach(() => {
    setLang("zh-CN");
    vi.unstubAllGlobals();
  });

  it("shows a tab per open tab and the window buttons", async () => {
    const { sk, maximize } = fakeSk();
    window.sk = sk;
    const first = get().tabs[0]!;
    set({ tabs: [first, { ...first, id: "tab-x", title: "D:\\" }] });
    const bar = mount(TitleBar);
    expect(bar.findAll("[role=tab]").map((t) => t.text())).toEqual([first.title, "D:\\"]);
    expect(bar.find("[role=tab][aria-selected=true]").text()).toBe(first.title);
    await bar.find(".window-btns .close").trigger("click");
    expect(sk.window.close).toHaveBeenCalled();
    maximize(true);
    await nextTick();
    expect(bar.find(".window-btns button:nth-child(2)").attributes("aria-label")).toBe("还原");
    // the language is reactive: the labels follow without a remount
    setLang("en");
    await nextTick();
    expect(bar.find(".window-btns .close").attributes("aria-label")).toBe("Close");
    bar.unmount();
  });

  it("leaves the window buttons to macOS", () => {
    window.sk = fakeSk("darwin").sk;
    const bar = mount(TitleBar);
    expect(bar.find("header").classes()).toContain("mac");
    expect(bar.find(".window-btns").exists()).toBe(false);
    bar.unmount();
  });
});

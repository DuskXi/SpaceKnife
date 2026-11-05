// @vitest-environment happy-dom
// The start, machine and settings pages mount from the store as it is before the backend answers,
// and the start page takes the folders dropped on the window.

import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";

import type { SkApi } from "@/shared/api";
import { activeTab, get, resetState, set, startFormOf } from "@/state/store";

import MachinePage from "./MachinePage.vue";
import SettingsPage from "./SettingsPage.vue";
import StartPage from "./StartPage.vue";

let drop: ((paths: string[]) => void) | null = null;

beforeEach(() => {
  resetState();
  drop = null;
  window.sk = {
    platform: "win32",
    host: "app",
    version: "0.0.0",
    versions: { tauri: "2", webview: "1" },
    shell: { openPath: vi.fn(), openExternal: vi.fn(), copyText: vi.fn() },
    dialog: { openFolders: vi.fn(async () => []), openSnapshot: vi.fn(async () => null) },
    files: { pathOf: () => "", onDropPaths: (cb: (p: string[]) => void) => ((drop = cb), () => (drop = null)) },
  } as unknown as SkApi;
});
afterEach(() => vi.restoreAllMocks());

describe("pages", () => {
  it("shows the start page and puts dropped folders in the path box", async () => {
    const tab = activeTab();
    const w = mount(StartPage, { props: { tab } });
    expect(w.find(".volumes-empty").exists()).toBe(true);
    expect(w.find("button.primary.big").attributes("disabled")).toBeDefined();
    drop?.(["D:\\Games", "E:\\Work"]);
    await nextTick();
    expect(startFormOf(get(), tab.id).path).toBe("D:\\Games; E:\\Work");
    expect((w.find(".path-input").element as HTMLInputElement).value).toBe("D:\\Games; E:\\Work");
    w.unmount();
    expect(drop).toBeNull();
  });

  it("shows the machine page while the disks are read", () => {
    const w = mount(MachinePage);
    expect(w.find("h1").text()).toBe("本机磁盘");
    w.unmount();
  });

  it("shows the settings nav and waits for the settings", () => {
    set({ settingsPage: "general" });
    const w = mount(SettingsPage);
    expect(w.findAll(".settings-nav-item")).toHaveLength(9);
    expect(w.find(".settings-nav-item.on").text()).toContain("常规");
    expect(w.text()).toContain("正在读取设置…");
    w.unmount();
  });
});

// @vitest-environment happy-dom
// The context menu's 「在画面中隐藏这个文件夹」 / 「不再隐藏这个文件夹」 (UI-W5-52): folders only (the zoom
// root too), after 「归类」; which of the two from view.hidden_folder; greyed with the backend's
// reason (the scan's root, MFT pseudo folders, a comparison) or while the setting can't change; a
// click changes the setting through view.hide_folder and closes the menu.

import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { HiddenFolderInfo, SettingsState } from "@/protocol/generated";
import { call, setViewSettings } from "@/state/actions";
import { type MenuState, get, resetState, set } from "@/state/store";
import { SMALL_KEY, TREEMAP_ROWS, encodeFrame } from "@/visual/testing/fixtures";

vi.mock("@/state/actions", async (orig) => ({
  ...(await orig<typeof import("@/state/actions")>()),
  call: vi.fn(),
  applySettings: vi.fn(),
  setViewSettings: vi.fn(),
}));

const { default: ContextMenu } = await import("./ContextMenu.vue");

const SETTINGS = { meta: [{ key: "treemap.hidden_folders", enabled: true, reason: "" }], values: {}, version: 1 } as unknown as SettingsState;
const forms = (raw: string) => ({ raw, quoted: `"${raw}"`, posix: raw.replace(/\\/g, "/"), uri: `file:///${raw.replace(/\\/g, "/")}`, on_disk: true });
const free = (path: string): HiddenFolderInfo => ({ listed: [], path, reason: "" });

/** What the backend answers about each node (view.hidden_folder). */
let answers: Record<number, HiddenFolderInfo> = {};
const mocked = vi.mocked(call);

function open(menu: Partial<MenuState>, host: "app" | "browser" = "app") {
  vi.stubGlobal("window", Object.assign(window, { sk: { platform: "win32", host, shell: { copyText: vi.fn() } } }));
  const { frame } = encodeFrame("drawlist.v1", TREEMAP_ROWS);
  set({
    conn: "ready",
    settings: SETTINGS,
    views: { v1: { state: { scan_id: "s1", view_id: "v1", hide_folders: true, compare: false }, frame } } as never,
    menu: { viewId: "v1", scanId: "s1", key: 3, node: 3, x: 10, y: 10, ...menu },
  });
  return mount(ContextMenu, { attachTo: document.body });
}

const labels = (w: ReturnType<typeof open>) => w.findAll("[role=menuitem]").map((b) => b.text());
const hideButton = (w: ReturnType<typeof open>) => w.findAll("[role=menuitem]").find((b) => b.text().includes("隐藏这个文件夹"));

describe("ContextMenu: hiding a folder", () => {
  beforeEach(() => {
    resetState();
    answers = { 3: free("C:\\root\\sub") };
    mocked.mockImplementation(async (method, params) => {
      const p = params as { node: number };
      if (method === "path.forms") return forms(p.node === 3 ? "C:\\root\\sub" : "C:\\root\\a.mp4") as never;
      if (method === "tree.node") return { node: { parent: 1 }, path: "C:\\root\\a.mp4" } as never;
      if (method === "view.hidden_folder") return answers[p.node] as never;
      if (method === "view.hide_folder") return SETTINGS as never;
      return {} as never;
    });
  });
  afterEach(() => {
    vi.clearAllMocks();
    vi.unstubAllGlobals();
    document.body.innerHTML = "";
  });

  it("offers hiding a folder that isn't listed, after 「归类」", async () => {
    const w = open({});
    await flushPromises();
    expect(mocked).toHaveBeenCalledWith("view.hidden_folder", { view_id: "v1", node: 3 });
    const all = labels(w);
    const at = all.findIndex((l) => l.includes("在画面中隐藏这个文件夹"));
    expect(all[at - 1]).toContain("归类");
    expect(all[at + 1]).toContain("重新扫描此文件夹");
    expect(hideButton(w)!.attributes("aria-disabled")).toBe("false");
    w.unmount();
  });

  it("offers showing it again once the list names it (the zoom root included)", async () => {
    answers[3] = { listed: ["c:\\ROOT\\sub"], path: "", reason: "" };
    const w = open({});
    await flushPromises();
    expect(hideButton(w)!.text()).toContain("不再隐藏这个文件夹");
    expect(labels(w).some((l) => l.includes("在画面中隐藏这个文件夹"))).toBe(false);
    w.unmount();
  });

  it("has no such row for a file or a small items block", async () => {
    for (const menu of [{ key: 2, node: 2 }, { key: SMALL_KEY, node: null }]) {
      const w = open(menu);
      await flushPromises();
      expect(labels(w).length).toBeGreaterThan(0);
      expect(hideButton(w)).toBeUndefined();
      w.unmount();
    }
    expect(mocked.mock.calls.filter(([m]) => m === "view.hidden_folder")).toEqual([]);
  });

  it("is greyed with the backend's reason: the scan's root, a pseudo folder, a comparison", async () => {
    for (const reason of ["扫描的根不能隐藏", "NTFS 元数据与孤立文件只在 MFT 快速扫描里可见，没有对应的位置", "对比时不可用，先退出对比"]) {
      answers[3] = { listed: [], path: "", reason };
      const w = open({});
      await flushPromises();
      const b = hideButton(w)!;
      expect(b.attributes("aria-disabled")).toBe("true");
      expect(b.attributes("title")).toBe(reason);
      await b.trigger("click");
      expect(get().toast).toBe(reason);
      expect(mocked.mock.calls.some(([m]) => m === "view.hide_folder")).toBe(false);
      w.unmount();
    }
  });

  it("is greyed for the top of a scan of several roots, which has no node", async () => {
    const w = open({ key: 1, node: null });
    await flushPromises();
    expect(hideButton(w)!.attributes("title")).toBe("扫描的根不能隐藏");
    w.unmount();
  });

  it("is greyed while the setting can't be changed (no backend)", async () => {
    const w = open({});
    set({ conn: "lost" });
    await flushPromises();
    expect(hideButton(w)!.attributes("title")).toBe("与后端的连接已断开，设置暂不可修改");
    w.unmount();
  });

  it("is there in a browser tab of headless mode: settings are the server's, and can change", async () => {
    const w = open({}, "browser");
    await flushPromises();
    expect(labels(w).some((l) => l.includes("打开"))).toBe(false); // the server's desktop actions are not
    expect(hideButton(w)!.attributes("aria-disabled")).toBe("false");
    w.unmount();
  });

  it("hides it on a click: the setting changes through the backend and the menu closes", async () => {
    const w = open({});
    await flushPromises();
    await hideButton(w)!.trigger("click");
    await flushPromises();
    expect(mocked).toHaveBeenCalledWith("view.hide_folder", { view_id: "v1", node: 3, hide: true });
    expect(get().menu).toBeNull();
    expect(setViewSettings).not.toHaveBeenCalled(); // the view hides them already
    expect(get().toast).toBe("已隐藏「sub」：工具栏的「隐藏文件夹」（H）可以让它回来");
    w.unmount();
  });

  it("shows it again on a click when it is listed", async () => {
    answers[3] = { listed: ["C:\\root\\sub"], path: "", reason: "" };
    const w = open({});
    await flushPromises();
    await hideButton(w)!.trigger("click");
    await flushPromises();
    expect(mocked).toHaveBeenCalledWith("view.hide_folder", { view_id: "v1", node: 3, hide: false });
    expect(get().toast).toBe("「sub」不再隐藏");
    w.unmount();
  });
});

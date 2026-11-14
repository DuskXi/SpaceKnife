// 「在画面中隐藏这个文件夹」 / 「不再隐藏这个文件夹」 (UI-W5-52): when the setting can be changed (as on
// the settings page), what the menu's row offers, and what hiding or showing a folder does: the
// backend changes the setting, the view's 「隐藏文件夹」 comes on, the toast says how to undo it.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SettingMeta, SettingsState } from "@/protocol/generated";
import { frameInfo } from "@/visual/testing/fixtures";

import { applySettings, call, setViewSettings } from "./actions";
import { hideFolder, hideRow, settingLocked } from "./hiddenFolders";
import { get, resetState, set } from "./store";

vi.mock(import("./actions"), async (original) => ({ ...(await original()), call: vi.fn(), applySettings: vi.fn(), setViewSettings: vi.fn() }));

const META = { key: "treemap.hidden_folders", enabled: true, reason: "" } as SettingMeta;
const settings = (meta: SettingMeta = META) => ({ meta: [meta], values: {} }) as unknown as SettingsState;

describe("settingLocked", () => {
  it("allows the change while the backend is there, as the settings page does", () => {
    expect(settingLocked({ conn: "ready", settings: settings() })).toBeUndefined();
    expect(settingLocked({ conn: "ready", settings: null })).toBeUndefined();
  });

  it("says why not without a backend or when the setting is off here", () => {
    expect(settingLocked({ conn: "lost", settings: settings() })).toBe("与后端的连接已断开，设置暂不可修改");
    expect(settingLocked({ conn: "ready", settings: settings({ ...META, enabled: false, reason: "M8 接入" }) })).toBe("M8 接入");
    expect(settingLocked({ conn: "ready", settings: settings({ ...META, enabled: false }) })).toBe("这个设置现在不能修改");
  });
});

describe("hideRow", () => {
  it("hides a folder the list doesn't name, shows one it does", () => {
    expect(hideRow({ listed: [], path: "C:\\Users\\lenny\\Music", reason: "" }, undefined)).toEqual({ hide: true, disabled: undefined });
    expect(hideRow({ listed: ["c:\\users\\lenny\\music"], path: "", reason: "" }, undefined)).toEqual({ hide: false, disabled: undefined });
  });

  it("is greyed with the backend's reason, else when the setting can't change", () => {
    expect(hideRow({ listed: [], path: "", reason: "扫描的根不能隐藏" }, "与后端的连接已断开，设置暂不可修改")).toEqual({ hide: true, disabled: "扫描的根不能隐藏" });
    expect(hideRow({ listed: ["/a"], path: "", reason: "" }, "与后端的连接已断开，设置暂不可修改")).toEqual({ hide: false, disabled: "与后端的连接已断开，设置暂不可修改" });
  });

  it("offers hiding while the answer is on its way", () => {
    expect(hideRow(null, undefined)).toEqual({ hide: true, disabled: undefined });
  });
});

describe("hideFolder", () => {
  const mocked = vi.mocked(call);
  const saved = settings();
  const view = (hide_folders: boolean, crumbs = [{ key: 1, label: "C:\\" }]) =>
    set({ conn: "ready", settings: saved, views: { v1: { state: { scan_id: "s1", hide_folders }, frame: { info: frameInfo({ breadcrumb: crumbs }) } } } as never });

  beforeEach(() => {
    resetState();
    mocked.mockImplementation(async (method) => (method === "view.hide_folder" ? saved : { node: { name: "Music" } }) as never);
  });
  afterEach(() => vi.clearAllMocks());

  it("adds the folder through the backend and takes the settings it answers", async () => {
    view(true);
    await hideFolder("v1", 7, true, "Music");
    expect(mocked).toHaveBeenCalledWith("view.hide_folder", { view_id: "v1", node: 7, hide: true });
    expect(applySettings).toHaveBeenCalledWith(saved);
    expect(setViewSettings).not.toHaveBeenCalled();
    expect(get().toast).toBe("已隐藏「Music」：工具栏的「隐藏文件夹」（H）可以让它回来");
  });

  it("turns the view's 「隐藏文件夹」 on when it was off", async () => {
    view(false);
    await hideFolder("v1", 7, true, "Music");
    expect(setViewSettings).toHaveBeenCalledWith("v1", { hide_folders: true });
  });

  it("says it stays drawn while the view is zoomed into it", async () => {
    view(true, [
      { key: 1, label: "C:\\" },
      { key: 7, label: "Music" },
    ]);
    await hideFolder("v1", 7, true, "Music");
    expect(get().toast).toBe("已隐藏「Music」：放大在它里面时照常显示，回到外面就不占画面了");
  });

  it("takes it out again, the view's switch left as it is", async () => {
    view(false);
    await hideFolder("v1", 7, false);
    expect(mocked).toHaveBeenCalledWith("view.hide_folder", { view_id: "v1", node: 7, hide: false });
    // no name given: the scan's
    expect(mocked).toHaveBeenCalledWith("tree.node", { scan_id: "s1", node: 7 });
    expect(setViewSettings).not.toHaveBeenCalled();
    expect(get().toast).toBe("「Music」不再隐藏");
  });

  it("shows the backend's refusal and changes nothing more", async () => {
    view(false);
    mocked.mockRejectedValue(new Error("扫描的根不能隐藏"));
    await hideFolder("v1", 1, true, "C:\\");
    expect(applySettings).not.toHaveBeenCalled();
    expect(setViewSettings).not.toHaveBeenCalled();
    expect(get().toast).toBe("扫描的根不能隐藏");
  });

  it("doesn't ask the backend while the setting can't change", async () => {
    view(true);
    set({ conn: "lost" });
    await hideFolder("v1", 7, true, "Music");
    expect(mocked).not.toHaveBeenCalled();
    expect(get().toast).toBe("与后端的连接已断开，设置暂不可修改");
  });
});

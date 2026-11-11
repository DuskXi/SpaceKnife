// @vitest-environment happy-dom
// 「要隐藏的文件夹」: the folders one per row, taken out with their button, added by a full path
// (Enter or 「添加」) or 「浏览…」 in the app; every change saved as the folders one per line.

import { mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";

import FolderList from "./FolderList.vue";

const sk = (host: "app" | "browser", picked: string[] = []) => ({ platform: "win32", host, dialog: { openFolders: vi.fn(async () => picked) } });

const mountList = (value: string) => mount(FolderList, { props: { value, label: "要隐藏的文件夹", disabled: false, describedBy: "d" } });

afterEach(() => vi.unstubAllGlobals());

describe("FolderList", () => {
  it("lists the folders and takes one out", async () => {
    vi.stubGlobal("window", Object.assign(window, { sk: sk("app") }));
    const w = mountList("C:\\Users\\lenny\\Music\nC:\\Users\\lenny\\Games");
    expect(w.findAll(".fl-name").map((p) => p.attributes("title"))).toEqual(["C:\\Users\\lenny\\Music", "C:\\Users\\lenny\\Games"]);
    await w.findAll(".fl-item button")[0]!.trigger("click");
    expect(w.emitted("save")).toEqual([["C:\\Users\\lenny\\Games"]]);
  });

  it("says what the list is for while it is empty", () => {
    vi.stubGlobal("window", Object.assign(window, { sk: sk("app") }));
    const w = mountList("");
    expect(w.find(".fl-items").exists()).toBe(false);
    expect(w.find(".fl-empty").text()).toContain("隐藏文件夹");
  });

  it("adds a full path on Enter, and refuses one that isn't", async () => {
    vi.stubGlobal("window", Object.assign(window, { sk: sk("app") }));
    const w = mountList("C:\\Users\\lenny\\Music");
    const input = w.find("input");
    await input.setValue("Games");
    await input.trigger("keydown", { key: "Enter" });
    expect(w.emitted("save")).toBeUndefined();
    expect(w.find(".set-error").text()).toContain("完整路径");
    await input.setValue("  D:\\Games  ");
    await input.trigger("keydown", { key: "Enter" });
    expect(w.emitted("save")).toEqual([["C:\\Users\\lenny\\Music\nD:\\Games"]]);
    expect(w.find(".set-error").exists()).toBe(false);
    expect((input.element as HTMLInputElement).value).toBe("");
  });

  it("keeps a folder added just before, while the saved list is still on its way", async () => {
    vi.stubGlobal("window", Object.assign(window, { sk: sk("app") }));
    const w = mountList("");
    const input = w.find("input");
    await input.setValue("D:\\Videos");
    await input.trigger("keydown", { key: "Enter" });
    await input.setValue("D:\\Music");
    await input.trigger("keydown", { key: "Enter" });
    expect(w.emitted("save")).toEqual([["D:\\Videos"], ["D:\\Videos\nD:\\Music"]]);
    expect(w.findAll(".fl-name").map((p) => p.attributes("title"))).toEqual(["D:\\Videos", "D:\\Music"]);
    // the stored value wins once it comes
    await w.setProps({ value: "D:\\Music" });
    expect(w.findAll(".fl-name").map((p) => p.attributes("title"))).toEqual(["D:\\Music"]);
  });

  it("adds what 「浏览…」 picked, a folder already listed once", async () => {
    const api = sk("app", ["C:\\Users\\lenny\\Music", "C:\\Users\\lenny\\Videos"]);
    vi.stubGlobal("window", Object.assign(window, { sk: api }));
    const w = mountList("C:\\Users\\lenny\\Music");
    const browse = w.findAll("button").find((b) => b.text() === "浏览…")!;
    await browse.trigger("click");
    await new Promise((r) => setTimeout(r, 0));
    expect(api.dialog.openFolders).toHaveBeenCalled();
    expect(w.emitted("save")).toEqual([["C:\\Users\\lenny\\Music\nC:\\Users\\lenny\\Videos"]]);
  });

  it("has no 「浏览…」 in a browser: the folders are on the other computer", () => {
    vi.stubGlobal("window", Object.assign(window, { sk: sk("browser") }));
    const w = mountList("");
    expect(w.findAll("button").map((b) => b.text())).not.toContain("浏览…");
  });
});

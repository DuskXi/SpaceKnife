// @vitest-environment happy-dom
// The log panel: keys pressed in the list stay in the list (arrows move the highlight, Ctrl+C copies
// the line's path, typing never reaches the map's shortcuts); 「只看警告」 filters the lines.

import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SkApi } from "@/shared/api";
import { get, type LogEntry, resetState, set } from "@/state/store";

import LogPanel from "./LogPanel.vue";

const entry = (seq: number, level: LogEntry["level"], path = ""): LogEntry => ({ seq, time: "2026-01-01T10:00:00Z", level, source: "scan", message: `m${seq} ${path}`, scanId: "", path });

describe("LogPanel", () => {
  const copyText = vi.fn();
  let onWindow: ReturnType<typeof vi.fn<(e: KeyboardEvent) => void>>;
  beforeEach(() => {
    resetState();
    window.sk = { shell: { copyText } } as unknown as SkApi;
    onWindow = vi.fn<(e: KeyboardEvent) => void>();
    window.addEventListener("keydown", onWindow);
  });
  afterEach(() => {
    window.removeEventListener("keydown", onWindow);
    copyText.mockClear();
  });

  it("keeps an empty dock while closed", () => {
    const w = mount(LogPanel);
    expect(w.find(".logdock").classes()).not.toContain("open");
    w.unmount();
  });

  it("moves the highlight, copies the path, and keeps keys from the window", async () => {
    set({ logOpen: true, logs: [entry(1, "INFO"), entry(2, "WARNING", "C:\\a"), entry(3, "ERROR")] });
    const w = mount(LogPanel, { attachTo: document.body });
    const list = w.find(".log-list");
    expect(list.attributes("aria-activedescendant")).toBeUndefined();
    await list.trigger("keydown", { key: "ArrowUp" }); // from nothing: the newest line
    expect(list.attributes("aria-activedescendant")).toBe("log-row-2");
    await list.trigger("keydown", { key: "ArrowUp" });
    expect(w.find(".log-row.on").attributes("id")).toBe("log-row-1");
    await list.trigger("keydown", { key: "c", ctrlKey: true });
    expect(copyText).toHaveBeenCalledWith("C:\\a");
    expect(get().toast).toBe("已复制");
    await list.trigger("keydown", { key: "Home" });
    expect(list.attributes("aria-activedescendant")).toBe("log-row-0");
    // a letter or Delete is the log's, not the map's (F, Delete…)
    await list.trigger("keydown", { key: "f" });
    await list.trigger("keydown", { key: "Delete" });
    expect(onWindow).not.toHaveBeenCalled();
    w.unmount();
  });

  it("shows only warnings and errors on request, and closes", async () => {
    set({ logOpen: true, logs: [entry(1, "INFO"), entry(2, "WARNING"), entry(3, "ERROR")] });
    const w = mount(LogPanel);
    expect(w.findAll(".log-row")).toHaveLength(3);
    await w.find(".log-summary button").trigger("click");
    expect(w.findAll(".log-row").map((r) => r.classes())).toEqual([expect.arrayContaining(["warning"]), expect.arrayContaining(["error"])]);
    await w.find("[role=tab]:nth-child(2)").trigger("click");
    expect(get().logTab).toBe("stats");
    expect(w.find(".log-empty").text()).toBe("当前标签没有扫描");
    await w.find(".log-head .icon-btn").trigger("click");
    expect(get()).toMatchObject({ logOpen: false, logDismissed: true });
    w.unmount();
  });
});

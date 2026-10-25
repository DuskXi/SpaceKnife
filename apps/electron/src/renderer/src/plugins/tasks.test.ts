// Plugins' background tasks in the renderer: events put in place, what the status bar shows (running
// first, ended ones for a minute), how far they are, and the status bar's entry in both languages.

import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PluginTaskInfo } from "@/protocol/generated";
import { set } from "@/state/store";
import { setLang } from "@/lib/i18n";

import { TaskRow, TasksStatusItem } from "./TasksStatus";
import { ENDED_SHOWN_MS, onTask, overallShare, putTask, shownTasks, taskShare } from "./tasks";

// a server render reads a store's initial state: these tests read the current one
vi.mock(import("@/state/store"), async (original) => {
  const store = await original();
  return { ...store, useApp: Object.assign(<T>(pick: (s: ReturnType<typeof store.get>) => T) => pick(store.get()), store.useApp) as typeof store.useApp };
});

const NOW = 1_800_000_000_000;

function task(over: Partial<PluginTaskInfo>): PluginTaskInfo {
  return { id: "task-1", plugin: "x.dupes", name: "hash", key: "", title: "计算哈希", state: "running", done: 0, total: 0, text: "", started: NOW / 1000 - 10, ended: null, attempt: 1, error: "", ...over };
}

afterEach(() => {
  set({ pluginTasks: [] });
  setLang("zh-CN");
});

describe("background tasks", () => {
  it("puts each change in place and keeps new ones in order", () => {
    let list = putTask([], task({ id: "a" }));
    list = putTask(list, task({ id: "b" }));
    list = putTask(list, task({ id: "a", done: 5, total: 10 }));
    expect(list.map((t) => [t.id, t.done])).toEqual([
      ["a", 5],
      ["b", 0],
    ]);
    onTask(task({ id: "c" }));
    onTask(task({ id: "c", state: "done", ended: NOW / 1000 }));
  });

  it("shows running ones first, ended ones for a minute", () => {
    const list = [
      task({ id: "old", state: "done", ended: (NOW - ENDED_SHOWN_MS - 1) / 1000 }),
      task({ id: "failed", state: "failed", error: "x", ended: (NOW - 1000) / 1000 }),
      task({ id: "run" }),
      task({ id: "again", state: "restarting", attempt: 2 }),
    ];
    expect(shownTasks(list, NOW).map((t) => t.id)).toEqual(["run", "again", "failed"]);
  });

  it("says how far they are only when every running one can", () => {
    expect(taskShare(task({ done: 3, total: 4 }))).toBe(0.75);
    expect(taskShare(task({ done: 3, total: 0 }))).toBeNull();
    expect(overallShare([task({ done: 1, total: 4 }), task({ id: "b", done: 3, total: 4 })])).toBe(0.5);
    expect(overallShare([task({ done: 1, total: 4 }), task({ id: "b" })])).toBeNull();
    expect(overallShare([task({ state: "done", done: 1, total: 4 })])).toBeNull();
  });

  it("puts a running task in the status bar, nothing when there is none", () => {
    expect(renderToString(createElement(TasksStatusItem))).toBe("");
    set({ pluginTasks: [task({ done: 2, total: 8, started: Date.now() / 1000 })] });
    const out = renderToString(createElement(TasksStatusItem));
    expect(out).toContain("计算哈希");
    expect(out).toContain("25%");
    expect(out).toContain("spinner");
    set({ pluginTasks: [task({}), task({ id: "b" })] });
    setLang("en");
    expect(renderToString(createElement(TasksStatusItem))).toContain("2 plugin tasks");
  });

  it("writes how a task ended, and offers to cancel only a running one", () => {
    setLang("en");
    const failed = renderToString(createElement(TaskRow, { task: task({ state: "failed", error: "disk gone", ended: NOW / 1000 }) }));
    expect(failed).toContain("Failed: disk gone");
    expect(failed).not.toContain("plugin-task-cancel");
    const live = renderToString(createElement(TaskRow, { task: task({ text: "file 3 of 8", done: 3, total: 8 }) }));
    expect(live).toContain("plugin-task-cancel");
    expect(live).toContain("file 3 of 8 · 38%");
    expect(renderToString(createElement(TaskRow, { task: task({ state: "restarting", attempt: 2 }) }))).toContain("attempt 2");
  });
});

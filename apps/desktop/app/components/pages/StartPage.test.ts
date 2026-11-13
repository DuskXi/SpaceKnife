// @vitest-environment happy-dom
// The start page checks a typed path once, shortly after typing stops: the check's answer, kept
// with the form, must not start the same check again (it did, about three times a second, for as
// long as the path stayed there).

import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";

import type { SkApi } from "@/shared/api";
import { activeTab, patchStartForm, resetState } from "@/state/store";

const call = vi.fn(async (method: string, _params?: unknown) => (method === "scan.check_paths" ? { items: [] } : {}));
vi.mock("@/state/actions", async (orig) => ({
  ...(await orig<typeof import("@/state/actions")>()),
  call: (method: string, params?: unknown) => call(method, params),
}));

const { default: StartPage } = await import("./StartPage.vue");

const checks = () => call.mock.calls.filter(([m]) => m === "scan.check_paths");

beforeEach(() => {
  resetState();
  call.mockClear();
  vi.useFakeTimers();
  window.sk = {
    platform: "win32",
    host: "app",
    version: "0.0.0",
    versions: { runtime: "Tauri", runtimeVersion: "2", webview: "1" },
    shell: { openPath: vi.fn(), openExternal: vi.fn(), copyText: vi.fn() },
    dialog: { openFolders: vi.fn(async () => []), openSnapshot: vi.fn(async () => null) },
    files: { pathOf: () => "", onDropPaths: () => () => {}, onDragState: () => () => {} },
  } as unknown as SkApi;
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("StartPage", () => {
  it("checks a typed path once, and not again when the answer comes", async () => {
    const tab = activeTab();
    const w = mount(StartPage, { props: { tab } });
    patchStartForm(tab.id, { path: "D:\\Games" });
    await nextTick();
    expect(checks()).toHaveLength(0); // not while typing
    await vi.advanceTimersByTimeAsync(300);
    expect(checks()).toEqual([["scan.check_paths", { paths: ["D:\\Games"] }]]);
    // the answer is kept with the form; seconds later, still the one check
    await vi.advanceTimersByTimeAsync(5000);
    expect(checks()).toHaveLength(1);
    // another path: checked once more
    patchStartForm(tab.id, { path: "D:\\Work" });
    await vi.advanceTimersByTimeAsync(5000);
    expect(checks().map(([, p]) => p)).toEqual([{ paths: ["D:\\Games"] }, { paths: ["D:\\Work"] }]);
    w.unmount();
  });
});

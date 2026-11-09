// @vitest-environment happy-dom
// The Electron shell's host adapter: window.sk calls go to the main process's IPC channels, and files
// dropped on the window reach the start page with their paths (plan §3.1: StartPage takes drops only
// through onDropPaths in the app, so without this a drop would do nothing).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({ isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: tauri.isTauri }));
vi.mock("@/shim", () => ({ installShim: vi.fn(async () => void (window.sk = { host: "browser" } as never)) }));

import { DRAG_GONE_MS, type ElectronBridge, electronApi } from "./electronHost";
import { installHost } from "./installHost";

function fakeBridge() {
  const calls: unknown[][] = [];
  const listeners = new Map<string, (v: unknown) => void>();
  const bridge: ElectronBridge = {
    platform: "win32",
    version: "0.3.5",
    justUpdated: true,
    versions: { runtime: "Electron", runtimeVersion: "44.9.3", webview: "Chromium 146" },
    invoke: vi.fn(async (channel: string, ...args: unknown[]) => (calls.push(["invoke", channel, ...args]), channel === "sk:openFolders" ? ["D:\\x"] : null)),
    send: vi.fn((channel: string, ...args: unknown[]) => void calls.push(["send", channel, ...args])),
    on: vi.fn((channel: string, cb: (v: unknown) => void) => (listeners.set(channel, cb), () => listeners.delete(channel))),
    pathOf: (file: File) => (file as unknown as { path?: string }).path ?? "",
  };
  return { bridge, calls, listeners };
}

/** A drag event as Chromium sends it for files from the system (or for a drag inside the page). */
function drag(type: string, files: { path: string }[] | null, kind: "Files" | "text/plain" = "Files"): Event {
  const e = new Event(type, { cancelable: true, bubbles: true });
  Object.defineProperty(e, "dataTransfer", { value: { types: [kind], files: files ?? [], dropEffect: "none" } });
  return e;
}

afterEach(() => {
  vi.useRealTimers();
  delete (window as { sk?: unknown }).sk;
  delete window.skElectron;
  tauri.isTauri.mockReturnValue(false);
});

describe("electronApi", () => {
  it("passes what the preload knows and calls the main process's channels", async () => {
    const { bridge, calls, listeners } = fakeBridge();
    const sk = electronApi(bridge, new EventTarget() as never);
    expect([sk.platform, sk.version, sk.host, sk.justUpdated, sk.remote]).toEqual(["win32", "0.3.5", "app", true, null]);
    expect(sk.versions).toEqual({ runtime: "Electron", runtimeVersion: "44.9.3", webview: "Chromium 146" });
    expect(await sk.dialog.openFolders()).toEqual(["D:\\x"]);
    await sk.dialog.saveFile({ title: "Export", filters: [], defaultName: "a.csv" });
    await sk.update.install("C:\\u\\SpaceKnife-0.3.6-Setup-x64.exe", "seamless");
    sk.window.toggleMaximize();
    sk.shell.openTerminal("D:\\a&b");
    sk.setTheme("dark");
    expect(calls).toEqual([
      ["invoke", "sk:openFolders"],
      ["invoke", "sk:saveFile", { title: "Export", filters: [], defaultName: "a.csv" }],
      ["invoke", "sk:installUpdate", "C:\\u\\SpaceKnife-0.3.6-Setup-x64.exe", "seamless"],
      ["send", "sk:win", "toggleMaximize"],
      ["send", "sk:openTerminal", "D:\\a&b"],
      ["send", "sk:setTheme", "dark"],
    ]);
    const seen: unknown[] = [];
    const stop = sk.onBackend((s) => seen.push(s));
    listeners.get("sk:backend")?.({ state: "starting" });
    stop();
    expect(listeners.has("sk:backend")).toBe(false);
    expect(seen).toEqual([{ state: "starting" }]);
  });

  it("gives the start page the paths of files dropped on the window, and the overlay's state", () => {
    vi.useFakeTimers();
    const target = new EventTarget();
    const sk = electronApi(fakeBridge().bridge, target as never);
    const drops: string[][] = [];
    const states: boolean[] = [];
    sk.files.onDropPaths((p) => drops.push(p));
    sk.files.onDragState((over) => states.push(over));

    target.dispatchEvent(drag("dragenter", null));
    const over = drag("dragover", null);
    target.dispatchEvent(over);
    expect(over.defaultPrevented).toBe(true); // a drop is allowed anywhere (Chromium would open the file)
    // moving over the page's elements: enter the next one, leave the last one
    target.dispatchEvent(drag("dragenter", null));
    target.dispatchEvent(drag("dragleave", null));
    expect(states).toEqual([true]);
    const drop = drag("drop", [{ path: "D:\\Games" }, { path: "E:\\Work & Play" }, { path: "" }]);
    target.dispatchEvent(drop);
    expect(drop.defaultPrevented).toBe(true);
    expect(drops).toEqual([["D:\\Games", "E:\\Work & Play"]]);
    expect(states).toEqual([true, false]);

    // leaving the window
    target.dispatchEvent(drag("dragenter", null));
    target.dispatchEvent(drag("dragleave", null));
    expect(states).toEqual([true, false, true, false]);
  });

  it("drops the overlay when the files leave without a dragleave", () => {
    vi.useFakeTimers();
    const target = new EventTarget();
    const sk = electronApi(fakeBridge().bridge, target as never);
    const states: boolean[] = [];
    sk.files.onDragState((over) => states.push(over));
    target.dispatchEvent(drag("dragenter", null));
    vi.advanceTimersByTime(DRAG_GONE_MS - 100);
    target.dispatchEvent(drag("dragover", null)); // still held over the window
    vi.advanceTimersByTime(DRAG_GONE_MS - 100);
    expect(states).toEqual([true]);
    vi.advanceTimersByTime(200);
    expect(states).toEqual([true, false]);
  });

  it("leaves drags inside the page alone, and stops telling after the unsubscribe", () => {
    const target = new EventTarget();
    const sk = electronApi(fakeBridge().bridge, target as never);
    const drops: string[][] = [];
    const states: boolean[] = [];
    const stopDrops = sk.files.onDropPaths((p) => drops.push(p));
    sk.files.onDragState((over) => states.push(over));
    const inner = drag("dragover", null, "text/plain");
    target.dispatchEvent(drag("dragenter", null, "text/plain"));
    target.dispatchEvent(inner);
    target.dispatchEvent(drag("drop", [{ path: "D:\\x" }], "text/plain"));
    expect([inner.defaultPrevented, drops, states]).toEqual([false, [], []]);
    stopDrops();
    target.dispatchEvent(drag("drop", [{ path: "D:\\x" }]));
    expect(drops).toEqual([]);
  });
});

describe("installHost", () => {
  beforeEach(() => {
    delete (window as { sk?: unknown }).sk;
  });

  it("takes the Electron shell's bridge first, then Tauri, then the browser shim", async () => {
    window.skElectron = fakeBridge().bridge;
    tauri.isTauri.mockReturnValue(true); // never both, but Electron's preload is the plainer sign
    await installHost();
    expect(window.sk.versions.runtime).toBe("Electron");

    delete (window as { sk?: unknown }).sk;
    delete window.skElectron;
    tauri.isTauri.mockReturnValue(false);
    await installHost();
    expect(window.sk.host).toBe("browser");
  });

  it("keeps a window.sk that is already there", async () => {
    const mine = { host: "app" } as never;
    window.sk = mine;
    window.skElectron = fakeBridge().bridge;
    await installHost();
    expect(window.sk).toBe(mine);
  });
});

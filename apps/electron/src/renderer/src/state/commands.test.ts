// Keyboard shortcuts of palette commands: which key names which command, and that a key runs its
// command only where the backend's palette.check says the palette would offer it.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { PaletteCommand } from "@/protocol/generated";

import { call, rescanFolder, startScan } from "./actions";
import { runShortcut, shortcutCommand } from "./commands";
import { get, set } from "./store";

vi.mock(import("./actions"), async (original) => ({ ...(await original()), call: vi.fn(), rescanFolder: vi.fn(), startScan: vi.fn() }));

const key = (k: string, mods: { ctrl?: boolean; meta?: boolean; shift?: boolean; alt?: boolean } = {}) => ({
  key: k,
  ctrlKey: !!mods.ctrl,
  metaKey: !!mods.meta,
  shiftKey: !!mods.shift,
  altKey: !!mods.alt,
});

describe("shortcutCommand", () => {
  it("names the command of F5, Shift+F5 and Ctrl+S (⌘S on macOS)", () => {
    expect(shortcutCommand(key("F5"))).toBe("view.rescan");
    expect(shortcutCommand(key("F5", { shift: true }))).toBe("scan.rescan_all");
    expect(shortcutCommand(key("s", { ctrl: true }))).toBe("snapshot.save");
    expect(shortcutCommand(key("S", { ctrl: true }))).toBe("snapshot.save"); // Caps Lock
    expect(shortcutCommand(key("s", { meta: true }))).toBe("snapshot.save");
  });

  it("leaves every other key alone", () => {
    for (const k of [key("s"), key("S", { shift: true }), key("s", { ctrl: true, shift: true }), key("s", { ctrl: true, alt: true })]) {
      expect(shortcutCommand(k)).toBeNull();
    }
    for (const k of [key("F5", { ctrl: true }), key("F5", { alt: true }), key("F6"), key("5")]) expect(shortcutCommand(k)).toBeNull();
  });
});

describe("runShortcut", () => {
  const mocked = vi.mocked(call);
  const state = (enabled: boolean, status = ""): PaletteCommand => ({ id: "x", name: "保存快照", enabled, status, hits: [], icon: "snapshot", keys: ["Ctrl+S"], more: false, action: null, menus: [], plugin: null });

  beforeEach(() => {
    set({
      tabs: [{ id: "t1", kind: "scan", scanId: "s1", viewIds: ["v1"], focusedPane: 0, title: "C:" }],
      active: "t1",
      toast: null,
    });
  });
  afterEach(() => vi.clearAllMocks());

  it("asks the backend where the user is, and runs the command when it is available", async () => {
    mocked.mockImplementation(async (method) => (method === "palette.check" ? state(true) : { path: "C:/snapshots/c-2026.sks" }) as never);
    await runShortcut("snapshot.save");
    expect(mocked.mock.calls[0]).toEqual(["palette.check", { id: "snapshot.save", context: { tab: "scan", scan_id: "s1", view_id: "v1", basket: 0 } }]);
    expect(mocked.mock.calls[1]).toEqual(["snapshot.save", { scan_id: "s1" }]);
    expect(get().toast).toBe("快照已保存：c-2026.sks");
  });

  it("says why not when the palette would show the command greyed out", async () => {
    mocked.mockResolvedValue(state(false, "扫描进行中") as never);
    await runShortcut("snapshot.save");
    expect(mocked).toHaveBeenCalledTimes(1);
    expect(get().toast).toBe("保存快照：扫描进行中");
  });

  it("rescans the view's zoom root, the top of several roots included", async () => {
    mocked.mockResolvedValue(state(true) as never);
    set({ views: { v1: { state: { zoom_node: 0 } } } as never });
    await runShortcut("view.rescan");
    expect(rescanFolder).toHaveBeenCalledWith("s1", 0);
  });

  it("rescans everything with the scan's roots", async () => {
    mocked.mockResolvedValue(state(true) as never);
    set({ scans: { s1: { roots: [{ path: "C:\\" }, { path: "D:\\" }] } } as never });
    await runShortcut("scan.rescan_all");
    expect(startScan).toHaveBeenCalledWith({ roots: ["C:\\", "D:\\"] }, "t1");
  });
});

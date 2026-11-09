// index.ts as it starts: SPACEKNIFE_READY_FILE (end-to-end tests, demos, the swap watching a new
// version) is out of this process's environment before anything runs, so that nothing the app
// starts — a backend restarted later, an update's helper, a terminal — inherits it. Electron is a
// stand-in that does nothing (the window never comes: whenReady never settles).

import { describe, expect, it, vi } from "vitest";

function inert(fields: Record<string, unknown> = {}): Record<string, unknown> {
  return new Proxy(fields, {
    get(target, key) {
      if (typeof key !== "string") return undefined;
      if (key in target) return target[key];
      return (..._args: unknown[]) => {
        if (key === "whenReady") return new Promise(() => {});
        if (key === "getPath") return "/nowhere";
        return undefined;
      };
    },
  });
}

vi.mock("electron", () => ({
  app: inert({ isPackaged: false }),
  BrowserWindow: inert(),
  Menu: inert(),
  clipboard: inert(),
  dialog: inert(),
  ipcMain: inert(),
  nativeTheme: inert(),
  protocol: inert(),
  session: inert(),
  shell: inert(),
}));

describe("the main process as it starts", () => {
  it("takes the ready file out of its environment", async () => {
    const argv = process.argv;
    process.env.SPACEKNIFE_READY_FILE = "ready-from-the-test.json";
    try {
      process.argv = [process.execPath, "SpaceKnife"];
      await import("./index");
      expect(process.env.SPACEKNIFE_READY_FILE).toBeUndefined();
      expect(Object.keys(process.env).filter((k) => k.toUpperCase() === "SPACEKNIFE_READY_FILE")).toEqual([]);
    } finally {
      process.argv = argv;
      delete process.env.SPACEKNIFE_READY_FILE;
    }
  });
});

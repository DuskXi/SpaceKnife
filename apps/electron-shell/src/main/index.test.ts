// index.ts under `--startup-check` (the swap helper's question to the new version of a portable
// copy, still unpacked in the updates folder): it answers and ends before anything else of the main
// process runs. Electron is a stand-in here that records what the module asks of it.

import { afterEach, describe, expect, it, vi } from "vitest";

const calls: string[] = [];
let exit: (code: number) => void = () => {};
const exited = new Promise<number>((resolve) => (exit = resolve));

/** An object every property of which is a function noting its call ("app.setPath"); `fields` are
 *  plain values. */
function recorder(name: string, fields: Record<string, unknown> = {}): Record<string, unknown> {
  return new Proxy(fields, {
    get(target, key) {
      if (typeof key !== "string") return undefined;
      if (key in target) return target[key];
      return (...args: unknown[]) => {
        calls.push(`${name}.${key}`);
        if (key === "exit") exit(Number(args[0] ?? 0));
        if (key === "whenReady") return new Promise(() => {});
        if (key === "getPath") return "/nowhere";
        return undefined;
      };
    },
  });
}

vi.mock("electron", () => ({
  app: recorder("app", { isPackaged: true }),
  BrowserWindow: recorder("BrowserWindow"),
  Menu: recorder("Menu"),
  clipboard: recorder("clipboard"),
  dialog: recorder("dialog"),
  ipcMain: recorder("ipcMain"),
  nativeTheme: recorder("nativeTheme"),
  protocol: recorder("protocol"),
  session: recorder("session"),
  shell: recorder("shell"),
}));

const argv = process.argv;
afterEach(() => {
  process.argv = argv;
  vi.restoreAllMocks();
});

describe("--startup-check", () => {
  it("is answered before the portable copy's data folder, the profile or the app:// scheme", async () => {
    const written: string[] = [];
    vi.spyOn(process.stdout, "write").mockImplementation(((chunk: string, done?: () => void) => {
      written.push(String(chunk));
      done?.();
      return true;
    }) as typeof process.stdout.write);
    const home = process.env.SPACEKNIFE_HOME;
    process.argv = [process.execPath, "SpaceKnife", "--startup-check"];
    // the module never finishes evaluating when it is asked: nothing after the answer runs
    void import("./index");
    expect(await exited).toBe(0);
    await new Promise((r) => setTimeout(r, 50)); // what would have run next, had it gone on
    expect(written.join("")).toBe(`${JSON.stringify({ ok: true, electron: process.versions.electron ?? "" })}\n`);
    expect(calls.filter((c) => c !== "app.exit")).toEqual([]);
    expect(process.env.SPACEKNIFE_HOME).toBe(home);
  });
});

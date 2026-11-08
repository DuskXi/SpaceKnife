// @vitest-environment happy-dom
// The command palette: rows from palette.query, the highlight under the arrow keys, Enter runs the
// highlighted row (or, typed ahead of the answer, the first runnable row once it arrives), Tab
// completes, Esc closes.

import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { PaletteCommand, PaletteResult } from "@/protocol/generated";
import { get, resetState, set } from "@/state/store";

const query = vi.fn<(text: string) => PaletteResult>();
const runCommand = vi.fn(async (_id: string) => {});

vi.mock("@/state/actions", async (orig) => ({
  ...(await orig<typeof import("@/state/actions")>()),
  cancellable: (method: string, params: { text: string }) => ({
    promise: method === "palette.query" ? Promise.resolve(query(params.text)) : Promise.resolve(null),
    cancel: () => {},
  }),
  call: vi.fn(async () => ({})),
}));
vi.mock("@/state/commands", async (orig) => ({
  ...(await orig<typeof import("@/state/commands")>()),
  runCommand: (id: string) => runCommand(id),
}));

const { default: Palette } = await import("./Palette.vue");

const cmd = (id: string, name: string, enabled = true): PaletteCommand => ({ id, name, enabled, hits: [], icon: "", keys: [], more: false, status: "" });
const answer = (text: string, commands: PaletteCommand[]): PaletteResult => ({ text, commands, direct: [], recent: [], paths: false, seed: null });

describe("Palette", () => {
  beforeEach(() => {
    resetState();
    vi.useFakeTimers({ toFake: ["setTimeout", "setInterval"] });
    query.mockImplementation((text) => answer(text, [cmd("a", "Alpha", false), cmd("b", "Beta"), cmd("c", "Gamma")]));
  });
  afterEach(() => {
    vi.useRealTimers();
    runCommand.mockClear();
  });

  async function open() {
    set({ paletteOpen: true });
    const w = mount(Palette, { attachTo: document.body });
    await flushPromises();
    return w;
  }

  it("is there only while open", async () => {
    const w = mount(Palette);
    expect(w.find(".palette").exists()).toBe(false);
    w.unmount();
  });

  it("highlights the first runnable row and moves with the arrows", async () => {
    const w = await open();
    const input = w.find("input");
    expect(w.findAll(".pal-row").map((r) => r.attributes("aria-label"))).toEqual(["Alpha", "Beta", "Gamma"]);
    // Alpha is disabled: Beta is highlighted first
    expect(input.attributes("aria-activedescendant")).toBe("pal-1");
    await input.trigger("keydown", { key: "ArrowDown" });
    expect(input.attributes("aria-activedescendant")).toBe("pal-2");
    await input.trigger("keydown", { key: "ArrowDown" }); // wraps
    expect(input.attributes("aria-activedescendant")).toBe("pal-0");
    await input.trigger("keydown", { key: "ArrowUp" });
    expect(w.find(".pal-row.on").attributes("aria-label")).toBe("Gamma");
    w.unmount();
  });

  it("runs the highlighted row on Enter and closes", async () => {
    const w = await open();
    await w.find("input").trigger("keydown", { key: "Enter" });
    expect(get().paletteOpen).toBe(false);
    vi.runAllTimers();
    expect(runCommand).toHaveBeenCalledWith("b");
    w.unmount();
  });

  it("waits for the answer to the current input before Enter runs", async () => {
    const w = await open();
    const input = w.find("input");
    query.mockImplementation((text) => answer(text, [cmd("g", "Gamma")]));
    (input.element as HTMLInputElement).value = "gam";
    await input.trigger("input");
    // the rows on screen still answer "": Enter waits
    await input.trigger("keydown", { key: "Enter" });
    expect(runCommand).not.toHaveBeenCalled();
    await flushPromises();
    vi.runAllTimers();
    expect(runCommand).toHaveBeenCalledWith("g");
    w.unmount();
  });

  it("completes a command's name with Tab and closes on Esc", async () => {
    const w = await open();
    const input = w.find("input");
    await input.trigger("keydown", { key: "Tab" });
    expect((input.element as HTMLInputElement).value).toBe("Beta");
    await input.trigger("keydown", { key: "Escape" });
    expect(get().paletteOpen).toBe(false);
    w.unmount();
  });
});

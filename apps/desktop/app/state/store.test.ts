// The store keeps zustand's semantics on top of Pinia (store.ts): a plain state replaced by set(),
// and computed reads for components that follow only the fields they read.

import { afterEach, describe, expect, it } from "vitest";
import { effectScope, nextTick, watch } from "vue";

import { appStore, get, patchView, resetState, set, subscribe, useApp } from "./store";

afterEach(() => resetState());

describe("store", () => {
  it("merges patches shallowly and replaces the state object", () => {
    const before = get();
    set({ toast: "hello" });
    const after = get();
    expect(after).not.toBe(before);
    expect(before.toast).toBeNull(); // a state read earlier stays as it was
    expect(after.toast).toBe("hello");
    expect(after.tabs).toBe(before.tabs);
    set((s) => ({ logs: [...s.logs, { seq: 1, time: "", level: "INFO", source: "system", message: "x", scanId: "", path: "" }] }));
    expect(get().logs).toHaveLength(1);
    expect(get().toast).toBe("hello");
  });

  it("leaves the state alone when a patch function returns it unchanged", () => {
    const before = get();
    patchView("no-such-view", { filterText: "x" });
    expect(get()).toBe(before);
  });

  it("mirrors every change into the Pinia store", () => {
    set({ paletteOpen: true });
    expect(appStore().paletteOpen).toBe(true);
    set({ paletteOpen: false });
    expect(appStore().paletteOpen).toBe(false);
  });

  it("tells subscribers the new and the previous state", () => {
    const seen: [string | null, string | null][] = [];
    const off = subscribe((s, prev) => seen.push([s.toast, prev.toast]));
    set({ toast: "a" });
    off();
    set({ toast: "b" });
    expect(seen).toEqual([["a", null]]);
  });

  it("gives components computed reads that follow only what they read", async () => {
    const scope = effectScope();
    const runs: number[] = [];
    scope.run(() => {
      const open = useApp((s) => s.logOpen);
      watch(open, (v) => runs.push(v ? 1 : 0));
    });
    set({ toast: "unrelated" });
    await nextTick();
    expect(runs).toEqual([]);
    set({ logOpen: true });
    await nextTick();
    expect(runs).toEqual([1]);
    scope.stop();
  });

  it("does not proxy what it holds (frames carry typed arrays and class instances)", () => {
    const frame = { rows: new Float32Array(4) };
    set({ menu: null, views: { v: { frame } as never } });
    expect(appStore().views.v).toBe(get().views.v);
    expect((appStore().views.v as unknown as { frame: unknown }).frame).toBe(frame);
  });
});

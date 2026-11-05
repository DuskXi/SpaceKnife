import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BinaryFrame } from "@/protocol/rpc";

import { FramePairer, frameKey } from "./frames";

const bin = (seq: number, layout = "drawlist.v1", stream = "view:v1"): BinaryFrame => ({
  header: { stream, seq, final: true, layout, count: 0, columns: {} },
  columns: {},
  blobs: {},
});

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("FramePairer", () => {
  it("pairs a binary frame that came first with the JSON that follows", async () => {
    const p = new FramePairer();
    const f = bin(3);
    p.accept(f);
    expect(p.sizes).toEqual({ frames: 1, waiters: 0 });
    await expect(p.waitFor(frameKey("view:v1", 3))).resolves.toBe(f);
    expect(p.sizes).toEqual({ frames: 0, waiters: 0 });
  });

  it("pairs JSON that came first with the binary frame that follows", async () => {
    const p = new FramePairer();
    const got = p.waitFor(frameKey("view:v1", 4));
    expect(p.sizes.waiters).toBe(1);
    const f = bin(4);
    p.accept(f);
    await expect(got).resolves.toBe(f);
    expect(p.sizes).toEqual({ frames: 0, waiters: 0 });
  });

  it("keeps frames of other views and sequence numbers apart", async () => {
    const p = new FramePairer();
    const a = bin(1, "drawlist.v1", "view:a");
    const b = bin(1, "drawlist.v1", "view:b");
    p.accept(a);
    p.accept(b);
    await expect(p.waitFor("view:b#1")).resolves.toBe(b);
    await expect(p.waitFor("view:a#1")).resolves.toBe(a);
  });

  it("gives up waiting after the timeout and leaves no waiter behind", async () => {
    const p = new FramePairer();
    const got = p.waitFor(frameKey("view:v1", 9), 10_000);
    vi.advanceTimersByTime(9_999);
    expect(p.sizes.waiters).toBe(1);
    vi.advanceTimersByTime(1);
    await expect(got).resolves.toBeNull();
    expect(p.sizes.waiters).toBe(0);
    // the frame arriving late is kept like any other
    p.accept(bin(9));
    expect(p.sizes).toEqual({ frames: 1, waiters: 0 });
  });

  it("waits ten seconds by default", async () => {
    const p = new FramePairer();
    const got = p.waitFor(frameKey("view:v1", 1));
    vi.advanceTimersByTime(10_000);
    await expect(got).resolves.toBeNull();
  });

  it("never keeps a frame of a layout it cannot draw, and ends its waiter with null", async () => {
    const p = new FramePairer();
    p.accept(bin(1, "rings.v9"));
    expect(p.sizes).toEqual({ frames: 0, waiters: 0 });
    const got = p.waitFor(frameKey("view:v1", 2));
    p.accept(bin(2, "rings.v9"));
    await expect(got).resolves.toBeNull();
    expect(p.sizes).toEqual({ frames: 0, waiters: 0 });
  });

  it("decodes what visual/frame.ts knows by default, or what it is told", () => {
    const known = new FramePairer();
    for (const layout of ["drawlist.v1", "flame.v1", "polar.v1"]) known.accept(bin(1, layout, `view:${layout}`));
    expect(known.sizes.frames).toBe(3);
    const treemapOnly = new FramePairer((layout) => layout === "drawlist.v1");
    treemapOnly.accept(bin(1, "flame.v1"));
    expect(treemapOnly.sizes.frames).toBe(0);
  });

  it("keeps the latest 32 frames nobody waited for", async () => {
    const p = new FramePairer();
    for (let seq = 1; seq <= 40; seq++) p.accept(bin(seq));
    expect(p.sizes.frames).toBe(32);
    const old = p.waitFor(frameKey("view:v1", 8));
    vi.advanceTimersByTime(10_000);
    await expect(old).resolves.toBeNull();
    await expect(p.waitFor(frameKey("view:v1", 9))).resolves.not.toBeNull();
    await expect(p.waitFor(frameKey("view:v1", 40))).resolves.not.toBeNull();
  });

  it("lets a second wait for the same frame replace the first", async () => {
    const p = new FramePairer();
    const first = p.waitFor(frameKey("view:v1", 5));
    const second = p.waitFor(frameKey("view:v1", 5));
    await expect(first).resolves.toBeNull();
    const f = bin(5);
    p.accept(f);
    await expect(second).resolves.toBe(f);
    expect(p.sizes.waiters).toBe(0);
  });

  it("ends every waiter and forgets every frame on clear", async () => {
    const p = new FramePairer();
    p.accept(bin(1));
    const got = p.waitFor(frameKey("view:v1", 2));
    p.clear();
    await expect(got).resolves.toBeNull();
    expect(p.sizes).toEqual({ frames: 0, waiters: 0 });
  });
});

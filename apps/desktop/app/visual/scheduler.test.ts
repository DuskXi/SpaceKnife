import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { breathGap, FPS_GAP, FrameLoop, frameGap, transitionFor } from "./scheduler";
import { polarScene, rectScene } from "./testing/fixtures";
import { HIT_NONE } from "./types";

describe("frame pacing", () => {
  const capped = { fpsGap: FPS_GAP["60"]!, dynaSmooth: true };
  const free = { fpsGap: 0, dynaSmooth: true };

  it("draws at once while nothing moves", () => {
    expect(frameGap(capped, { user: false, ambient: false })).toBe(0);
    expect(breathGap(capped, false, false)).toBe(0);
    expect(breathGap(capped, true, true)).toBe(0); // reduced motion: one still drawing
  });

  it("gives what the user does the full rate and change flashes 30 fps under DynaSmooth", () => {
    expect(frameGap(capped, { user: true, ambient: true })).toBe(FPS_GAP["60"]);
    expect(frameGap(capped, { user: false, ambient: true })).toBe(FPS_GAP["30"]);
    expect(frameGap(free, { user: true, ambient: false })).toBe(0);
    expect(frameGap({ ...capped, dynaSmooth: false }, { user: false, ambient: true })).toBe(FPS_GAP["60"]);
    expect(breathGap(free, true, false)).toBe(FPS_GAP["30"]);
    expect(breathGap({ fpsGap: 0, dynaSmooth: false }, true, false)).toBe(0);
  });
});

describe("FrameLoop", () => {
  const queue = new Map<number, FrameRequestCallback>();
  let next = 1;
  let clock = 0;
  beforeEach(() => {
    queue.clear();
    clock = 1000;
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => (queue.set(next, cb), next++));
    vi.stubGlobal("cancelAnimationFrame", (id: number) => queue.delete(id));
    vi.spyOn(performance, "now").mockImplementation(() => clock);
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });
  /** One display frame `ms` later. */
  const tick = (ms: number) => {
    clock += ms;
    const due = [...queue.values()];
    queue.clear();
    due.forEach((cb) => cb(clock));
  };

  it("draws now and goes on while the drawing asks for more", () => {
    let left = 3;
    const draw = vi.fn(() => --left > 0);
    const loop = new FrameLoop(draw, () => 0);
    loop.run();
    expect(draw).toHaveBeenCalledTimes(1);
    tick(16);
    tick(16);
    tick(16);
    expect(draw).toHaveBeenCalledTimes(3);
  });

  it("skips display frames that come too early for the cap", () => {
    const draw = vi.fn(() => true);
    const loop = new FrameLoop(draw, () => 30);
    loop.run();
    tick(16); // 16 ms after the first draw: skipped
    expect(draw).toHaveBeenCalledTimes(1);
    tick(16); // 32 ms
    expect(draw).toHaveBeenCalledTimes(2);
  });

  it("stops", () => {
    const draw = vi.fn(() => true);
    const loop = new FrameLoop(draw, () => 0);
    loop.run();
    loop.stop();
    tick(16);
    expect(draw).toHaveBeenCalledTimes(1);
  });
});

describe("transitionFor", () => {
  const before = rectScene().frame;
  const after = rectScene({ info: { seq: 2 } }).frame;

  it("zooms into the old frame's row of the new zoom key", () => {
    expect(transitionFor(before, after, "zoom", 3, 1)).toEqual({ kind: "in", row: 2 });
    expect(transitionFor(before, after, "zoom", 999, 1)).toBeNull();
  });

  it("shrinks the new frame from the row that was the zoom root", () => {
    for (const reason of ["back", "up", "home"]) expect(transitionFor(before, after, reason, 1, 3)).toEqual({ kind: "out", row: 2 });
    expect(transitionFor(before, after, "up", 1, null)).toBeNull();
    expect(transitionFor(before, after, "up", 1, 999)).toBeNull();
  });

  it("grows like a zoom going forward, when the old frame has the new zoom root", () => {
    expect(transitionFor(before, after, "forward", 3, 1)).toEqual({ kind: "in", row: 2 });
    expect(transitionFor(before, after, "forward", 999, 1)).toBeNull();
  });

  it("cross-fades another visualization", () => {
    const polar = polarScene().frame;
    expect(transitionFor(before, polar, "scan", null, null)).toEqual({ kind: "swap", row: HIT_NONE });
    expect(transitionFor(before, after, "visual", 1, 1)).toEqual({ kind: "swap", row: HIT_NONE });
  });

  it("simply replaces a frame for any other reason", () => {
    expect(transitionFor(before, after, "scan", 1, 1)).toBeNull();
    expect(transitionFor(before, after, "settings", 1, 1)).toBeNull();
  });
});

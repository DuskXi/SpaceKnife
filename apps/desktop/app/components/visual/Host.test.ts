// @vitest-environment happy-dom
// The view host's pointer handling: hit testing through the component's geometry, the hover it
// reports, the calls a click / double click / right click make, hover driven from the store, and the
// handle the shell asks for a row's box. The GL context, the component and the overlays are fakes.

import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";

import type { SkApi } from "@/shared/api";
import { get, resetState, set, type ViewData } from "@/state/store";
import { hosts } from "@/visual/hosts";
import { HIT_NONE, HIT_ROOT, type HoverInfo, type VisualFrame } from "@/visual/types";

const calls = vi.hoisted(() => ({ viewCall: vi.fn(async () => undefined), setViewSettings: vi.fn(async () => undefined) }));
vi.mock("@/state/actions", () => calls);

const gl = vi.hoisted(() => ({
  acquireGl: vi.fn(() => ({ gl: { getExtension: () => null }, lost: false, begin: vi.fn(), shared: (_k: string, make: () => unknown) => make(), restored: vi.fn(), release: vi.fn() })),
}));
vi.mock("@/visual/glContext", () => gl);

vi.mock("@/visual/overlays", () => ({
  breathingRows: () => [],
  trashMarks: () => [],
  flashOf: () => null,
  classMarks: () => null,
  classFade: () => 0,
  trashMoves: () => false,
  trailMark: () => null,
  drawBreath: vi.fn(),
  drawClassMark: vi.fn(),
  drawFlash: () => false,
  drawTrail: () => [],
  drawTrash: vi.fn(),
}));

// one block at the left half (rows 0), the zoom root's bar along the top, nothing elsewhere
const instance = vi.hoisted(() => ({ setFrame: vi.fn(), setHover: vi.fn(), draw: vi.fn(() => false), dispose: vi.fn(), contextRestored: vi.fn() }));
const component = vi.hoisted(() => ({
  layout: "fake.v1",
  hoverPlacement: "beside" as "beside" | "pointer",
  create: vi.fn(() => instance),
  geometry: {
    hitTest: (_f: unknown, x: number, y: number) => (y < 10 ? -2 : x < 50 ? 0 : -1),
    bounds: (_f: unknown, hit: number) => (hit === 0 ? { x: 0, y: 10, w: 50, h: 90 } : hit === -2 ? { x: 0, y: 0, w: 100, h: 10 } : null),
    anchor: () => null,
    filled: () => true,
  },
}));
vi.mock("@/visual/registry", () => ({ componentFor: () => component }));

import Host from "./Host.vue";

const VIEW = "view-1";

function fakeFrame(): VisualFrame {
  return {
    info: { width: 100, height: 100, dpr: 1, seq: 1, visual: "treemap", root: { key: 99, node: 5, kind: 1, tone: 0, branch: 0, size: 10, flags: 0 }, flash: [] },
    layout: "fake.v1",
    count: 1,
    key: Uint32Array.of(7),
    node: Int32Array.of(3),
    kind: Uint8Array.of(0),
    delta: null,
    rowOf: (key: number) => (key === 7 ? 0 : -1),
  } as unknown as VisualFrame;
}

function putView(frame: VisualFrame, patch: Partial<ViewData> = {}): void {
  const view = { state: { scan_id: "s1", color_mode: "class", visual: "treemap", zoom_key: null }, info: null, frame, filter: null, parse: null, filterText: "", hoverKey: null, ...patch } as unknown as ViewData;
  set({ views: { ...get().views, [VIEW]: view } });
}

const at = (x: number, y: number) => ({ clientX: x, clientY: y });

describe("Host", () => {
  beforeEach(() => {
    resetState();
    vi.clearAllMocks();
    window.sk = { platform: "win32" } as unknown as SkApi;
    // happy-dom lays nothing out and has no canvas: a 100 × 100 picture at the origin, a 2D context that draws nothing
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue({ left: 0, top: 0, width: 100, height: 100, right: 100, bottom: 100, x: 0, y: 0, toJSON: () => ({}) });
    const ctx2d = new Proxy({}, { get: () => () => undefined });
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(ctx2d as never);
    vi.stubGlobal("requestAnimationFrame", () => 0);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("hands the frame to a new component instance", () => {
    const frame = fakeFrame();
    putView(frame);
    const host = mount(Host, { props: { viewId: VIEW } });
    expect(component.create).toHaveBeenCalledTimes(1);
    expect(instance.setFrame).toHaveBeenCalledWith(frame, expect.objectContaining({ colorMode: "class" }));
    host.unmount();
    expect(instance.dispose).toHaveBeenCalled();
  });

  it("hit tests the pointer and reports the hover once per target", async () => {
    putView(fakeFrame());
    const host = mount(Host, { props: { viewId: VIEW } });
    const el = host.find(".treemap");
    // a new frame under no pointer: no hover
    expect(host.emitted("hover")).toEqual([[null]]);
    await el.trigger("mousemove", at(20, 50));
    expect(instance.setHover).toHaveBeenLastCalledWith(0);
    const hovers = host.emitted("hover") as [HoverInfo | null][];
    expect(hovers).toHaveLength(2);
    expect(hovers[1]![0]).toMatchObject({ viewId: VIEW, target: 0, avoid: { x: 0, y: 10, w: 50, h: 90 }, pointer: { x: 20, y: 50 }, placement: "beside" });
    // the same block: a card beside it stays where it is
    await el.trigger("mousemove", at(30, 60));
    expect(host.emitted("hover")).toHaveLength(2);
    // the zoom root's bar, then nothing
    await el.trigger("mousemove", at(30, 5));
    expect((host.emitted("hover")!.at(-1) as [HoverInfo])[0].target).toBe(HIT_ROOT);
    await el.trigger("mousemove", at(80, 50));
    expect(host.emitted("hover")!.at(-1)).toEqual([null]);
    expect(instance.setHover).toHaveBeenLastCalledWith(HIT_NONE);
    await el.trigger("mousemove", at(20, 50));
    await el.trigger("mouseleave");
    expect(host.emitted("hover")!.at(-1)).toEqual([null]);
    host.unmount();
  });

  it("follows the pointer inside a row when the card does", async () => {
    component.hoverPlacement = "pointer";
    try {
      putView(fakeFrame());
      const host = mount(Host, { props: { viewId: VIEW } });
      await host.find(".treemap").trigger("mousemove", at(20, 50));
      await host.find(".treemap").trigger("mousemove", at(25, 55));
      const hovers = (host.emitted("hover") as [HoverInfo][]).slice(1);
      expect(hovers.map((h) => h[0].pointer)).toEqual([
        { x: 20, y: 50 },
        { x: 25, y: 55 },
      ]);
      host.unmount();
    } finally {
      component.hoverPlacement = "beside";
    }
  });

  it("selects, zooms and opens the menu of what is under the pointer", async () => {
    const frame = fakeFrame();
    putView(frame);
    const host = mount(Host, { props: { viewId: VIEW } });
    const el = host.find(".treemap");
    await el.trigger("click", at(20, 50));
    expect(calls.viewCall).toHaveBeenLastCalledWith(VIEW, "view.select", { key: 7 });
    await el.trigger("click", at(20, 5));
    expect(calls.viewCall).toHaveBeenLastCalledWith(VIEW, "view.select", { key: null });
    await el.trigger("dblclick", at(20, 50));
    expect(calls.viewCall).toHaveBeenLastCalledWith(VIEW, "view.zoom", { key: 7 });
    await el.trigger("dblclick", at(20, 5));
    expect(calls.viewCall).toHaveBeenLastCalledWith(VIEW, "view.up");
    await el.trigger("contextmenu", at(20, 50));
    const menu = host.emitted("menu")!;
    expect(menu).toHaveLength(1);
    expect(menu[0]![0]).toBe(0);
    expect(menu[0]![1]).toBe(frame);
    // empty space has no menu
    await el.trigger("contextmenu", at(80, 50));
    expect(host.emitted("menu")).toHaveLength(1);
    host.unmount();
  });

  it("highlights the row the result list hovers", async () => {
    const frame = fakeFrame();
    putView(frame);
    const host = mount(Host, { props: { viewId: VIEW } });
    instance.setHover.mockClear();
    putView(frame, { hoverKey: 7 });
    await nextTick();
    expect(instance.setHover).toHaveBeenLastCalledWith(0);
    putView(frame, { hoverKey: 99 });
    await nextTick();
    expect(instance.setHover).toHaveBeenLastCalledWith(HIT_ROOT);
    host.unmount();
  });

  it("keeps the block under a still pointer lit, with its card, through new pictures", async () => {
    putView(fakeFrame());
    const host = mount(Host, { props: { viewId: VIEW } });
    await host.find(".treemap").trigger("mousemove", at(20, 50));
    expect(instance.setHover).toHaveBeenLastCalledWith(0);
    for (let i = 0; i < 3; i++) {
      instance.setHover.mockClear();
      putView(fakeFrame()); // a file change redraws the view: the hover key stays null
      await nextTick();
      expect(instance.setHover).toHaveBeenLastCalledWith(0);
      expect((host.emitted("hover")!.at(-1) as [HoverInfo])[0]).toMatchObject({ target: 0, pointer: { x: 20, y: 50 } });
    }
    host.unmount();
  });

  it("answers where a row is while mounted", async () => {
    putView(fakeFrame());
    const host = mount(Host, { props: { viewId: VIEW } });
    await nextTick();
    const handle = hosts.get(VIEW)!;
    expect(handle.element.classList.contains("treemap")).toBe(true);
    expect(handle.boundsCss(0)).toEqual({ x: 0, y: 10, w: 50, h: 90 });
    expect(handle.boundsCss(HIT_NONE)).toBeNull();
    // beside a block: near its top
    expect(handle.menuAnchor(0)).toMatchObject({ x: 25, y: 34 });
    host.unmount();
    expect(hosts.has(VIEW)).toBe(false);
  });
});

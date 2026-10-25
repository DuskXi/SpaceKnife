// Round 5 (docs/plugins.md 「试写第四轮之后的修正」): plugin shot acts on what is drawn on the canvas
// (click:node=a/b …: the page says where a node is drawn), and the plugins page names every kind of
// contribution.

import { afterEach, describe, expect, it } from "vitest";

import type { ScanStatus } from "@/protocol/generated";
import { type AppState, get, set } from "@/state/store";
import { devHooks } from "@/state/devlink";
import { hosts } from "@/visual/hosts";
import { pointOf } from "@/visual/pointOf";
import { shapesGeometry } from "@/visual/shapes/geometry";
import { rectScene, shapesScene } from "@/visual/testing/fixtures";
import { treemapGeometry } from "@/visual/treemap/geometry";
import { HIT_NONE, HIT_ROOT } from "@/visual/types";

const initial = get();
afterEach(() => {
  hosts.clear();
  set({ scans: {}, tabs: initial.tabs, active: initial.active, views: initial.views });
});

describe("a point that hits a row", () => {
  it("lies on the folder itself, not on the shapes drawn inside it", () => {
    const f = shapesScene().frame;
    const p = pointOf(shapesGeometry, f, 0); // sub, with a.mp4 and x.pdf inside
    expect(p).not.toBeNull();
    expect(shapesGeometry.hitTest(f, p!.x, p!.y)).toBe(0);
    const file = pointOf(shapesGeometry, f, 1);
    expect(shapesGeometry.hitTest(f, file!.x, file!.y)).toBe(1);
  });

  it("finds the zoom root between the shapes, and nothing for what is not drawn", () => {
    const f = shapesScene().frame;
    const root = pointOf(shapesGeometry, f, HIT_ROOT);
    expect(shapesGeometry.hitTest(f, root!.x, root!.y)).toBe(HIT_ROOT);
    expect(pointOf(shapesGeometry, shapesScene({ root: false }).frame, HIT_ROOT)).toBeNull();
    expect(pointOf(shapesGeometry, f, HIT_NONE)).toBeNull();
  });

  it("works for the built-in treemap: a folder's own header, not its children", () => {
    const f = rectScene().frame;
    const p = pointOf(treemapGeometry, f, 2); // sub, its files fill most of it
    expect(treemapGeometry.hitTest(f, p!.x, p!.y)).toBe(2);
    expect(pointOf(treemapGeometry, f, HIT_ROOT)).toBeNull(); // the zoom root fills the canvas: never hit
  });
});

describe("plugin shot's canvas steps", () => {
  const scan = { scan_id: "scan-1", roots: [{ path: "/data" }] } as unknown as ScanStatus;

  it("say why there is no point when no view of the scan is drawn", async () => {
    await expect(devHooks(scan).where("a/b")).rejects.toThrow();
    await expect(devHooks(scan).where("")).rejects.toThrow("no view of the scan is drawn");
  });

  it("ask the view's host: node= is the zoom root, a path its node", async () => {
    const asked: (number | null)[] = [];
    const host = {
      element: null as unknown as HTMLElement,
      menuAnchor: () => null,
      boundsCss: () => null,
      pointOfKey: (key: number | null) => (asked.push(key), key === null ? { x: 10, y: 20 } : null),
    };
    hosts.set("v1", host);
    const tab = { id: "t1", kind: "scan", viewIds: ["v1"], focusedPane: 0 };
    set({ tabs: [tab], active: "t1", views: { v1: { state: { scan_id: "scan-1" } } } } as unknown as Partial<AppState>);
    expect(await devHooks(scan).where("")).toEqual({ x: 10, y: 20 });
    expect(asked).toEqual([null]);
    hosts.set("v1", { ...host, pointOfKey: () => null });
    await expect(devHooks(scan).where("")).rejects.toThrow("no place of its own");
  });
});

// @vitest-environment happy-dom
// The result list: pages fetched for the visible rows (and only those), a new frame or order drops
// the pages, rows rendered from them, and the keyboard (move, zoom, basket).

import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";

import { PAGE, ROW_H } from "@/lib/results";
import type { ResultRow, ViewResultsParams } from "@/protocol/generated";
import { get, resetState, set, type ViewData } from "@/state/store";

const TOTAL = 250;
const row = (i: number): ResultRow => ({ alloc: 1000 - i, count: 1, key: 1000 + i, kind: i % 2 ? "folder" : "file", name: `f${i}.txt`, node: 500 + i, path: "dir", size: 1, tag: null, tag_inherited: false });

const api = vi.hoisted(() => ({
  requests: [] as ViewResultsParams[],
  cancels: 0,
}));
vi.mock("@/state/actions", () => ({
  cancellable: (_method: string, params: ViewResultsParams) => {
    api.requests.push(params);
    const offset = params.offset ?? 0;
    const rows = Array.from({ length: Math.max(0, Math.min(params.limit ?? PAGE, TOTAL - offset)) }, (_, k) => row(offset + k));
    return { promise: Promise.resolve({ active: true, alloc: 0, files: TOTAL, max_alloc: 1000, offset, total: TOTAL, rows }), cancel: () => api.cancels++ };
  },
  viewCall: vi.fn(async () => undefined),
  basketAdd: vi.fn(async () => undefined),
  clearFilter: vi.fn(),
}));

import { basketAdd, viewCall } from "@/state/actions";

import Results from "./Results.vue";

const VIEW = "view-1";

function putView(seq = 1, sort = "size"): void {
  const view = {
    state: { scan_id: "s1", results_sort: sort },
    info: { seq, scanned: 10_000, breadcrumb: [{ key: 1, label: "C:\\" }] },
    frame: null,
    filter: { active: true, files: TOTAL, alloc: 5000 },
    parse: null,
    filterText: "",
    hoverKey: null,
  } as unknown as ViewData;
  set({ views: { ...get().views, [VIEW]: view } });
}

describe("Results", () => {
  beforeEach(() => {
    resetState();
    api.requests.length = 0;
    api.cancels = 0;
    vi.clearAllMocks();
    putView();
  });
  afterEach(() => vi.unstubAllGlobals());

  it("fetches the first page and renders the visible rows", async () => {
    const list = mount(Results, { props: { viewId: VIEW } });
    expect(api.requests).toEqual([{ view_id: VIEW, sort: "size", offset: 0, limit: PAGE }]);
    await flushPromises();
    const rows = list.findAll(".res-row");
    // 20 visible + the overscan below
    expect(rows).toHaveLength(26);
    expect(rows[0]!.attributes("id")).toBe(`${VIEW}-res-0`);
    expect(rows[0]!.find(".res-label").text()).toBe("f0.txt");
    expect(rows[1]!.find(".res-n").text()).toBe("1 项");
    expect(list.find(".res-spacer").attributes("style")).toContain(`height: ${TOTAL * ROW_H}px`);
    list.unmount();
  });

  it("fetches the next page only when the scroll reaches it", async () => {
    const list = mount(Results, { props: { viewId: VIEW } });
    await flushPromises();
    const el = list.find(".res-list").element as HTMLDivElement;
    el.scrollTop = 40 * ROW_H;
    await list.find(".res-list").trigger("scroll");
    await flushPromises();
    expect(api.requests).toHaveLength(1); // rows 34–47: still page 0
    el.scrollTop = 120 * ROW_H;
    await list.find(".res-list").trigger("scroll");
    await flushPromises();
    expect(api.requests.map((r) => r.offset)).toEqual([0, PAGE]);
    expect(list.find(`#${VIEW}-res-120 .res-label`).text()).toBe("f120.txt");
    list.unmount();
  });

  it("drops the pages of an older frame or order", async () => {
    const list = mount(Results, { props: { viewId: VIEW } });
    await flushPromises();
    putView(2);
    await flushPromises();
    expect(api.requests.map((r) => r.offset)).toEqual([0, 0]);
    // the order: the menu, then 「按名称」
    await list.find(".res-sort > button").trigger("click");
    await list.findAll(".res-sort .menu-item")[1]!.trigger("click");
    await flushPromises();
    expect(api.requests.at(-1)).toMatchObject({ sort: "name", offset: 0 });
    expect(get().views[VIEW]!.state.results_sort).toBe("name");
    list.unmount();
  });

  it("moves with the keyboard, highlighting the row in the picture", async () => {
    const list = mount(Results, { props: { viewId: VIEW } });
    await flushPromises();
    const box = list.find(".res-list");
    await box.trigger("keydown", { key: "ArrowDown" });
    expect(box.attributes("aria-activedescendant")).toBe(`${VIEW}-res-0`);
    expect(get().views[VIEW]!.hoverKey).toBe(1000);
    await box.trigger("keydown", { key: "ArrowDown" });
    expect(get().views[VIEW]!.hoverKey).toBe(1001);
    await box.trigger("keydown", { key: "Enter" });
    expect(viewCall).toHaveBeenLastCalledWith(VIEW, "view.zoom", { key: 1001 });
    await box.trigger("keydown", { key: "Delete" });
    expect(basketAdd).toHaveBeenLastCalledWith("s1", 501);
    await box.trigger("keydown", { key: " " });
    expect(viewCall).toHaveBeenLastCalledWith(VIEW, "view.reveal", { key: 1001 });
    // view shortcuts pass through
    await box.trigger("keydown", { key: "ArrowUp", altKey: true });
    expect(box.attributes("aria-activedescendant")).toBe(`${VIEW}-res-1`);
    await box.trigger("keydown", { key: "Home" });
    expect(box.attributes("aria-activedescendant")).toBe(`${VIEW}-res-0`);
    await box.trigger("keydown", { key: "End" });
    await nextTick();
    expect(box.attributes("aria-activedescendant")).toBe(`${VIEW}-res-${TOTAL - 1}`);
    list.unmount();
    // leaving takes the highlight away and drops what is still on its way
    expect(get().views[VIEW]!.hoverKey).toBeNull();
  });
});

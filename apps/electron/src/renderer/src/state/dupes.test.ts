// The duplicate-file search as the interface keeps it (docs/dupes.md §10): the list that keeps its
// length and place while results come in, the group lit in every pane (dupes.locate after the
// pointer settles, at most 2000 copies), the settings-backed keep rules, the cross-volume switch,
// the basket hand-off and picking up a search after a reconnect.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DupesPlan } from "@/protocol/generated";

import { call, updateSettings, viewCall } from "./actions";
import {
  applyKeep,
  askFolder,
  cannotSearch,
  crossIds,
  dupesOffered,
  focusGroup,
  getDupes,
  keepOptions,
  LIT_MAX,
  loadGroups,
  onDupesChanged,
  onDupesProgress,
  openDrawer,
  pickUp,
  resetDupes,
  revealCopy,
  setDupes,
  startSearch,
} from "./dupes";
import { get, set } from "./store";
import { group, scan, status } from "./testing/dupes";

// not the real module: it imports this one back, so a partial mock would hand dupes.ts the real call
vi.mock(import("./actions"), () => ({ call: vi.fn(), updateSettings: vi.fn(async () => true), viewCall: vi.fn(async () => null) }) as never);

const mocked = vi.mocked(call);

const page = (ids: number[], total = ids.length, version = 1) => ({ version, total, items: ids.map((i) => group(i)) });

beforeEach(() => {
  resetDupes();
  set({
    tabs: [{ id: "t1", kind: "scan", scanId: "s1", viewIds: ["v1", "v2"], focusedPane: 0, title: "C:" }],
    active: "t1",
    scans: { s1: scan("s1"), s2: scan("s2"), snap: scan("snap", { source: "snapshot", method: "snapshot" }) },
    settings: null,
    toast: null,
    basketOpen: false,
  });
});
afterEach(() => {
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("the group list", () => {
  it("keeps its length and order while results come in, and pages on", async () => {
    mocked.mockResolvedValueOnce(page([1, 2, 3], 250) as never);
    await loadGroups("dupes-1", "reset");
    expect(mocked.mock.calls[0]).toEqual(["dupes.groups", expect.objectContaining({ job_id: "dupes-1", offset: 0, limit: 100, sort: "reclaim" })]);
    // the user paged on to 103 groups
    setDupes({ groups: Array.from({ length: 103 }, (_, i) => group(i)) });
    mocked.mockResolvedValueOnce(page([7, 1, 2], 251, 2) as never);
    await loadGroups("dupes-1", "refresh");
    // as many as shown come again, in the new order: a new group slots in, nothing is dropped
    expect(mocked.mock.calls[1]![1]).toMatchObject({ offset: 0, limit: 103 });
    expect(getDupes().groups.map((g) => g.id)).toEqual([7, 1, 2]);
    expect(getDupes().groupsVersion).toBe(2);
    mocked.mockResolvedValueOnce(page([9], 251, 2) as never);
    await loadGroups("dupes-1", "more");
    expect(mocked.mock.calls[2]![1]).toMatchObject({ offset: 3, limit: 100 });
    expect(getDupes().groups.map((g) => g.id)).toEqual([7, 1, 2, 9]);
  });

  it("brings every group shown back on a refresh, past 500, in pages", async () => {
    setDupes({ groupsJob: "dupes-1", groups: Array.from({ length: 700 }, (_, i) => group(i)), groupsTotal: 900 });
    mocked.mockResolvedValueOnce(page(Array.from({ length: 500 }, (_, i) => i), 900, 3) as never);
    mocked.mockResolvedValueOnce(page(Array.from({ length: 200 }, (_, i) => 500 + i), 900, 3) as never);
    await loadGroups("dupes-1", "refresh");
    expect(mocked.mock.calls.map((c) => [(c[1] as { offset: number }).offset, (c[1] as { limit: number }).limit])).toEqual([
      [0, 500],
      [500, 200],
    ]);
    expect(getDupes().groups).toHaveLength(700); // the list keeps its length (UI-WD-04)
  });

  it("filters by name or path and by the folder in view", async () => {
    set({ views: { v1: { state: { scan_id: "s1", zoom_node: 42 } } } as never });
    setDupes({ query: " photo ", underZoom: true });
    mocked.mockResolvedValueOnce(page([1]) as never);
    await loadGroups("dupes-1", "reset");
    expect(mocked.mock.calls[0]![1]).toMatchObject({ query: "photo", scan_id: "s1", under: 42 });
  });

  it("only takes the scans open in a tab into a cross-volume search", () => {
    set({ scans: { ...get().scans, old: scan("old") } }); // finished, but its tab is closed
    expect(crossIds("s1")).toEqual(["s1"]);
    set({ tabs: [...get().tabs, { id: "t2", kind: "scan", scanId: "s2", viewIds: ["v3"], focusedPane: 0, title: "D:" }] });
    expect(crossIds("s1")).toEqual(["s1", "s2"]);
  });

  it("drops an answer a newer request overtook", async () => {
    let first!: (v: unknown) => void;
    mocked.mockImplementationOnce(() => new Promise((r) => (first = r)) as never);
    const slow = loadGroups("dupes-1", "reset");
    mocked.mockResolvedValueOnce(page([5]) as never);
    await loadGroups("dupes-1", "reset");
    first(page([1, 2]));
    await slow;
    expect(getDupes().groups.map((g) => g.id)).toEqual([5]);
  });

  it("refreshes on dupes.changed only while the drawer shows that search", async () => {
    onDupesProgress(status());
    onDupesChanged({ job_id: "dupes-1", version: 2, reason: "verified" });
    expect(mocked).not.toHaveBeenCalled(); // drawer closed
    setDupes({ open: true, groupsJob: "dupes-1" });
    mocked.mockResolvedValue(page([1]) as never);
    onDupesChanged({ job_id: "dupes-9", version: 2, reason: "verified" });
    expect(mocked).not.toHaveBeenCalled(); // another search
    onDupesChanged({ job_id: "dupes-1", version: 3, reason: "verified" });
    expect(mocked.mock.calls[0]![0]).toBe("dupes.groups");
  });

  it("forgets a cleared search everywhere, and an older one a new search replaced", () => {
    onDupesProgress(status({ scan_ids: ["s1", "s2"], cross_volume: true }));
    setDupes({ groups: [group(1)], groupsJob: "dupes-1", focus: { jobId: "dupes-1", group: 1 } });
    onDupesProgress(status({ job_id: "dupes-2", scan_ids: ["s1"] }));
    expect(getDupes().byScan.s1!.job_id).toBe("dupes-2");
    expect(getDupes().byScan.s2).toBeUndefined(); // the replaced cross-volume search left s2 too
    onDupesChanged({ job_id: "dupes-1", version: 0, reason: "cleared" });
    expect(getDupes().groups).toEqual([]);
    expect(getDupes().focus).toBeNull();
  });
});

describe("lighting a group in every pane", () => {
  it("lights at once and asks where hidden copies are once the pointer settles", async () => {
    vi.useFakeTimers();
    mocked.mockResolvedValue({ seq: 4, rows: [{ row: 3, exact: false, role: "extra" }], truncated: true } as never);
    focusGroup("dupes-1", 7, ["v1", "v2"]);
    expect(getDupes().focus).toEqual({ jobId: "dupes-1", group: 7 });
    expect(mocked).not.toHaveBeenCalled();
    focusGroup("dupes-1", 8, ["v1", "v2"]); // the pointer moved on: only the last one is asked
    await vi.advanceTimersByTimeAsync(100);
    expect(mocked.mock.calls).toEqual([
      ["dupes.locate", { job_id: "dupes-1", view_id: "v1", group: 8, limit: LIT_MAX }],
      ["dupes.locate", { job_id: "dupes-1", view_id: "v2", group: 8, limit: LIT_MAX }],
    ]);
    expect(LIT_MAX).toBe(2000);
    expect(getDupes().located.v1).toEqual({ seq: 4, rows: [{ row: 3, exact: false, role: "extra" }], truncated: true });
    focusGroup(null, null);
    expect(getDupes().focus).toBeNull();
    expect(getDupes().located).toEqual({});
  });

  it("ignores locate answers for a group no longer lit", async () => {
    vi.useFakeTimers();
    let answer!: (v: unknown) => void;
    mocked.mockImplementation(() => new Promise((r) => (answer = r)) as never);
    focusGroup("dupes-1", 1, ["v1"]);
    await vi.advanceTimersByTimeAsync(100);
    focusGroup(null, null);
    answer({ seq: 1, rows: [{ row: 0, exact: true, role: "keep" }], truncated: false });
    await vi.runAllTimersAsync();
    expect(getDupes().located).toEqual({});
  });

  it("finds a copy in a view with view.reveal", async () => {
    await revealCopy("v1", 42);
    expect(vi.mocked(viewCall)).toHaveBeenCalledWith("v1", "view.reveal", { key: 42 });
  });
});

describe("starting a search", () => {
  it("refuses snapshots and unfinished scans, saying why", async () => {
    expect(cannotSearch("snap")).toBe("快照是过去的状态，不能核对内容");
    set({ scans: { ...get().scans, s3: scan("s3", { state: "scanning" }) } });
    expect(cannotSearch("s3")).toBe("扫描完成后才能查找重复文件");
    expect(cannotSearch("s1")).toBe("");
    await startSearch("snap");
    expect(mocked).not.toHaveBeenCalled();
    expect(get().toast).toBe("快照是过去的状态，不能核对内容");
  });

  it("compares one scan, or with 「跨盘比较」 every open finished live scan", async () => {
    mocked.mockResolvedValue(status() as never);
    await startSearch("s1");
    expect(mocked.mock.calls[0]![1]).toMatchObject({ scope: { scan_ids: ["s1"], cross_volume: false, mode: "all", min_size: 1 << 20 } });
    expect(getDupes().open).toBe(true);
    set({ settings: { values: { "dupes.cross_volume": true, "dupes.min_size": 4 } } as never });
    expect(crossIds("s2")).toEqual(["s2", "s1"]); // the snapshot is not one
    await startSearch("s2");
    expect(mocked.mock.calls[1]![1]).toMatchObject({ scope: { scan_ids: ["s2", "s1"], cross_volume: true, min_size: 4 << 20 } });
    // a file's own question stays in its scan
    await startSearch("s1", { mode: "file", node: 5 });
    expect(mocked.mock.calls[2]![1]).toMatchObject({ scope: { scan_ids: ["s1"], cross_volume: false, mode: "file", node: 5 } });
  });

  it("answers 「这个文件夹在别处有没有？」 from a finished whole-scan search", async () => {
    onDupesProgress(status());
    mocked.mockResolvedValue({ node: 9, redundant: 0 } as never);
    await askFolder("s1", 9, "photos");
    expect(mocked.mock.calls).toEqual([["dupes.folder", { job_id: "dupes-1", scan_id: "s1", node: 9 }]]);
    expect(getDupes().coverage["dupes-1"]).toMatchObject({ node: 9 });
    expect(getDupes().open).toBe(true);
    // without one: a search of the folder's own
    resetDupes();
    mocked.mockResolvedValue(status({ job_id: "dupes-2", mode: "against", node: 9, state: "planning" }) as never);
    await askFolder("s1", 9);
    expect(mocked.mock.calls[1]![0]).toBe("dupes.start");
    expect(mocked.mock.calls[1]![1]).toMatchObject({ scope: { mode: "against", node: 9 } });
  });
});

describe("the 「重复」 colouring", () => {
  it("is offered once a search covers the scan, before the next view state says so", () => {
    expect(dupesOffered({ scan_id: "s1", has_dupes: false })).toBe(false);
    expect(dupesOffered({ scan_id: "s1", has_dupes: true })).toBe(true);
    onDupesProgress(status({ state: "verifying" }));
    expect(dupesOffered({ scan_id: "s1", has_dupes: false })).toBe(true);
    expect(dupesOffered({ scan_id: "s2", has_dupes: false })).toBe(false);
    expect(dupesOffered(undefined)).toBe(false);
  });
});

describe("keep rules", () => {
  it("are the settings, changed from the drawer and applied at once", async () => {
    expect(keepOptions()).toEqual({ rule: "oldest", folderConsistent: true, perVolume: true, cross: false });
    set({ settings: { values: { "dupes.keep_rule": "shortest", "dupes.folder_consistent": false } } as never });
    expect(keepOptions().rule).toBe("shortest");
    mocked.mockImplementation((async (m: string) => (m === "dupes.keep" ? status() : page([]))) as never);
    await applyKeep("dupes-1", { rule: "newest" });
    expect(vi.mocked(updateSettings)).toHaveBeenCalledWith({ "dupes.keep_rule": "newest" });
    // no overrides: the backend keeps the manual choices made so far
    expect(mocked.mock.calls[0]).toEqual(["dupes.keep", { job_id: "dupes-1", rule: "shortest", prefer: [], folder_consistent: false, per_volume: true, overrides: [], reset_overrides: false }]);
  });
});

describe("after a reconnect", () => {
  it("picks the backend's search up once, through the plan", async () => {
    const plan = { candidates: 1, groups: 1, bytes: 1, probe_bytes: 1, full_bytes_max: 1, reclaim_max: 1, excluded: {}, current: status({ job_id: "dupes-7" }) } as DupesPlan;
    mocked.mockResolvedValue(plan as never);
    pickUp("s1", false);
    expect(mocked).not.toHaveBeenCalled();
    pickUp("s1", true);
    pickUp("s1", true);
    await vi.waitFor(() => expect(getDupes().byScan.s1?.job_id).toBe("dupes-7"));
    expect(mocked).toHaveBeenCalledTimes(1);
  });

  it("opening the drawer without a search shows the plan", async () => {
    mocked.mockResolvedValue({ candidates: 3, groups: 1, bytes: 1, probe_bytes: 1, full_bytes_max: 1, reclaim_max: 1, excluded: {} } as never);
    set({ basketOpen: true });
    openDrawer();
    expect(get().basketOpen).toBe(false);
    await vi.waitFor(() => expect(getDupes().plan.s1?.candidates).toBe(3));
    expect(mocked.mock.calls[0]![0]).toBe("dupes.plan");
  });
});

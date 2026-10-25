// A plugin opens a comparison (a command's CompareOpen, a panel's sk.host.compare): in the tab that
// shows the snapshots' folders, scanning them first for "now", opening the target for two
// snapshots; snapshots.changed refreshes the lists of the tabs that compare.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { runAction } from "@/plugins/actions";
import { callPlugin } from "@/plugins/client";

import { call, openSnapshot, startScan } from "./actions";
import { onSnapshotsChanged, openCompare } from "./compare";
import { get, set } from "./store";

// without the original: it loads ./compare itself, which would then hold the original call
vi.mock(import("./actions"), () => ({
  call: vi.fn(),
  openSnapshot: vi.fn(),
  startScan: vi.fn(),
  openView: vi.fn(),
  setViewSettings: vi.fn(),
  viewCall: vi.fn(),
  basketAdd: vi.fn(),
  editFilter: vi.fn(),
  updateSettings: vi.fn(),
  openSettingsTab: vi.fn(),
}) as never);
vi.mock(import("@/plugins/client"), async (original) => ({ ...(await original()), callPlugin: vi.fn() }));

const SNAP = { path: "/snaps/data/2026-10-01.sks", created_at: "2026-10-01T00:00:00Z", roots: ["/data"], node_count: 3, bytes_alloc: 10, files: 1, file_bytes: 1, method: null };
const OLD = { ...SNAP, path: "/snaps/data/2026-09-01.sks" };
const sources = { snapshots: [SNAP, OLD], now: { kind: "now", path: null, created_at: "2026-10-25T00:00:00Z", bytes_alloc: 10, node_count: 3 }, now_note: "" };

function answer(method: string): unknown {
  if (method === "snapshot.list") return { snapshots: [SNAP, OLD] };
  if (method === "compare.sources") return sources;
  if (method === "compare.open") return { compare_id: "cmp-1" };
  return {};
}

beforeEach(() => {
  vi.stubGlobal("window", { sk: { platform: "linux" } });
  vi.mocked(call).mockImplementation(async (method: string) => answer(method) as never);
  set({
    tabs: [
      { id: "start", kind: "start", viewIds: [], focusedPane: 0, title: "" },
      { id: "t1", kind: "scan", scanId: "s1", viewIds: ["v1"], focusedPane: 0, title: "data" },
    ] as never,
    active: "start",
    scans: { s1: { scan_id: "s1", state: "done", source: "scan", roots: [{ path: "/data" }], snapshot: null } } as never,
    toast: null,
    plugins: { plugins: [{ id: "a.b", name: "A", granted: ["snapshots.read"] }], folder: "", dev_mode: false } as never,
    snapshotsChanged: { n: 0, saved: [], removed: [] },
  });
});
afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

const opened = () => vi.mocked(call).mock.calls.filter(([m]) => m === "compare.open").map(([, p]) => p);

describe("openCompare", () => {
  it("compares in the tab that shows the snapshots' folders, with those ends", async () => {
    await openCompare({ base: OLD.path, target: SNAP.path });
    expect(get().active).toBe("t1");
    expect(opened()).toEqual([{ scan_id: "s1", base: OLD.path, target: SNAP.path }]);
    expect(get().tabs.find((t) => t.id === "t1")?.compare).toMatchObject({ base: OLD.path, target: SNAP.path });
  });

  it("scans the folders first for 'now' when no tab shows them", async () => {
    set({ tabs: get().tabs.filter((t) => t.id !== "t1"), scans: {} });
    vi.mocked(startScan).mockResolvedValue("s9");
    await openCompare({ base: SNAP.path, target: "now" });
    expect(startScan).toHaveBeenCalledWith({ roots: ["/data"] });
    expect(get().toast).toBe("扫描完成后会自动与这个快照对比");
  });

  it("opens the target of two snapshots when no tab shows their folders", async () => {
    set({ tabs: get().tabs.filter((t) => t.id !== "t1"), scans: {} });
    vi.mocked(openSnapshot).mockImplementation(async () => {
      set((s) => ({
        tabs: [...s.tabs, { id: "t2", kind: "scan", scanId: "s2", viewIds: [], focusedPane: 0, title: "data" }] as never,
        active: "t2",
        scans: { s2: { scan_id: "s2", state: "done", source: "snapshot", roots: [{ path: "/data" }], snapshot: SNAP.path } } as never,
      }));
    });
    await openCompare({ base: OLD.path, target: SNAP.path });
    expect(openSnapshot).toHaveBeenCalledWith(SNAP.path);
    expect(opened()).toEqual([{ scan_id: "s2", base: OLD.path, target: "now" }]);
  });

  it("a command's method answering CompareOpen opens it (with snapshots.read)", async () => {
    vi.mocked(callPlugin).mockResolvedValue({ type: "compare", base: OLD.path, target: "now", scan_id: "s1" });
    await runAction({ kind: "call", method: "plugin.a.b.compare_last", args: {} }, { owner: "a.b" });
    expect(opened()).toEqual([{ scan_id: "s1", base: OLD.path, target: "now" }]);
    set({ plugins: { plugins: [{ id: "a.b", name: "A", granted: [] }], folder: "", dev_mode: false } as never });
    vi.mocked(call).mockClear();
    await runAction({ kind: "call", method: "plugin.a.b.compare_last", args: {} }, { owner: "a.b" });
    expect(opened()).toEqual([]);
    expect(get().toast).toContain("snapshots.read");
  });
});

describe("snapshots.changed", () => {
  it("is counted for panels and refreshes the lists of tabs that compare", async () => {
    await openCompare({ base: OLD.path, target: SNAP.path });
    vi.mocked(call).mockClear();
    onSnapshotsChanged({ saved: ["/snaps/data/new.sks"], removed: [OLD.path] });
    expect(get().snapshotsChanged).toEqual({ n: 1, saved: ["/snaps/data/new.sks"], removed: [OLD.path] });
    await vi.waitFor(() => expect(vi.mocked(call).mock.calls.map(([m]) => m)).toContain("compare.sources"));
  });
});

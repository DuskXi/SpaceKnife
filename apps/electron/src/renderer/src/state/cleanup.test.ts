// The cleanup plan's state: entering cleanup mode, edits sent with their version (a newer plan
// refuses them), bulk edits only after a preview, running only after approval and the last
// confirmation, no key that runs, undoes or empties anything.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { call } from "./actions";
import {
  bulkAction,
  cleanupKey,
  commitPreview,
  confirmExecute,
  confirmPurge,
  confirmUndo,
  diffLine,
  edit,
  getCleanup,
  groupAction,
  loadGroups,
  onCleanupProgress,
  openCleanup,
  requestExecute,
  requestPurge,
  requestUndo,
  resetCleanup,
  setCleanup,
} from "./cleanup";
import { get, set } from "./store";
import { cstate, group } from "./testing/cleanup";

vi.mock(import("./actions"), () => ({ call: vi.fn(), client: () => ({ hello: { capabilities: ["dupes.v1", "dupes.cleanup.v1"] } }), setViewSettings: vi.fn(async () => undefined) }) as never);
const mocked = vi.mocked(call);
const page = (ids: number[]) => ({ version: 3, total: ids.length, offset: 0, items: ids.map((i) => group(i)) });
const detail = (id: number) => ({ version: 3, group: group(id), copies: [], out_reason: "" });

function answer(m: Record<string, unknown>) {
  mocked.mockImplementation((async (method: string) => {
    if (method in m) {
      const v = m[method];
      if (v instanceof Error) throw v;
      return typeof v === "function" ? (v as () => unknown)() : v;
    }
    if (method === "dupes.cleanup_groups") return page([1, 2]);
    if (method === "dupes.cleanup_group") return detail(1);
    if (method === "dupes.cleanup_state") return cstate();
    return null;
  }) as never);
}

beforeEach(() => {
  resetCleanup();
  set({ tabs: [{ id: "t1", kind: "scan", scanId: "s1", viewIds: ["v1"], focusedPane: 0, title: "C:" }], active: "t1", toast: null });
});
afterEach(() => vi.clearAllMocks());

describe("cleanup mode", () => {
  it("opens the plan of a search in the tab and loads its groups", async () => {
    answer({ "dupes.cleanup_open": cstate() });
    await openCleanup("t1", "dupes-1");
    expect(mocked.mock.calls[0]).toEqual(["dupes.cleanup_open", { job_id: "dupes-1", preset: null, backup_volume: "" }]);
    expect(get().tabs[0]!.cleanup).toEqual({ planId: "plan-1", jobId: "dupes-1" });
    expect(getCleanup().groups.map((g) => g.id)).toEqual([1, 2]);
    expect(getCleanup().focus).toBe(1);
  });

  it("offers saved edits when there are some", async () => {
    answer({ "dupes.cleanup_open": cstate({ resume_offer: { saved_at: "2026-10-20T10:00:00", manual: 37, rules: 2 } }) });
    await openCleanup("t1", "dupes-1");
    expect(getCleanup().dialog).toEqual({ kind: "resume" });
  });
});

describe("edits", () => {
  beforeEach(() => setCleanup({ tabId: "t1", planId: "plan-1", jobId: "dupes-1", state: cstate() }));

  it("go with the version they were made on", async () => {
    answer({ "dupes.cleanup_edit": cstate({ version: 4 }) });
    await edit({ op: "keep_add", scope: { copies: [{ group: 1, path: "C:\\copy\\f1.jpg" }] } });
    expect(mocked.mock.calls[0]).toEqual(["dupes.cleanup_edit", { plan_id: "plan-1", base_version: 3, edit: { op: "keep_add", scope: { copies: [{ group: 1, path: "C:\\copy\\f1.jpg" }] } } }]);
    expect(getCleanup().state!.version).toBe(4);
  });

  it("refused (a newer plan, the last keeper): said, and the plan is read again", async () => {
    answer({ "dupes.cleanup_edit": new Error("每组至少留一份：先选另一份做保留点") });
    expect(await edit({ op: "keep_remove", scope: { copies: [{ group: 1, path: "C:\\k\\f1.jpg" }] } })).toBe(false);
    expect(get().toast).toBe("每组至少留一份：先选另一份做保留点");
    expect(mocked.mock.calls.some((c) => c[0] === "dupes.cleanup_state")).toBe(true);
  });

  it("in bulk are previewed first and kept only from the preview", async () => {
    const diff = { version: 3, before: { trash: 2 }, after: { hardlink: 2 }, freed_before: 1, freed_after: 1, unavailable: {}, risks_added: {}, risks_removed: {}, risk_titles: {}, protected: 1, changed: 2, examples: [], refused: "" };
    answer({ "dupes.cleanup_preview": diff, "dupes.cleanup_edit": cstate({ version: 4 }) });
    await bulkAction("hardlink");
    expect(mocked.mock.calls.map((c) => c[0])).toEqual(["dupes.cleanup_preview"]);
    expect(getCleanup().dialog?.kind).toBe("preview");
    await commitPreview(true);
    const sent = mocked.mock.calls.find((c) => c[0] === "dupes.cleanup_edit")!;
    expect((sent[1] as { edit: { op: string; action: string; include_overridden: boolean } }).edit).toMatchObject({ op: "action_set", action: "hardlink", include_overridden: true });
    // the receipt says what changed, and the way back
    expect(getCleanup().receipt).toBe("已改 2 份（2 组） · Ctrl+Z 撤回");
  });

  it("the receipt of a preview says what a risk rule held back", () => {
    const diff = { version: 3, before: { trash: 3 }, after: { trash: 1, leave: 2 }, freed_before: 3, freed_after: 1, unavailable: { risk_rule: 2 }, risks_added: {}, risks_removed: {}, risk_titles: {}, protected: 0, changed: 2, examples: [], refused: "" };
    expect(diffLine(diff)).toBe("已改 2 份（2 组），其中 2 份有高风险，按规则改成了不动");
  });

  it("a single key says so when no copy could take the action", async () => {
    setCleanup({ groups: [group(1)], focus: 1 });
    answer({ "dupes.cleanup_edit": cstate({ version: 4 }), "dupes.cleanup_groups": page([1]) });
    await groupAction(1, "hardlink");
    expect(getCleanup().receipt).toContain("这一组没有能换成硬链接的份");
  });
});

describe("the group list", () => {
  beforeEach(() => setCleanup({ tabId: "t1", planId: "plan-1", jobId: "dupes-1", state: cstate(), focus: 7 }));

  it("keeps the terms the filter left out, and the inspector follows the list", async () => {
    answer({ "dupes.cleanup_groups": { ...page([1, 2]), problems: ["看不懂「size:>>2MB」"] } });
    await loadGroups();
    expect(getCleanup().problems).toEqual(["看不懂「size:>>2MB」"]);
    expect(getCleanup().focus).toBe(1); // group 7 is not in the list any more
  });

  it("? shows the keys, Ctrl+A ticks the groups shown", () => {
    setCleanup({ groups: [group(1), group(2)] });
    expect(cleanupKey({ key: "?", shiftKey: true, ctrlKey: false, metaKey: false })).toBe(true);
    expect(getCleanup().keysHelp).toBe(true);
    cleanupKey({ key: "a", shiftKey: false, ctrlKey: true, metaKey: false });
    expect(getCleanup().selected).toEqual([1, 2]);
  });
});

describe("running", () => {
  beforeEach(() => setCleanup({ tabId: "t1", planId: "plan-1", jobId: "dupes-1", state: cstate(), groups: [group(1)], focus: 1 }));

  it("needs the confirmation; it approves first when there is no token", async () => {
    answer({ "dupes.cleanup_approve": { token: "0123456789abcdef", version: 3, steps: 2, by_action: {}, acks: {}, at: "" }, "dupes.cleanup_execute": { task_id: "cleanup-1", kind: "execute", run_id: "r-1", total: 2, delay_s: 0 } });
    requestExecute();
    expect(getCleanup().dialog).toEqual({ kind: "execute" });
    expect(mocked).not.toHaveBeenCalled();
    await confirmExecute();
    const methods = mocked.mock.calls.map((c) => c[0]);
    expect(methods.indexOf("dupes.cleanup_approve")).toBeLessThan(methods.indexOf("dupes.cleanup_execute"));
    expect(getCleanup().view).toBe("run");
  });

  it("asks again when the preflight dropped steps — once, and without a 「完成 0 项」 toast", async () => {
    answer({ "dupes.cleanup_runs": { total: 0, items: [] } });
    setCleanup({ task: { task_id: "cleanup-1", kind: "execute", phase: "preflight", state: "preflight", done: 0, total: 2, bytes: 0, current: "", counts: {}, freed_now: 0, freed_later: 0, run_id: "r-1", plan_id: "plan-1", message: "", skipped: [], needs_confirm: false } });
    const held = { task_id: "cleanup-1", kind: "execute" as const, phase: "confirm", state: "waiting" as const, done: 0, total: 2, bytes: 0, current: "", counts: {}, freed_now: 0, freed_later: 0, run_id: "r-1", plan_id: "plan-1", message: "", skipped: [{ code: "skipped_changed", text: "变了", count: 1, examples: ["C:\\copy\\f1.jpg"] }], needs_confirm: true };
    onCleanupProgress(held);
    expect(getCleanup().dialog?.kind).toBe("drops");
    setCleanup({ dialog: null }); // 「取消」
    onCleanupProgress({ ...held, state: "stopped" }); // the held run ends right after: not asked again
    expect(getCleanup().dialog).toBe(null);
    expect(get().toast).toBe(null);
  });

  it("「执行其余的」 waits for the held run to end and reads the plan again first", async () => {
    const approval = { token: "0123456789abcdef", version: 3, steps: 2, by_action: {}, acks: {}, at: "" };
    answer({ "dupes.cleanup_state": cstate({ approval }), "dupes.cleanup_execute": { task_id: "cleanup-2", kind: "execute", run_id: "r-2", total: 2, delay_s: 0 } });
    const held = { task_id: "cleanup-1", kind: "execute" as const, phase: "confirm", state: "waiting" as const, done: 0, total: 2, bytes: 0, current: "", counts: {}, freed_now: 0, freed_later: 0, run_id: "r-1", plan_id: "plan-1", message: "", skipped: [], needs_confirm: true };
    setCleanup({ task: held, dialog: { kind: "drops", task: held } });
    const going = confirmExecute(true);
    setTimeout(() => onCleanupProgress({ ...held, state: "stopped" }), 150);
    await going;
    const methods = mocked.mock.calls.map((c) => c[0]);
    expect(methods).toEqual(["dupes.cleanup_runs", "dupes.cleanup_state", "dupes.cleanup_execute"]);
    expect(mocked.mock.calls.at(-1)![1]).toMatchObject({ token: approval.token, proceed_after_drops: true });
    expect(getCleanup().task?.task_id).toBe("cleanup-2");
  });

  it("no key runs, undoes or empties anything", () => {
    answer({});
    for (const key of [..."abcdefghijklmnopqrstuvwxyz0123456789", "Enter", " ", "Delete", "F5"]) {
      cleanupKey({ key, shiftKey: false, ctrlKey: false, metaKey: false });
      cleanupKey({ key, shiftKey: true, ctrlKey: false, metaKey: false });
      cleanupKey({ key, shiftKey: false, ctrlKey: true, metaKey: false });
    }
    const methods = new Set(mocked.mock.calls.map((c) => c[0] as string));
    for (const m of ["dupes.cleanup_execute", "dupes.cleanup_approve", "dupes.cleanup_undo", "dupes.cleanup_purge", "dupes.cleanup_preflight"]) expect(methods.has(m)).toBe(false);
  });

  it("single keys edit the focused group", async () => {
    answer({ "dupes.cleanup_edit": cstate({ version: 4 }) });
    cleanupKey({ key: "h", shiftKey: false, ctrlKey: false, metaKey: false });
    await vi.waitFor(() => expect(mocked.mock.calls.some((c) => c[0] === "dupes.cleanup_edit")).toBe(true));
    const sent = mocked.mock.calls.find((c) => c[0] === "dupes.cleanup_edit")![1] as { edit: unknown };
    expect(sent.edit).toEqual({ op: "action_set", action: "hardlink", scope: { groups: [1] } });
  });
});

describe("undo and emptying the recovery area", () => {
  it("each is its own confirmed step", async () => {
    answer({
      "dupes.cleanup_undo_plan": { run_id: "r-1", items: [{ seq: 1, path: "C:\\copy\\a", can: true, why: "", bytes: 1 }], can: 1, cannot: 0 },
      "dupes.cleanup_undo": { task_id: "cleanup-2", kind: "undo", run_id: "r-1", total: 1, delay_s: 0 },
      "dupes.cleanup_purge_plan": { run_id: "r-1", items: [], can: 1, cannot: 0, bytes: 5, streams: 0, in_trash: 0 },
      "dupes.cleanup_purge": { task_id: "cleanup-3", kind: "purge", run_id: "r-1", total: 1, delay_s: 5 },
    });
    await requestUndo("r-1");
    expect(mocked.mock.calls.map((c) => c[0])).toEqual(["dupes.cleanup_undo_plan"]);
    await confirmUndo(false);
    expect(mocked.mock.calls.at(-1)).toEqual(["dupes.cleanup_undo", { run_id: "r-1", seqs: null, beside: false }]);
    await requestPurge("r-1");
    expect(getCleanup().dialog?.kind).toBe("purge");
    expect(mocked.mock.calls.some((c) => c[0] === "dupes.cleanup_purge")).toBe(false);
    await confirmPurge();
    expect(mocked.mock.calls.at(-1)).toEqual(["dupes.cleanup_purge", { run_id: "r-1", seqs: null, verify: "auto", ack_streams: false }]);
    expect(getCleanup().task?.state).toBe("waiting"); // the backend waits 5 s: 「取消」 calls it off
  });
});

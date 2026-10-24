// The duplicate-file cleanup plan (docs/dupes.md §16): a tab in cleanup mode shows one plan. Its
// own small store (progress events re-render only what reads them). Nothing here changes a file by
// itself: edits only change the plan; running needs an approval, its token and a confirmation
// whose focus starts on 「取消」; undo and emptying the recovery area are their own confirmed steps.
// There is no shortcut for running, undoing or emptying.

import { create } from "zustand";

import type {
  CleanupBucket,
  CleanupChangedEvent,
  CleanupDiff,
  CleanupEdit,
  CleanupGroup,
  CleanupGroupDetail,
  CleanupGroupsParams,
  CleanupInterrupted,
  CleanupPairRow,
  CleanupProgressEvent,
  CleanupPurgePlan,
  CleanupRiskCopies,
  CleanupRunDetail,
  CleanupRunSummary,
  CleanupState,
  CleanupUndoPlan,
  CleanupVolumeMatrix,
} from "@/protocol/generated";
import { fmtInt, fmtSize, plural } from "@/lib/format";
import { t } from "@/lib/i18n";

import { call, client } from "./actions";
import { get as getApp, set as setApp, toast } from "./store";

export type CleanupView = "groups" | "pairs" | "volumes" | "review" | "run" | "history";
export type CleanupSort = NonNullable<CleanupGroupsParams["sort"]>;
export type CleanupActionName = "leave" | "trash" | "hardlink" | "symlink" | "clone";

export type CleanupDialog =
  | { kind: "preview"; edit: CleanupEdit; diff: CleanupDiff; label: string }
  | { kind: "execute" }
  | { kind: "drops"; task: CleanupProgressEvent }
  | { kind: "undo"; runId: string; plan: CleanupUndoPlan }
  | { kind: "purge"; runId: string; plan: CleanupPurgePlan; ackStreams: boolean }
  | { kind: "resume" }
  | null;

export interface CleanupStore {
  tabId: string | null;
  planId: string | null;
  jobId: string | null;
  state: CleanupState | null;
  view: CleanupView;
  filter: string;
  sort: CleanupSort;
  groups: CleanupGroup[];
  total: number;
  version: number;
  /** groups shown with every copy */
  expanded: number[];
  /** groups ticked for a bulk edit */
  selected: number[];
  /** the group in the inspector, and the keyboard cursor */
  focus: number | null;
  detail: CleanupGroupDetail | null;
  pairs: CleanupPairRow[];
  pairsTotal: number;
  volumes: CleanupVolumeMatrix | null;
  /** a risk kind opened on the review page, with its copies */
  riskOpen: CleanupRiskCopies | null;
  dialog: CleanupDialog;
  /** the cleanup task this session follows (preflight, run, undo, purge) */
  task: CleanupProgressEvent | null;
  runs: CleanupRunSummary[];
  runsTotal: number;
  run: CleanupRunDetail | null;
  runFilter: string;
  interrupted: CleanupInterrupted | null;
  /** a receipt of the last edit (「已改 228 份 · Ctrl+Z 撤回」) */
  receipt: string;
  busy: boolean;
  /** terms of the filter the backend could not read (left out), in words */
  problems: string[];
  /** narrow windows: the inspector shown as a sheet over the table */
  sheet: boolean;
  /** the list of keys (「?」) */
  keysHelp: boolean;
  /** the run whose dropped steps were already put to the user (asked once) */
  askedDrops: string;
}

const initial = (): CleanupStore => ({
  tabId: null,
  planId: null,
  jobId: null,
  state: null,
  view: "groups",
  filter: "",
  sort: "freed",
  groups: [],
  total: 0,
  version: -1,
  expanded: [],
  selected: [],
  focus: null,
  detail: null,
  pairs: [],
  pairsTotal: 0,
  volumes: null,
  riskOpen: null,
  dialog: null,
  task: null,
  runs: [],
  runsTotal: 0,
  run: null,
  runFilter: "",
  interrupted: null,
  receipt: "",
  busy: false,
  problems: [],
  sheet: false,
  keysHelp: false,
  askedDrops: "",
});

export const useCleanup = create<CleanupStore>(initial);
export const getCleanup = useCleanup.getState;
export const setCleanup = useCleanup.setState;

/** Back to nothing (tests; a backend that restarted). */
export function resetCleanup(): void {
  useCleanup.setState(initial(), true);
}

export const PAGE = 200;
const RUNNING = new Set(["waiting", "preflight", "running", "paused"]);
export const taskRunning = (ev: CleanupProgressEvent | null | undefined): boolean => !!ev && RUNNING.has(ev.state);

/** The backend offers cleanup plans (capability dupes.cleanup.v1). */
export function cleanupSupported(): boolean {
  try {
    return client().hello?.capabilities.includes("dupes.cleanup.v1") ?? false;
  } catch {
    return false;
  }
}

export function readable(err: unknown): string {
  if (err && typeof err === "object" && "message" in err) return String((err as { message: unknown }).message);
  return String(err);
}

// ------------------------------------------------------------------ the words of the plan
export const actionLabel = (a: CleanupActionName | "keep" | string): string =>
  ({
    leave: t("不动"),
    trash: typeof window !== "undefined" && window.sk?.platform === "darwin" ? t("移到废纸篓") : t("移到回收站"),
    hardlink: t("换成硬链接"),
    symlink: t("换成符号链接"),
    clone: t("共用数据块（块克隆）"),
    keep: t("保留"),
  })[a] ?? a;

export const actionShort = (a: string): string =>
  ({ leave: t("不动"), trash: t("回收站"), hardlink: t("硬链接"), symlink: t("符号链接"), clone: t("块克隆"), keep: t("留") })[a] ?? a;

export const ACTIONS: CleanupActionName[] = ["leave", "trash", "hardlink", "symlink", "clone"];

export const sevLabel = (s: string): string => ({ info: t("提示"), low: t("低"), medium: t("中"), high: t("高"), blocked: t("不可用"), none: "" })[s] ?? s;
export const sevMark = (s: string): string => ({ info: "·", low: "○", medium: "■", high: "▲", blocked: "⊘", none: "" })[s] ?? "";

export const presetLabel = (p: string): string =>
  ({ watch: t("只看，不动"), conservative: t("保守"), recommended: t("推荐"), links: t("链接优先"), backup: t("备份盘整理"), custom: t("自定义") })[p] ?? p;

export const criterionLabel = (c: string): string =>
  ({
    oldest_created: t("最早创建"),
    oldest_modified: t("最早修改"),
    newest_modified: t("最新修改"),
    shortest_path: t("路径最短"),
    longest_path: t("路径最长"),
    shallowest: t("层数最少"),
    deepest: t("层数最多"),
    plain_name: t("名字不像副本"),
  })[c] ?? c;

/** A group's actions in a few characters: 「留1·链2」. */
export function actionsLine(acts: Record<string, number>): string {
  return ["keep", "clone", "hardlink", "symlink", "trash", "leave"]
    .filter((a) => acts[a])
    .map((a) => `${actionShort(a)}${acts[a]}`)
    .join(" · ");
}

// ------------------------------------------------------------------ entering and leaving
function patchTab(tabId: string, cleanup: { planId: string; jobId: string } | undefined): void {
  const a = getApp();
  setApp({ tabs: a.tabs.map((tab) => (tab.id === tabId ? { ...tab, cleanup } : tab)) });
}

/** 「清理方案…」: the tab goes into cleanup mode on the plan of this search (built when asked). */
export async function openCleanup(tabId: string, jobId: string, preset: string | null = null): Promise<void> {
  if (!cleanupSupported()) return toast(t("后端不支持清理方案（版本太旧）"));
  setCleanup({ busy: true });
  try {
    const st = await call("dupes.cleanup_open", { job_id: jobId, preset: (preset as never) ?? null, backup_volume: "" });
    setCleanup({ ...initial(), tabId, planId: st.plan_id, jobId, state: st, view: "groups", dialog: st.resume_offer ? { kind: "resume" } : null });
    patchTab(tabId, { planId: st.plan_id, jobId });
    await loadGroups();
  } catch (err) {
    toast(readable(err), 3000);
  } finally {
    setCleanup({ busy: false });
  }
}

/** 「重复文件清理记录」: the history, also without a plan (no search needed). */
export async function openHistory(tabId: string): Promise<void> {
  const c = getCleanup();
  if (c.tabId !== tabId) setCleanup({ ...initial(), tabId });
  setCleanup({ view: "history" });
  patchTab(tabId, { planId: getCleanup().planId ?? "", jobId: getCleanup().jobId ?? "" });
  await loadRuns();
}

/** Leave cleanup mode: the plan's edits are kept by the backend (not the acknowledgements). */
export async function closeCleanup(): Promise<void> {
  const c = getCleanup();
  if (c.planId) await call("dupes.cleanup_close", { plan_id: c.planId }).catch(() => undefined);
  if (c.tabId) patchTab(c.tabId, undefined);
  resetCleanup();
}

// ------------------------------------------------------------------ reading
export async function refreshState(): Promise<void> {
  const id = getCleanup().planId;
  if (!id) return;
  try {
    setCleanup({ state: await call("dupes.cleanup_state", { plan_id: id }) });
  } catch (err) {
    toast(readable(err));
  }
}

export async function loadGroups(more = false): Promise<void> {
  const c = getCleanup();
  if (!c.planId) return;
  const offset = more ? c.groups.length : 0;
  const limit = more ? PAGE : Math.max(PAGE, Math.min(500, c.groups.length));
  try {
    const page = await call("dupes.cleanup_groups", { plan_id: c.planId, filter: c.filter, sort: c.sort, offset, limit, include_out: false });
    const groups = more ? [...getCleanup().groups, ...page.items] : page.items;
    setCleanup({ groups, total: page.total, version: page.version, problems: page.problems ?? [] });
    const f = getCleanup().focus;
    const inList = f !== null && groups.some((g) => g.id === f);
    if (!inList && !more) await inspect(groups.length ? groups[0]!.id : null);
    else if (f !== null) await inspect(f);
  } catch (err) {
    toast(readable(err));
  }
}

export async function inspect(group: number | null): Promise<void> {
  const id = getCleanup().planId;
  setCleanup({ focus: group });
  if (!id || group === null) return setCleanup({ detail: null });
  try {
    const detail = await call("dupes.cleanup_group", { plan_id: id, group });
    if (getCleanup().focus === group) setCleanup({ detail });
  } catch {
    setCleanup({ detail: null });
  }
}

export function setFilter(filter: string): void {
  setCleanup({ filter, selected: [] });
  void loadGroups();
}

export function setSort(sort: CleanupSort): void {
  setCleanup({ sort });
  void loadGroups();
}

export function toggleExpand(group: number): void {
  const e = getCleanup().expanded;
  setCleanup({ expanded: e.includes(group) ? e.filter((g) => g !== group) : [...e, group] });
}

export function toggleSelect(group: number, extend = false): void {
  const c = getCleanup();
  if (extend && c.selected.length) {
    const ids = c.groups.map((g) => g.id);
    const from = ids.indexOf(c.selected[c.selected.length - 1]!);
    const to = ids.indexOf(group);
    if (from >= 0 && to >= 0) {
      const [a, b] = from < to ? [from, to] : [to, from];
      setCleanup({ selected: [...new Set([...c.selected, ...ids.slice(a, b + 1)])] });
      return;
    }
  }
  setCleanup({ selected: c.selected.includes(group) ? c.selected.filter((g) => g !== group) : [...c.selected, group] });
}

export async function setView(view: CleanupView): Promise<void> {
  setCleanup({ view, riskOpen: null });
  if (view === "pairs") await loadPairs();
  else if (view === "volumes") await loadVolumes();
  else if (view === "history") await loadRuns();
  else if (view === "review") await refreshState();
}

export async function loadPairs(): Promise<void> {
  const id = getCleanup().planId;
  if (!id) return;
  try {
    const page = await call("dupes.cleanup_pairs", { plan_id: id, offset: 0, limit: 200, merge: true });
    setCleanup({ pairs: page.items, pairsTotal: page.total });
  } catch (err) {
    toast(readable(err));
  }
}

export async function loadVolumes(): Promise<void> {
  const id = getCleanup().planId;
  if (!id) return;
  try {
    setCleanup({ volumes: await call("dupes.cleanup_volumes", { plan_id: id }) });
  } catch (err) {
    toast(readable(err));
  }
}

export async function openRisk(risk: string | null): Promise<void> {
  const id = getCleanup().planId;
  if (!id || risk === null) return setCleanup({ riskOpen: null });
  try {
    setCleanup({ riskOpen: await call("dupes.cleanup_risk_copies", { plan_id: id, risk, offset: 0, limit: 100 }) });
  } catch (err) {
    toast(readable(err));
  }
}

// ------------------------------------------------------------------ editing
/** The receipt of an edit: what it did, then how to take it back. */
export function receiptOf(what: string): string {
  return t("{what} · Ctrl+Z 撤回", { what: what || t("已改") });
}

/** What a previewed edit changed, in copies (and the copies a risk rule turned into 「不动」). */
export function diffLine(diff: CleanupDiff): string {
  const keys = new Set([...Object.keys(diff.before), ...Object.keys(diff.after)]);
  let moved = 0;
  for (const k of keys) moved += Math.max(0, (diff.after[k] ?? 0) - (diff.before[k] ?? 0));
  const held = diff.unavailable.risk_rule ?? 0;
  const line = plural(moved, t("已改 {n} 份（{g} 组）", { n: fmtInt(moved), g: fmtInt(diff.changed) }), "Changed 1 copy ({g} groups)", { g: fmtInt(diff.changed) });
  return held ? `${line}${t("，其中 {n} 份有高风险，按规则改成了不动", { n: fmtInt(held) })}` : line;
}

/** An edit, sent with the version it was made on (a newer plan refuses it: it is shown again). */
export async function edit(e: CleanupEdit, label = ""): Promise<boolean> {
  const c = getCleanup();
  if (!c.planId || !c.state) return false;
  try {
    const st = await call("dupes.cleanup_edit", { plan_id: c.planId, base_version: c.state.version, edit: e });
    setCleanup({ state: st, receipt: receiptOf(label), dialog: null });
    await loadGroups();
    if (getCleanup().view === "pairs") await loadPairs();
    return true;
  } catch (err) {
    toast(readable(err), 3500);
    await refreshState();
    return false;
  }
}

/** A bulk edit: previewed first, kept only from the preview dialog. */
export async function preview(e: CleanupEdit, label: string): Promise<void> {
  const c = getCleanup();
  if (!c.planId || !c.state) return;
  try {
    const diff = await call("dupes.cleanup_preview", { plan_id: c.planId, base_version: c.state.version, edit: e });
    if (diff.refused) return toast(diff.refused, 3500);
    setCleanup({ dialog: { kind: "preview", edit: e, diff, label } });
  } catch (err) {
    toast(readable(err), 3500);
  }
}

export async function commitPreview(includeOverridden = false): Promise<void> {
  const d = getCleanup().dialog;
  if (d?.kind !== "preview") return;
  await edit({ ...d.edit, include_overridden: includeOverridden }, d.diff.refused ? d.label : diffLine(d.diff));
}

export async function undoEdit(redo = false): Promise<void> {
  const c = getCleanup();
  if (!c.planId || !c.state) return;
  if (redo ? !c.state.can_redo : !c.state.can_undo) return;
  try {
    const st = await call(redo ? "dupes.cleanup_redo_edit" : "dupes.cleanup_undo_edit", { plan_id: c.planId, base_version: c.state.version });
    setCleanup({ state: st, receipt: redo ? t("已重做 · Ctrl+Z 撤回") : t("已撤回 · Ctrl+Shift+Z 重做") });
    await loadGroups();
  } catch (err) {
    toast(readable(err));
  }
}

/** The scope of a bulk edit: the ticked groups, or (none ticked) everything the filter shows. */
export function bulkScope(): { groups?: number[]; filter?: string } {
  const c = getCleanup();
  return c.selected.length ? { groups: c.selected } : { filter: c.filter };
}

export const keepOnly = (group: number, path: string) => edit({ op: "keep_only", scope: { copies: [{ group, path }] } }, t("这组只保留这一份"));
export const keepAdd = (group: number, path: string) => edit({ op: "keep_add", scope: { copies: [{ group, path }] } }, t("也保留这一份"));
export const keepRemove = (group: number, path: string) => edit({ op: "keep_remove", scope: { copies: [{ group, path }] } }, t("不再保留这一份"));
export const setAction = (group: number, path: string, action: CleanupActionName) => edit({ op: "action_set", action, scope: { copies: [{ group, path }] } }, t("这一份：{act}", { act: actionLabel(action) }));
export const setTarget = (group: number, path: string, target: string) => edit({ op: "target_set", target, scope: { copies: [{ group, path }] } }, t("改了链接指向的那份"));

/** One group's copies not kept to an action (a single key): the receipt says how many took it. */
export async function groupAction(group: number, action: CleanupActionName): Promise<void> {
  const ok = await edit({ op: "action_set", action, scope: { groups: [group] } }, t("这一组：{act}", { act: actionLabel(action) }));
  if (!ok || action === "leave") return;
  const g = getCleanup().groups.find((x) => x.id === group);
  if (g && !g.actions[action]) setCleanup({ receipt: receiptOf(t("这一组没有能{act}的份：做不到，或者有高风险（见检查器）", { act: actionLabel(action) })) });
}

export function bulkAction(action: CleanupActionName): Promise<void> {
  return preview({ op: "action_set", action, scope: bulkScope() }, t("{act}：已改", { act: actionLabel(action) }));
}

export function bulkExclude(): Promise<void> {
  return preview({ op: "exclude", scope: bulkScope() }, t("这些组不动"));
}

export function bulkClearManual(): Promise<void> {
  return preview({ op: "override_clear", scope: bulkScope() }, t("已清除手动选择"));
}

export async function markReviewed(groups: number[], value = true): Promise<void> {
  await edit({ op: "review", value, scope: { groups } }, value ? t("已标为过目") : t("已取消过目"));
}

export const ack = (risk: string) => edit({ op: "ack", risk }, t("已确认这一类风险"));
export const unack = (risk: string) => edit({ op: "unack", risk }, t("已取消确认"));
export const setPreset = (preset: string) => preview({ op: "preset", preset: preset as never }, t("预设：{p}", { p: presetLabel(preset) }));
export const setOptions = (options: NonNullable<CleanupEdit["options"]>) => edit({ op: "options", options }, t("已改选项"));

export async function resume(accept: boolean): Promise<void> {
  const id = getCleanup().planId;
  if (!id) return;
  try {
    setCleanup({ state: await call("dupes.cleanup_resume", { plan_id: id, accept }), dialog: null });
    await loadGroups();
  } catch (err) {
    toast(readable(err));
  }
}

// ------------------------------------------------------------------ approving and running
export async function approve(): Promise<string | null> {
  const c = getCleanup();
  if (!c.planId || !c.state) return null;
  try {
    const appr = await call("dupes.cleanup_approve", { plan_id: c.planId, version: c.state.version });
    await refreshState();
    return appr.token;
  } catch (err) {
    toast(readable(err), 4000);
    return null;
  }
}

async function token(): Promise<string | null> {
  const st = getCleanup().state;
  if (st?.approval && st.approval.version === st.version) return st.approval.token;
  return approve();
}

/** 「只预检」: every step checked again, nothing written. */
export async function preflightOnly(): Promise<void> {
  const c = getCleanup();
  const tok = await token();
  if (!tok || !c.planId) return;
  try {
    const task = await call("dupes.cleanup_preflight", { plan_id: c.planId, token: tok });
    setCleanup({ task: { task_id: task.task_id, kind: "preflight", phase: "preflight", state: "preflight", done: 0, total: task.total, bytes: 0, current: "", counts: {}, freed_now: 0, freed_later: 0, run_id: "", plan_id: c.planId, message: "", skipped: [], needs_confirm: false } });
  } catch (err) {
    toast(readable(err), 4000);
  }
}

/** 「执行…」 opens the last confirmation (its focus on 「取消」); only that one runs the plan. */
export function requestExecute(): void {
  setCleanup({ dialog: { kind: "execute" } });
}

/** Until the cleanup task this page follows has ended (a run held for a confirmation ends at once). */
async function settled(ms = 10_000): Promise<void> {
  const end = Date.now() + ms;
  while (taskRunning(getCleanup().task) && Date.now() < end) await new Promise((r) => setTimeout(r, 100));
}

export async function confirmExecute(proceedAfterDrops = false): Promise<void> {
  if (proceedAfterDrops) {
    setCleanup({ dialog: null });
    await settled();
    await refreshState();
  }
  const c = getCleanup();
  const tok = await token();
  if (!tok || !c.planId) return;
  setCleanup({ dialog: null });
  try {
    const task = await call("dupes.cleanup_execute", {
      plan_id: c.planId,
      token: tok,
      on_failure: null,
      verify: null,
      proceed_after_drops: proceedAfterDrops,
    });
    setCleanup({
      view: "run",
      task: { task_id: task.task_id, kind: "execute", phase: "preflight", state: "preflight", done: 0, total: task.total, bytes: 0, current: "", counts: {}, freed_now: 0, freed_later: 0, run_id: task.run_id, plan_id: c.planId, message: t("预检：核对 {n} 项没有变过…", { n: fmtInt(task.total) }), skipped: [], needs_confirm: false },
    });
  } catch (err) {
    toast(readable(err), 4000);
  }
}

export async function pauseTask(resume_ = false): Promise<void> {
  const task = getCleanup().task;
  if (!task) return;
  await call(resume_ ? "dupes.cleanup_resume_run" : "dupes.cleanup_pause", { task_id: task.task_id }).catch((err) => toast(readable(err)));
}

export async function stopTask(): Promise<void> {
  const task = getCleanup().task;
  if (!task) return;
  await call("dupes.cleanup_cancel", { task_id: task.task_id }).catch((err) => toast(readable(err)));
}

// ------------------------------------------------------------------ history: undo, empty the recovery area
export async function loadRuns(): Promise<void> {
  try {
    const page = await call("dupes.cleanup_runs", { offset: 0, limit: 50 });
    setCleanup({ runs: page.items, runsTotal: page.total });
    const cur = getCleanup().run;
    if (cur) await loadRun(cur.run.run_id);
  } catch (err) {
    toast(readable(err));
  }
}

export async function loadRun(runId: string, filter = getCleanup().runFilter): Promise<void> {
  try {
    setCleanup({ run: await call("dupes.cleanup_run", { run_id: runId, filter, offset: 0, limit: 500 }), runFilter: filter });
  } catch (err) {
    toast(readable(err));
  }
}

export async function requestUndo(runId: string): Promise<void> {
  try {
    const plan = await call("dupes.cleanup_undo_plan", { run_id: runId, seqs: null, beside: false });
    setCleanup({ dialog: { kind: "undo", runId, plan } });
  } catch (err) {
    toast(readable(err));
  }
}

export async function confirmUndo(beside: boolean): Promise<void> {
  const d = getCleanup().dialog;
  if (d?.kind !== "undo") return;
  setCleanup({ dialog: null });
  try {
    const task = await call("dupes.cleanup_undo", { run_id: d.runId, seqs: null, beside });
    setCleanup({ task: { task_id: task.task_id, kind: "undo", phase: "undo", state: "running", done: 0, total: d.plan.can, bytes: 0, current: "", counts: {}, freed_now: 0, freed_later: 0, run_id: d.runId, plan_id: "", message: "", skipped: [], needs_confirm: false } });
  } catch (err) {
    toast(readable(err), 4000);
  }
}

export async function requestPurge(runId: string): Promise<void> {
  try {
    const plan = await call("dupes.cleanup_purge_plan", { run_id: runId, seqs: null, verify: "auto", ack_streams: false });
    setCleanup({ dialog: { kind: "purge", runId, plan, ackStreams: false } });
  } catch (err) {
    toast(readable(err));
  }
}

/** Emptying waits 5 s on the backend first (「取消」 calls it off: nothing touched). */
export async function confirmPurge(): Promise<void> {
  const d = getCleanup().dialog;
  if (d?.kind !== "purge") return;
  setCleanup({ dialog: null });
  try {
    const task = await call("dupes.cleanup_purge", { run_id: d.runId, seqs: null, verify: "auto", ack_streams: d.ackStreams });
    setCleanup({ task: { task_id: task.task_id, kind: "purge", phase: "wait", state: "waiting", done: 0, total: d.plan.can, bytes: 0, current: "", counts: {}, freed_now: 0, freed_later: 0, run_id: d.runId, plan_id: "", message: t("{s} 秒后开始清除…", { s: Math.round(task.delay_s) }), skipped: [], needs_confirm: false } });
  } catch (err) {
    toast(readable(err), 4000);
  }
}

export async function resolveItem(runId: string, seq: number, choice: "put_back" | "keep_as_is"): Promise<void> {
  try {
    setCleanup({ run: await call("dupes.cleanup_resolve", { run_id: runId, seq, choice }) });
    await loadRuns();
  } catch (err) {
    toast(readable(err));
  }
}

// ------------------------------------------------------------------ export
export async function exportPlan(format: "json" | "csv", runId = ""): Promise<void> {
  const c = getCleanup();
  const path = await window.sk?.dialog.saveFile({
    title: runId ? t("导出清理报告") : t("导出清理方案"),
    filters: [{ name: format.toUpperCase(), extensions: [format] }],
    defaultName: runId ? `${runId}.${format}` : `cleanup-plan.${format}`,
  });
  if (!path) return;
  try {
    const r = await call("dupes.cleanup_export", { plan_id: runId ? "" : (c.planId ?? ""), run_id: runId, format, path });
    toast(t("已导出 {size}", { size: fmtSize(r.bytes) }));
  } catch (err) {
    toast(readable(err), 3500);
  }
}

// ------------------------------------------------------------------ events
let changedTimer: ReturnType<typeof setTimeout> | null = null;

export function onCleanupChanged(ev: CleanupChangedEvent): void {
  const c = getCleanup();
  if (ev.plan_id !== c.planId) return;
  if (ev.reason === "closed") return;
  if (c.state && ev.version === c.state.version && ev.reason === "edit") return;
  if (changedTimer) clearTimeout(changedTimer);
  changedTimer = setTimeout(() => {
    changedTimer = null;
    void refreshState().then(() => (ev.reason === "facts" && getCleanup().view !== "groups" ? undefined : loadGroups()));
  }, 250);
}

export function onCleanupProgress(ev: CleanupProgressEvent): void {
  const c = getCleanup();
  if (c.task && ev.task_id !== c.task.task_id) return;
  if (!c.task && ev.plan_id !== c.planId) return;
  setCleanup({ task: ev });
  if (ev.needs_confirm && ev.kind === "execute") {
    // asked once per run; the run itself ends right after (nothing done): no 「完成 0 项」 toast
    if (c.askedDrops !== ev.task_id) setCleanup({ dialog: { kind: "drops", task: ev }, askedDrops: ev.task_id });
    if (!taskRunning(ev)) void loadRuns();
    return;
  }
  if (!taskRunning(ev)) {
    if (ev.kind === "execute" || ev.kind === "undo" || ev.kind === "purge") void loadRuns();
    if (ev.kind === "execute" && !ev.needs_confirm) void refreshState().then(() => loadGroups());
    if (ev.kind === "execute" && ev.state !== "waiting") toast(runLine(ev), 4000);
  }
}

export function onCleanupInterrupted(ev: CleanupInterrupted): void {
  setCleanup({ interrupted: ev });
  toast(ev.message, 6000);
}

/** One line for a finished run: 「完成：块克隆 338 份…；跳过 8 份；0 份出错」. */
export function runLine(ev: CleanupProgressEvent): string {
  const done = (ev.counts.done ?? 0) + (ev.counts.done_partial ?? 0);
  const failed = Object.entries(ev.counts)
    .filter(([k]) => k.startsWith("failed"))
    .reduce((n, [, v]) => n + v, 0);
  const skipped = Object.entries(ev.counts)
    .filter(([k]) => k.startsWith("skipped") || k === "cancelled")
    .reduce((n, [, v]) => n + v, 0);
  return t("完成 {done} 项 · 跳过 {skipped} 项 · 出错 {failed} 项", { done: fmtInt(done), skipped: fmtInt(skipped), failed: fmtInt(failed) });
}

/** The high-risk kinds still to acknowledge (the gate of approving). */
export function unackedBuckets(state: CleanupState | null): CleanupBucket[] {
  return state?.summary.buckets.filter((b) => b.severity === "high" && b.unacked > 0) ?? [];
}

// ------------------------------------------------------------------ keyboard (the group table)
/** Single keys while the group table has the focus (docs/dupes.md §16.13.6). Running, undoing and
 * emptying have no key. Returns whether the key was used. */
export function cleanupKey(e: { key: string; shiftKey: boolean; ctrlKey: boolean; metaKey: boolean }): boolean {
  const c = getCleanup();
  const ids = c.groups.map((g) => g.id);
  const at = c.focus === null ? -1 : ids.indexOf(c.focus);
  const g = at >= 0 ? c.groups[at]! : null;
  const mod = e.ctrlKey || e.metaKey;
  if (mod && (e.key === "z" || e.key === "Z")) {
    void undoEdit(e.shiftKey);
    return true;
  }
  if (mod && (e.key === "a" || e.key === "A")) {
    setCleanup({ selected: ids, receipt: t("已勾选显示出来的 {n} 组", { n: fmtInt(ids.length) }) });
    return true;
  }
  if (mod) return false;
  if (e.key === "?") {
    setCleanup({ keysHelp: !c.keysHelp });
    return true;
  }
  if (e.key === "Escape" && c.keysHelp) {
    setCleanup({ keysHelp: false });
    return true;
  }
  switch (e.key) {
    case "ArrowDown":
    case "j":
      if (ids.length) void inspect(ids[Math.min(ids.length - 1, at + 1)]!);
      return true;
    case "ArrowUp":
    case "k":
      if (ids.length) void inspect(ids[Math.max(0, at - 1)]!);
      return true;
    case "ArrowRight":
      if (g && !c.expanded.includes(g.id)) toggleExpand(g.id);
      return true;
    case "ArrowLeft":
      if (g && c.expanded.includes(g.id)) toggleExpand(g.id);
      return true;
    case " ":
      if (g) toggleSelect(g.id);
      return true;
    case "x":
      if (g) void edit({ op: "exclude", scope: { groups: [g.id] } }, t("这一组不动"));
      return true;
    case "r":
      if (g) {
        void markReviewed([g.id]);
        const next = c.groups.slice(at + 1).find((x) => !x.reviewed);
        if (next) void inspect(next.id);
      }
      return true;
    case "t":
    case "h":
    case "s":
    case "c":
    case "l": {
      if (!g) return false;
      const action = ({ t: "trash", h: "hardlink", s: "symlink", c: "clone", l: "leave" } as const)[e.key];
      void groupAction(g.id, action);
      return true;
    }
  }
  if (/^[1-9]$/.test(e.key) && g) {
    const copies = g.members.filter((m) => !m.view_of);
    const m = copies[Number(e.key) - 1];
    if (m) void (e.shiftKey ? keepAdd(g.id, m.path) : keepOnly(g.id, m.path));
    return true;
  }
  return false;
}

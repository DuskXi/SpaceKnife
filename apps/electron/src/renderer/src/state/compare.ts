// Compare mode (W6): enter, change the two ends, swap them, leave. The backend computes a
// comparison as a "change tree" that views open like a scan (docs/manage.md §3); while a new pair
// of ends is computed the old result stays on screen, dimmed (UI-W6-41).

import type { CompareProgressEvent, SnapshotInfo, SnapshotsChangedEvent } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { t } from "@/lib/i18n";
import { isSynthetic } from "@/visual/frame";

import { call, openSnapshot, openView, setViewSettings, startScan, viewCall } from "./actions";
import { activeTab, get, patchCompare, set, type Tab, type TabCompare, toast } from "./store";

export const NOW = "now";

/** Events that came before compare.open answered (the computation can be that fast). */
const early = new Map<string, CompareProgressEvent>();
/** Each tab's latest request: an answer to an older one is dropped. */
const generation = new Map<string, number>();

const tabOf = (id: string): Tab | undefined => get().tabs.find((t) => t.id === id);

function quietly(p: Promise<unknown>): void {
  p.catch(() => undefined);
}

function dropView(viewId: string | null): void {
  if (!viewId) return;
  quietly(call("view.close", { view_id: viewId }));
  set((s) => {
    const views = { ...s.views };
    delete views[viewId];
    return { views };
  });
}

/** 「与快照对比…」 (UI-W6-01): the latest snapshot of this scan target against now; `base` and
 * `target` (a plugin's CompareOpen, a panel's sk.host.compare): those ends instead. */
export async function enterCompare(base?: string, target?: string): Promise<void> {
  const tab = activeTab();
  if (tab.kind !== "scan" || !tab.scanId) return;
  if (tab.compare) {
    const ends = { base: base ?? tab.compare.base, target: target ?? tab.compare.target };
    if (ends.base !== tab.compare.base || ends.target !== tab.compare.target) await setCompareEnds(tab.id, ends.base, ends.target);
    return;
  }
  let sources;
  try {
    sources = await call("compare.sources", { scan_id: tab.scanId });
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
    return;
  }
  const end = target ?? NOW;
  if (!sources.now && (end === NOW || base === NOW)) return toast(sources.now_note || t("现在还不能对比"));
  if (!sources.snapshots.length && !base) return toast(t("还没有这个位置的快照：先保存一次快照（Ctrl+S）"));
  // the newest snapshot that is not this very tree (a finished scan saves itself at once)
  const own = get().scans[tab.scanId]?.snapshot;
  const first = base ?? (sources.snapshots.find((s) => s.path !== own) ?? sources.snapshots[0]!).path;
  if (first === end) return toast(t("基准和对比目标不能是同一份数据"));
  const compare: TabCompare = {
    base: first,
    target: end,
    compareId: null,
    phase: "loading",
    progress: null,
    message: "",
    shownId: null,
    viewId: null,
    info: null,
    sources,
  };
  set((s) => ({ tabs: s.tabs.map((t) => (t.id === tab.id ? { ...t, compare } : t)), basketOpen: false }));
  await startCompare(tab.id, first, end);
}

const fold = (p: string) => (window.sk.platform === "win32" ? p.toLowerCase() : p).replace(/[\\/]+$/, "");
const rootsKey = (roots: readonly string[]) => roots.map(fold).sort().join("|");

/** The tab showing a finished scan of these folders (`live`: a scan, not an opened snapshot). */
function tabOfRoots(roots: readonly string[], live: boolean): Tab | undefined {
  const s = get();
  const want = rootsKey(roots);
  return s.tabs.find((t) => {
    const scan = t.kind === "scan" && t.scanId ? s.scans[t.scanId] : undefined;
    return scan && scan.state === "done" && (!live || scan.source === "scan") && rootsKey(scan.roots.map((r) => r.path)) === want;
  });
}

/** A plugin shows a comparison (a command's CompareOpen, a panel's sk.host.compare): two snapshot
 * files, or one and "now". The tab of `scanId`, else one showing the snapshots' folders, compares;
 * for "now" without one the folders are scanned first (then compared, as 「与现在对比」); two
 * snapshots without one: `target` opens in a new tab and is compared with `base` there. */
export async function openCompare(req: { base: string; target: string; scanId?: string | null }): Promise<void> {
  const { base, target } = req;
  if (base === target) return toast(t("基准和对比目标不能是同一份数据"));
  const s = get();
  const scanId = req.scanId;
  let tab = scanId && s.scans[scanId]?.state === "done" ? s.tabs.find((x) => x.kind === "scan" && x.scanId === scanId) : undefined;
  let known: SnapshotInfo | undefined;
  if (!tab) {
    let list: SnapshotInfo[] = [];
    try {
      list = (await call("snapshot.list", {})).snapshots;
    } catch {
      /* offline: the active tab tries */
    }
    const files = [base, target].filter((x) => x !== NOW).map(fold);
    known = list.find((x) => files.includes(fold(x.path)));
    tab = known ? tabOfRoots(known.roots, base === NOW || target === NOW) : undefined;
  }
  if (tab) {
    set({ active: tab.id });
    return enterCompare(base, target);
  }
  if (!known) {
    // a snapshot saved elsewhere: the tab in front compares (compare.open says when its folders differ)
    if (activeTab().kind === "scan") return enterCompare(base, target);
    return toast(t("找不到这个快照：{path}", { path: base === NOW ? target : base }));
  }
  if (base === NOW || target === NOW) {
    const started = await startScan({ roots: known.roots });
    if (started) {
      afterScan.set(started, { base, target });
      toast(t("扫描完成后会自动与这个快照对比"));
    }
    return;
  }
  // two snapshots and no tab of their folders: the target is opened, and compared with the base
  await openSnapshot(target);
  const opened = activeTab();
  if (opened.kind === "scan" && opened.scanId && fold(get().scans[opened.scanId]?.snapshot ?? "") === fold(target)) await enterCompare(base, NOW);
}

/** snapshots.changed: the comparisons' lists follow; panels that may read snapshots hear of it. */
export function onSnapshotsChanged(ev: SnapshotsChangedEvent): void {
  set((s) => ({ snapshotsChanged: { n: s.snapshotsChanged.n + 1, saved: ev.saved, removed: ev.removed } }));
  for (const tab of get().tabs) if (tab.compare) void refreshCompareSources(tab.id);
}

/** Compare other ends (base / target dropdowns, the timeline, swap). */
export async function setCompareEnds(tabId: string, base: string, target: string): Promise<void> {
  const tab = tabOf(tabId);
  if (!tab?.compare) return;
  if (base === target) return toast(t("基准和对比目标不能是同一份数据"));
  await startCompare(tabId, base, target);
}

export function swapCompare(tabId: string): Promise<void> {
  const c = tabOf(tabId)?.compare;
  return c ? setCompareEnds(tabId, c.target, c.base) : Promise.resolve();
}

async function startCompare(tabId: string, base: string, target: string): Promise<void> {
  const tab = tabOf(tabId);
  if (!tab?.compare || !tab.scanId) return;
  const pending = tab.compare.compareId;
  if (pending && pending !== tab.compare.shownId) quietly(call("compare.close", { compare_id: pending }));
  patchCompare(tabId, { base, target, compareId: null, phase: "loading", progress: null, message: "" });
  const gen = (generation.get(tabId) ?? 0) + 1;
  generation.set(tabId, gen);
  try {
    const task = await call("compare.open", { scan_id: tab.scanId, base, target });
    if (generation.get(tabId) !== gen || !tabOf(tabId)?.compare) {
      quietly(call("compare.close", { compare_id: task.compare_id })); // superseded meanwhile
      return;
    }
    patchCompare(tabId, { compareId: task.compare_id });
    const ev = early.get(task.compare_id);
    early.delete(task.compare_id);
    if (ev) onCompareProgress(ev);
  } catch (err) {
    if (generation.get(tabId) === gen) failed(tabId, err instanceof RpcError ? err.message : String(err));
  }
}

function failed(tabId: string, message: string): void {
  const c = tabOf(tabId)?.compare;
  toast(message || t("对比失败"));
  if (!c) return;
  if (!c.shownId) {
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, compare: undefined } : t)) }));
    return;
  }
  // keep what is on screen (UI-W6-42): the ends go back to the ones shown
  const info = c.info;
  patchCompare(tabId, {
    phase: "ready",
    message: "",
    compareId: c.shownId,
    base: info ? sideSpec(info.base) : c.base,
    target: info ? sideSpec(info.target) : c.target,
  });
}

const sideSpec = (side: { kind: string; path?: string | null }) => (side.kind === "now" ? NOW : side.path ?? NOW);

export function onCompareProgress(ev: CompareProgressEvent): void {
  const tab = get().tabs.find((t) => t.compare && (t.compare.compareId === ev.compare_id || t.compare.shownId === ev.compare_id));
  if (!tab?.compare) {
    early.set(ev.compare_id, ev);
    if (early.size > 16) early.delete(early.keys().next().value!);
    return;
  }
  const c = tab.compare;
  if (ev.phase === "ready" && ev.info) {
    if (c.shownId === ev.compare_id) return patchCompare(tab.id, { info: ev.info }); // live target recomputed
    if (c.compareId === ev.compare_id) void show(tab.id, ev);
    return;
  }
  if (c.compareId !== ev.compare_id) return; // a live recompute of the one shown: nothing to show
  if (ev.phase === "failed") return failed(tab.id, ev.message);
  if (ev.phase === "cancelled") return;
  patchCompare(tab.id, { phase: ev.phase, progress: ev.progress ?? null, message: ev.message });
}

async function show(tabId: string, ev: CompareProgressEvent): Promise<void> {
  // the new ends keep the changes shown and the visualization of the comparison on screen
  const shown = tabOf(tabId)?.compare?.viewId;
  const was = shown ? get().views[shown]?.state : undefined;
  let viewId: string;
  try {
    viewId = await openView(ev.compare_id, was ? { direction: was.direction, visual: was.visual, color_mode: was.color_mode } : undefined);
  } catch (err) {
    return failed(tabId, err instanceof RpcError ? err.message : String(err));
  }
  const c = tabOf(tabId)?.compare;
  if (!c || c.compareId !== ev.compare_id) {
    dropView(viewId);
    quietly(call("compare.close", { compare_id: ev.compare_id }));
    return;
  }
  const oldView = c.viewId;
  const oldId = c.shownId;
  patchCompare(tabId, { shownId: ev.compare_id, viewId, info: ev.info, phase: "ready", progress: 1, message: "" });
  dropView(oldView);
  if (oldId && oldId !== ev.compare_id) quietly(call("compare.close", { compare_id: oldId }));
}

/** 「退出对比」 / Esc (UI-W6-08): back to the scan; the selection stays when it still exists now. */
export async function exitCompare(tabId: string = activeTab().id): Promise<void> {
  const tab = tabOf(tabId);
  const c = tab?.compare;
  if (!tab || !c) return;
  let keep: string | null = null;
  const sel = c.viewId ? get().views[c.viewId]?.state.selected : null;
  if (c.shownId && sel != null && sel > 0 && !isSynthetic(sel)) {
    try {
      keep = (await call("tree.node", { scan_id: c.shownId, node: sel })).path;
    } catch {
      keep = null;
    }
  }
  set((s) => ({ tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, compare: undefined } : t)) }));
  releaseCompare(c);
  const viewId = tab.viewIds[tab.focusedPane] ?? tab.viewIds[0];
  if (keep && tab.scanId && viewId) {
    try {
      const found = await call("tree.find", { scan_id: tab.scanId, path: keep });
      if (found.node) await viewCall(viewId, "view.select", { key: found.node.id });
    } catch {
      /* gone meanwhile */
    }
  }
}

/** Close a tab's comparisons and their view (leaving compare mode, closing the tab). */
export function releaseCompare(c: TabCompare): void {
  dropView(c.viewId);
  for (const id of new Set([c.shownId, c.compareId])) if (id) quietly(call("compare.close", { compare_id: id }));
}

/** 「显示的变化」 (UI-W6-07); C cycles all → grow → shrink (W6 待定 16). */
export function cycleDirection(viewId: string): void {
  const st = get().views[viewId]?.state;
  if (!st?.compare) return;
  const order = ["all", "grow", "shrink"] as const;
  void setViewSettings(viewId, { direction: order[(order.indexOf(st.direction) + 1) % 3] });
}

/** Scans started by 「与现在对比」 (or a plugin's comparison with now): compared once they finish. */
const afterScan = new Map<string, { base: string; target: string }>();

/** 「与现在对比」 on the start page (UI-W6-01): the tab already showing that place compares at once;
 * otherwise the place is scanned in this tab and compared when the scan is done. */
export async function compareWithNow(snapshot: { path: string; roots: string[] }, tabId: string): Promise<void> {
  const tab = tabOfRoots(snapshot.roots, true);
  if (tab) {
    set({ active: tab.id });
    await enterCompare(snapshot.path);
    return;
  }
  const scanId = await startScan({ roots: snapshot.roots }, tabId);
  if (scanId) {
    afterScan.set(scanId, { base: snapshot.path, target: NOW });
    toast(t("扫描完成后会自动与这个快照对比"));
  }
}

export function compareWhenDone(scanId: string): void {
  const ends = afterScan.get(scanId);
  if (!ends) return;
  afterScan.delete(scanId);
  const tab = get().tabs.find((t) => t.scanId === scanId);
  if (!tab) return;
  set({ active: tab.id });
  void enterCompare(ends.base, ends.target);
}

/** A snapshot was saved: the timeline and the dropdowns list it (UI-W6-35). */
export async function refreshCompareSources(tabId: string): Promise<void> {
  const tab = tabOf(tabId);
  if (!tab?.compare || !tab.scanId) return;
  try {
    const sources = await call("compare.sources", { scan_id: tab.scanId });
    patchCompare(tabId, { sources });
  } catch {
    /* offline */
  }
}

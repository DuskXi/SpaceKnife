// Duplicate files (docs/dupes.md §10): the search's state per scan, the drawer, the group lit in
// every pane. Its own small store, so a progress event (four a second at most) re-renders only
// what reads it. The keep rules and 「跨盘比较」 are the settings of the same names (设置 › 重复文件),
// changed from the drawer too.

import { create } from "zustand";

import type { DupeGroup, DupesFolderInfo, DupesFolderRow, DupesGroupsParams, DupesPlan, DupesStatus } from "@/protocol/generated";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";

import { call, updateSettings, viewCall } from "./actions";
import { get as getApp, set as setApp, setting, toast } from "./store";

export type DupesTab = "groups" | "folders";
export type KeepRule = "oldest" | "newest" | "shortest";
export type DupesLoadLevel = "idle" | "quiet" | "balanced" | "max";
export type DupesSort = NonNullable<DupesGroupsParams["sort"]>;

export interface Located {
  seq: number;
  rows: { row: number; exact: boolean }[];
  /** more copies than the 2000 lit */
  truncated: boolean;
}

export interface DupesState {
  /** the drawer is open (it follows the tab's scan) */
  open: boolean;
  tab: DupesTab;
  /** latest status per scan id (a cross-volume search is filed under each of its scans) */
  byScan: Record<string, DupesStatus>;
  /** the plan shown before a search starts, per scan */
  plan: Record<string, DupesPlan>;
  /** the drawer's group list: the first pages in the backend's order, of `groupsJob` */
  groups: DupeGroup[];
  groupsTotal: number;
  groupsVersion: number;
  groupsJob: string | null;
  sort: DupesSort;
  crossOnly: boolean;
  /** also list the groups that are one file under several names, or clones (nothing to free) */
  includeShared: boolean;
  folders: DupesFolderRow[];
  foldersTotal: number;
  /** what of a folder exists elsewhere, per job: the folder a search was limited to (modes against /
   * inside), or one asked about after a whole-scan search */
  coverage: Record<string, { node: number; info: DupesFolderInfo }>;
  /** the group lit in every pane (hovered in the drawer or in the picture) */
  focus: { jobId: string; group: number } | null;
  /** rows of a view's frame that hold the lit group's copies, from dupes.locate */
  located: Record<string, Located>;
}

const initial = (): DupesState => ({
  open: false,
  tab: "groups",
  byScan: {},
  plan: {},
  groups: [],
  groupsTotal: 0,
  groupsVersion: -1,
  groupsJob: null,
  sort: "reclaim",
  crossOnly: false,
  includeShared: false,
  folders: [],
  foldersTotal: 0,
  coverage: {},
  focus: null,
  located: {},
});

export const useDupes = create<DupesState>(initial);

export const setDupes = useDupes.setState;
export const getDupes = useDupes.getState;
/** Back to nothing known (tests; a backend that restarted). */
export function resetDupes(): void {
  useDupes.setState(initial(), true);
  pickedUp.clear();
}

/** Groups a page; a refresh while results come in asks for as many as are shown, up to MAX_PAGE. */
export const PAGE = 100;
const MAX_PAGE = 500;
/** At most this many copies are lit (docs/dupes.md §10.3). */
export const LIT_MAX = 2000;
const RUNNING = new Set(["planning", "identifying", "probing", "verifying", "waiting_scan", "paused"]);

export const isRunning = (s: DupesStatus | null | undefined): boolean => !!s && RUNNING.has(s.state);

/** The status covering a scan (null: never searched). */
export function dupesOf(scanId: string | undefined | null): DupesStatus | null {
  return scanId ? (getDupes().byScan[scanId] ?? null) : null;
}

/** 「重复」 is offered as a colouring while a search covers the view's scan: the view state says so
 * (`has_dupes`), and so does this interface's own record of the search, which is there before the
 * next view state is. */
export function dupesOffered(st: { scan_id: string; has_dupes?: boolean } | null | undefined): boolean {
  return !!st && (!!st.has_dupes || !!getDupes().byScan[st.scan_id]);
}

/** The keep rules and the cross-volume switch, from the settings. */
export function keepOptions(): { rule: KeepRule; folderConsistent: boolean; perVolume: boolean; cross: boolean } {
  const a = getApp();
  const rule: string = setting(a, "dupes.keep_rule", "oldest");
  return {
    rule: rule === "newest" || rule === "shortest" ? rule : "oldest",
    folderConsistent: setting(a, "dupes.folder_consistent", true),
    perVolume: setting(a, "dupes.cross_keep_per_volume", true),
    cross: setting(a, "dupes.cross_volume", false),
  };
}

export const ruleLabel = (r: KeepRule): string => ({ oldest: t("最早创建"), newest: t("最新修改"), shortest: t("路径最短") })[r];

/** Why a scan cannot be searched now ("" when it can). */
export function cannotSearch(scanId: string | undefined | null): string {
  const scan = scanId ? getApp().scans[scanId] : undefined;
  if (!scan) return t("需要先完成扫描");
  if (scan.source !== "scan") return t("快照是过去的状态，不能核对内容");
  if (scan.state !== "done") return t("扫描完成后才能查找重复文件");
  return "";
}

/** One line for the status bar / palette: 「查找重复 43% · 已读 1.2 GB」 or 「重复：可腾出 12.3 GB」. */
export function dupesSummary(s: DupesStatus): string {
  if (s.state === "waiting_scan") return t("查找重复 · 等 {disk} 的扫描结束", { disk: s.waiting_for });
  if (s.state === "paused") return t("查找重复 · 已暂停");
  if (isRunning(s)) {
    const pct = Math.round(100 * (s.progress ?? 0));
    return t("查找重复 {pct}% · 已读 {read}", { pct: String(pct), read: fmtSize(s.bytes.read) });
  }
  return t("重复：可腾出 {size}", { size: fmtSize(s.reclaim) });
}

export function onDupesProgress(ev: DupesStatus): void {
  const d = getDupes();
  const byScan = { ...d.byScan };
  // a search that replaced another drops the old one from every scan it covered
  const replaced = new Set(ev.scan_ids.map((id) => d.byScan[id]?.job_id).filter((j): j is string => !!j && j !== ev.job_id));
  for (const [id, s] of Object.entries(byScan)) if (replaced.has(s.job_id)) delete byScan[id];
  for (const id of ev.scan_ids) byScan[id] = ev;
  setDupes({ byScan });
}

export function onDupesChanged(ev: { job_id: string; version: number; reason: string }): void {
  const d = getDupes();
  if (ev.reason === "cleared") {
    const byScan = Object.fromEntries(Object.entries(d.byScan).filter(([, s]) => s.job_id !== ev.job_id));
    const coverage = { ...d.coverage };
    delete coverage[ev.job_id];
    const mine = d.groupsJob === ev.job_id;
    setDupes({
      byScan,
      coverage,
      ...(mine ? { groups: [], groupsTotal: 0, groupsVersion: -1, groupsJob: null, folders: [], foldersTotal: 0 } : {}),
      ...(d.focus?.jobId === ev.job_id ? { focus: null, located: {} } : {}),
    });
    return;
  }
  const shown = currentStatus();
  if (!d.open || shown?.job_id !== ev.job_id) return;
  // results came in: the list keeps its length and its place (docs/dupes.md §10.5)
  void loadGroups(ev.job_id, "refresh");
  if (d.tab === "folders" && shown.scan_ids[0]) void loadFolders(ev.job_id, currentScanId() ?? shown.scan_ids[0]);
  const cover = d.coverage[ev.job_id];
  if (cover) void loadCoverage(shown, cover.node);
  else if (shown.node != null && (shown.mode === "against" || shown.mode === "inside")) void loadCoverage(shown);
}

function currentScanId(): string | null {
  const a = getApp();
  return a.tabs.find((x) => x.id === a.active)?.scanId ?? null;
}

/** The status of the active tab's scan. */
export function currentStatus(): DupesStatus | null {
  return dupesOf(currentScanId());
}

export function openDrawer(open = true): void {
  setDupes({ open });
  if (!open) return focusGroup(null, null);
  setApp({ basketOpen: false });
  const scanId = currentScanId();
  if (scanId && !currentStatus()) void loadPlan(scanId);
}

/** What a search would read (nothing is read to know it); it also picks up a search the backend
 * runs or ran for the scan, after the interface reconnected. */
export async function loadPlan(scanId: string): Promise<void> {
  if (cannotSearch(scanId)) return;
  const ids = keepOptions().cross ? crossIds(scanId) : [scanId];
  try {
    const plan = await call("dupes.plan", { scan_ids: ids, cross_volume: ids.length > 1, mode: "all", node: null, min_size: minSize(), include_system: false, include_network: false });
    setDupes({ plan: { ...getDupes().plan, [scanId]: plan } });
    if (plan.current && !dupesOf(scanId)) onDupesProgress(plan.current);
  } catch (err) {
    toast(String((err as Error).message ?? err));
  }
}

const pickedUp = new Set<string>();

/** A view says results cover its scan but this interface never heard of the search (it reconnected,
 * or the page was reloaded): ask once, through the plan. */
export function pickUp(scanId: string | undefined, hasDupes: boolean | undefined): void {
  if (!scanId || !hasDupes || dupesOf(scanId) || pickedUp.has(scanId)) return;
  pickedUp.add(scanId);
  void loadPlan(scanId);
}

/** The smallest file compared (the setting is in MB). */
export function minSize(): number {
  return Math.max(1, setting(getApp(), "dupes.min_size", 1)) * 1024 * 1024;
}

/** Every open, finished live scan, the given one first (cross-volume searches). */
export function crossIds(first: string): string[] {
  const a = getApp();
  const live = Object.values(a.scans)
    .filter((s) => s.state === "done" && s.source === "scan")
    .map((s) => s.scan_id);
  return [first, ...live.filter((id) => id !== first)].slice(0, 16);
}

export async function setCross(scanId: string, on: boolean): Promise<void> {
  await updateSettings({ "dupes.cross_volume": on });
  await loadPlan(scanId);
}

export async function startSearch(scanId: string, opts: { mode?: "all" | "inside" | "against" | "file"; node?: number | null } = {}): Promise<void> {
  const why = cannotSearch(scanId);
  if (why) return toast(why, 3000);
  // a file or a folder of its own is one scan's question; 「跨盘比较」 widens a whole search
  const mode = opts.mode ?? "all";
  const ids = keepOptions().cross && mode === "all" ? crossIds(scanId) : [scanId];
  try {
    const st = await call("dupes.start", {
      scope: { scan_ids: ids, cross_volume: ids.length > 1, mode, node: opts.node ?? null, min_size: minSize(), include_system: false, include_network: false },
      load: null,
      load_custom: null,
      budget_bytes: null,
      max_seconds: null,
      replace: false,
    });
    onDupesProgress(st);
    setDupes({ open: true, tab: "groups", groups: [], groupsTotal: 0, groupsVersion: -1, groupsJob: st.job_id });
    setApp({ basketOpen: false });
  } catch (err) {
    toast(String((err as Error).message ?? err), 3000);
  }
}

async function control(method: "dupes.pause" | "dupes.resume" | "dupes.cancel", jobId: string): Promise<void> {
  try {
    onDupesProgress(await call(method, { job_id: jobId }));
  } catch (err) {
    toast(String((err as Error).message ?? err));
  }
}

export const pauseSearch = (jobId: string, pause: boolean): Promise<void> => control(pause ? "dupes.pause" : "dupes.resume", jobId);
export const cancelSearch = (jobId: string): Promise<void> => control("dupes.cancel", jobId);

export async function clearSearch(jobId: string): Promise<void> {
  const scans = new Set(Object.entries(getDupes().byScan).filter(([, s]) => s.job_id === jobId).map(([id]) => id));
  try {
    await call("dupes.clear", { job_id: jobId });
    onDupesChanged({ job_id: jobId, version: 0, reason: "cleared" });
  } catch (err) {
    toast(String((err as Error).message ?? err));
    return;
  }
  // the backend put views in the 「重复」 colouring back to classes: their state follows
  for (const [viewId, v] of Object.entries(getApp().views)) if (scans.has(v.state.scan_id)) void viewCall(viewId, "view.render", {});
}

export async function setLoad(jobId: string, level: DupesLoadLevel): Promise<void> {
  try {
    onDupesProgress(await call("dupes.set_load", { job_id: jobId, level, custom: null }));
  } catch (err) {
    toast(String((err as Error).message ?? err));
  }
}

let groupsSeq = 0;

/**
 * The drawer's group list. "reset": the first page (another search, order or filter); "refresh":
 * results changed, so as many as are shown come again in the new order — groups verified meanwhile
 * slot in where they belong and nothing scrolls; "more": the next page.
 */
export async function loadGroups(jobId: string, how: "reset" | "refresh" | "more" = "reset"): Promise<void> {
  const d = getDupes();
  const same = d.groupsJob === jobId;
  const offset = how === "more" && same ? d.groups.length : 0;
  const limit = how === "refresh" && same ? Math.min(MAX_PAGE, Math.max(PAGE, d.groups.length)) : PAGE;
  const seq = ++groupsSeq;
  try {
    const page = await call("dupes.groups", {
      job_id: jobId,
      offset,
      limit,
      sort: d.sort,
      cross_only: d.crossOnly,
      include_shared: d.includeShared,
      scan_id: null,
      under: null,
    });
    if (seq !== groupsSeq) return; // a newer request answers
    const items = how === "more" && same ? [...getDupes().groups, ...page.items] : page.items;
    setDupes({ groups: items, groupsTotal: page.total, groupsVersion: page.version, groupsJob: jobId });
  } catch {
    // a search cleared meanwhile: nothing to show
  }
}

/** Another order or filter of the group list: the first page again. */
export function setListing(patch: Partial<Pick<DupesState, "sort" | "crossOnly" | "includeShared">>, jobId: string): void {
  setDupes(patch);
  void loadGroups(jobId, "reset");
}

export async function loadFolders(jobId: string, scanId: string): Promise<void> {
  try {
    const page = await call("dupes.folders", { job_id: jobId, scan_id: scanId, offset: 0, limit: PAGE });
    setDupes({ folders: page.items, foldersTotal: page.total });
  } catch {
    // ignored: the tab shows what it had
  }
}

/** The coverage bar (「这个文件夹在别处有没有？」, docs/dupes.md §8.4) of the folder a search was
 * limited to, or of `node` of the tab's scan: a whole-scan search already holds every answer. */
export async function loadCoverage(s: DupesStatus, node: number | null = null): Promise<void> {
  const at = node ?? s.node;
  const scanId = node != null ? (currentScanId() ?? s.scan_ids[0]) : s.scan_ids[0];
  if (at == null || !scanId) return;
  try {
    const info = await call("dupes.folder", { job_id: s.job_id, scan_id: scanId, node: at });
    setDupes({ coverage: { ...getDupes().coverage, [s.job_id]: { node: at, info } } });
  } catch {
    // the folder went away: no bar
  }
}

const labels = new Map<number, string>();
/** The name a folder was asked about by (the coverage bar's title). */
export const folderLabel = (node: number): string | undefined => labels.get(node);

/** 「这个文件夹在别处有没有？」: answered from a finished whole-scan search, else a search of its own. */
export async function askFolder(scanId: string, node: number, label = ""): Promise<void> {
  if (label) labels.set(node, label);
  const s = dupesOf(scanId);
  if (s && !isRunning(s) && s.mode === "all") {
    openDrawer();
    setDupes({ tab: "groups" });
    await loadCoverage(s, node);
    return;
  }
  await startSearch(scanId, { mode: "against", node });
}

async function keepCall(jobId: string, overrides: { group: number; path: string; role: "keep" | "extra" }[]): Promise<void> {
  const k = keepOptions();
  try {
    onDupesProgress(await call("dupes.keep", { job_id: jobId, rule: k.rule, prefer: [], folder_consistent: k.folderConsistent, per_volume: k.perVolume, overrides }));
    await loadGroups(jobId, "refresh");
  } catch (err) {
    toast(String((err as Error).message ?? err));
  }
}

/** A keep rule changed in the drawer: it is the setting too, and the search applies it at once. */
export async function applyKeep(jobId: string, patch: Partial<{ rule: KeepRule; folderConsistent: boolean; perVolume: boolean }>): Promise<void> {
  const values: Record<string, string | boolean> = {};
  if (patch.rule !== undefined) values["dupes.keep_rule"] = patch.rule;
  if (patch.folderConsistent !== undefined) values["dupes.folder_consistent"] = patch.folderConsistent;
  if (patch.perVolume !== undefined) values["dupes.cross_keep_per_volume"] = patch.perVolume;
  await updateSettings(values);
  await keepCall(jobId, []);
}

/** 「保留这一份」 on one member: a manual choice, kept with the search (and through a rerun). */
export async function keepThis(jobId: string, group: number, path: string): Promise<void> {
  await keepCall(jobId, [{ group, path, role: "keep" }]);
}

export async function toBasket(jobId: string, groups: number[] | null = null): Promise<void> {
  try {
    const r = await call("dupes.to_basket", { job_id: jobId, groups });
    if (r.members > 0) {
      const refused = r.refused.length ? t("（{n} 组没有加入：保留的那份已在删除篮里）", { n: String(r.refused.length) }) : "";
      toast(t("已把 {n} 份多余的副本加入删除篮 · 可腾出 {size}", { n: String(r.members), size: fmtSize(r.freed) }) + refused, 4000);
      setDupes({ open: false });
      setApp({ basketOpen: true });
    } else if (r.refused.length) toast(t("没有加入：本组保留的那份已在删除篮里"), 3000);
    else toast(t("没有多余的副本可加入"));
  } catch (err) {
    toast(String((err as Error).message ?? err), 3000);
  }
}

let locateTimer: ReturnType<typeof setTimeout> | null = null;

/** Light a group in every pane (null: none). Copies drawn as blocks light at once from the frame's
 * group column; the ones hidden in a collapsed folder or a block of small items come from
 * dupes.locate, asked 100 ms after the pointer settles. */
export function focusGroup(jobId: string | null, group: number | null, viewIds: string[] = []): void {
  if (jobId === null || group === null) {
    if (locateTimer) clearTimeout(locateTimer);
    locateTimer = null;
    if (getDupes().focus !== null) setDupes({ focus: null, located: {} });
    return;
  }
  const cur = getDupes().focus;
  if (cur && cur.jobId === jobId && cur.group === group) return;
  setDupes({ focus: { jobId, group }, located: {} });
  if (locateTimer) clearTimeout(locateTimer);
  locateTimer = setTimeout(() => {
    locateTimer = null;
    for (const viewId of viewIds) {
      void call("dupes.locate", { job_id: jobId, view_id: viewId, group, limit: LIT_MAX })
        .then((r) => {
          const f = getDupes().focus;
          if (f && f.jobId === jobId && f.group === group) setDupes({ located: { ...getDupes().located, [viewId]: { seq: r.seq, rows: r.rows, truncated: r.truncated } } });
        })
        .catch(() => undefined);
    }
  }, 100);
}

/** Show a copy in a view: its block selected, zoomed out to as needed (the result list's way). */
export async function revealCopy(viewId: string | undefined, node: number): Promise<void> {
  if (viewId) await viewCall(viewId, "view.reveal", { key: node });
}

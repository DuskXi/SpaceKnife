// Everything that talks to the backend: connection, scans, views, filter, tags, updates.

import type { BackendInfo, BackendState } from "../../../shared/api";
import type { FrameInfo, MethodName, Methods, OpProgressEvent, ScanStartParams, ScanStatus, SettingsState, UpdateStatus, ViewSettingsParams, ViewState, ViewUpdate } from "@/protocol/generated";
import { fmtInt, plural, setUnitBase } from "@/lib/format";
import { Rpc, RpcError } from "@/protocol/rpc";
// the built-in visualizations register before the first hello announces their layouts
import "@/visual/builtins";
import { decodeVisualFrame, isKnownLayout } from "@/visual/frame";
import { componentFor } from "@/visual/registry";

import {
  activeTab,
  addLocalLog,
  addLog,

  get,
  type LoadLevel,
  newTabId,
  patchView,
  resetLogSeq,
  set,
  setting,
  type SettingsPageId,
  type Tab,
  toast,
  writeText,
} from "./store";
import { compareWhenDone, onCompareProgress, releaseCompare } from "./compare";
import { FramePairer, frameKey } from "./frames";
import { downloading, downloadOf, hasUpdate, visibleSurface } from "./updates";
import { getLang, setLang, t } from "@/lib/i18n";

let rpc: Rpc | null = null;
let backendInfo: BackendInfo | null = null;

/** Layouts this renderer decodes and has a component for. */
const drawable = (layout: string) => isKnownLayout(layout) && componentFor(layout) !== null;
const frames = new FramePairer(drawable);

export function client(): Rpc {
  if (!rpc || !rpc.open) throw new RpcError(-32006, t("后端未连接"));
  return rpc;
}

/** A request; never throws synchronously (a missing connection becomes a rejected promise). */
export function call<M extends MethodName>(method: M, params: Methods[M]["params"]): Promise<Methods[M]["result"]> {
  try {
    return client().call(method, params);
  } catch (err) {
    return Promise.reject(err);
  }
}

/** A cancellable request (superseded input); offline it is an already-rejected job. */
export function cancellable<M extends MethodName>(method: M, params: Methods[M]["params"]): { promise: Promise<Methods[M]["result"] | null>; cancel: () => void } {
  try {
    return client().cancellable(method, params);
  } catch (err) {
    return { promise: Promise.reject(err), cancel: () => {} };
  }
}

// ------------------------------------------------------------------ connection

export async function boot(): Promise<void> {
  window.sk.onBackend((st) => void onBackend(st));
  await onBackend(await window.sk.backend());
  // started again by 「重启并更新」: say once that it worked (the details are in 「关于与更新」)
  if (window.sk.justUpdated) toast(t("已更新到 SpaceKnife {version}", { version: window.sk.version }), 5000);
}

let backendGone = false; // exited / failed: stop reconnecting until a new backend is ready

async function onBackend(st: BackendState): Promise<void> {
  backendGone = st.state === "exited" || st.state === "failed";
  if (st.state === "starting") set({ conn: "starting" });
  else if (st.state === "failed") set({ conn: "failed", connMessage: st.message });
  else if (st.state === "exited") set({ conn: "failed", connMessage: t("后端已退出（退出码 {code}）", { code: String(st.code) }) });
  else if (st.state === "ready" && (!backendInfo || backendInfo.url !== st.info.url)) {
    if (backendInfo) resetLogSeq(); // a new backend numbers its records from 1 again
    backendInfo = st.info;
    await connect();
  }
}

let connectedOnce = false;

async function connect(): Promise<void> {
  if (!backendInfo) return;
  const r = new Rpc();
  try {
    const hello = await r.connect(backendInfo.url, backendInfo.token, { name: "spaceknife-electron", version: window.sk.version });
    rpc = r;
    frames.clear();
    set({ conn: "ready", connMessage: "", serverVersion: hello.server.version });
  } catch (err) {
    if (backendGone) return; // keep the 「后端已退出」 message; a new 'ready' starts over
    set({ conn: "lost", connMessage: String(err) });
    // after a lost session, retrying must also reopen the views (reconnect), not just connect
    setTimeout(() => void (connectedOnce ? reconnect() : connect()), 1000);
    return;
  }
  connectedOnce = true;
  r.onBinary((f) => frames.accept(f));
  r.on("view.frame", (ev) => void applyFrame(ev.frame));
  r.on("scan.progress", (ev) => onScanStatus(ev.status));
  r.on("settings.changed", (ev) => applySettings(ev.state));
  r.on("basket.changed", (ev) => set({ basket: ev.state }));
  r.on("ops.progress", (ev) => onOpProgress(ev));
  r.on("compare.progress", (ev) => onCompareProgress(ev));
  r.on("update.changed", (ev) => applyUpdateStatus(ev.status));
  // the file class table changed (W7): the parser vocabulary and the colours follow
  r.on("classes.changed", () => void r.call("filter.vocab", {}).then((vocab) => set({ vocab })).catch(() => undefined));
  r.on("log.record", (ev) => addLog(ev));
  r.onClose(() => {
    if (rpc !== r) return;
    rpc = null;
    if (backendGone) return;
    set({ conn: "lost" });
    addLocalLog("WARNING", t("与后端的连接已断开，正在重连…"));
    setTimeout(() => void reconnect(), 800);
  });
  void r.call("$/subscribe", { topic: "log", params: { backlog: 500, min_level: "INFO" } }).catch(() => {});
  const [vocab, volumes, snapshots, scans, settings, basket, visuals, update] = await Promise.all([
    r.call("filter.vocab", {}),
    r.call("volume.list", {}),
    r.call("snapshot.list", {}),
    r.call("scan.list", {}),
    r.call("settings.get", {}),
    r.call("basket.list", {}).catch(() => null),
    r.call("visual.list", {}).catch(() => null), // a server before visuals.v1 has only the treemap
    r.call("update.status", {}).catch(() => null), // a server before updates.v1 checks for none
  ]);
  applySettings(settings);
  set({
    basket,
    vocab,
    visuals: visuals?.visuals ?? [],
    volumes: volumes.volumes,
    snapshots: snapshots.snapshots,
    scans: Object.fromEntries(scans.scans.map((s) => [s.scan_id, s])),
  });
  if (update) applyUpdateStatus(update);
  for (const s of scans.scans) if (s.state === "scanning" || s.state === "queued") void watchScan(s.scan_id);
}

/** After a reconnect the views of the old session are gone: reopen every scan tab, each view with
 * the visualization and display settings it had. */
async function reconnect(): Promise<void> {
  const before = get().views;
  await connect();
  if (!rpc) return;
  addLocalLog("INFO", t("已重新连接"));
  const tabs = get().tabs.filter((t) => t.kind === "scan" && t.scanId);
  // comparisons lived in the old backend: those tabs leave compare mode
  set((s) => ({ views: {}, tabs: s.tabs.map((t) => (t.compare ? { ...t, compare: undefined } : t)) }));
  for (const t of tabs) {
    if (!get().scans[t.scanId!]) continue;
    const ids: string[] = [];
    for (let i = 0; i < Math.max(1, t.viewIds.length); i++) {
      ids.push(await openLike(t.scanId!, t.viewIds[i] ? before[t.viewIds[i]!]?.state : undefined));
    }
    set((s) => ({ tabs: s.tabs.map((x) => (x.id === t.id ? { ...x, viewIds: ids } : x)) }));
  }
}

// ------------------------------------------------------------------ frames

/** Views already told that their visualization cannot be drawn here. */
const fellBack = new Set<string>();

async function applyFrame(info: FrameInfo): Promise<void> {
  const cur = get().views[info.view_id];
  if (!cur || (cur.info && info.seq <= cur.info.seq)) return;
  // a server before visuals.v1 sends no layout: its frames are all drawlist.v1
  if (info.layout === undefined) info = { ...info, layout: "drawlist.v1", visual: "treemap" };
  // a layout this renderer cannot draw: no waiting for its binary frame, the picture on screen
  // stays, and the view goes back to the treemap
  if (!drawable(info.layout)) return fallBack(info);
  const bin = await frames.waitFor(frameKey(`view:${info.view_id}`, info.seq));
  const now = get().views[info.view_id];
  if (!bin || !now || (now.info && info.seq <= now.info.seq)) return;
  const frame = decodeVisualFrame(bin, info);
  if (!frame) return fallBack(info);
  patchView(info.view_id, { info, frame, filter: info.filter ?? now.filter });
}

function fallBack(info: FrameInfo): void {
  if (info.visual === "treemap") return; // nothing to go back to
  if (!fellBack.has(info.view_id)) {
    fellBack.add(info.view_id);
    toast(t("这个界面画不了这种可视化，已换回树图"));
  }
  void setViewSettings(info.view_id, { visual: "treemap" });
}

async function applyUpdate(viewId: string, upd: ViewUpdate): Promise<void> {
  patchView(viewId, { state: upd.state, filter: upd.filter ?? null });
  if (upd.frame) await applyFrame(upd.frame);
}

// ------------------------------------------------------------------ views

export function defaultViewport(): { width: number; height: number; dpr: number } {
  const inspector = get().inspectorHidden ? 0 : get().inspectorWidth;
  return {
    width: Math.max(200, window.innerWidth - inspector),
    height: Math.max(200, window.innerHeight - 40 - 48 - 28),
    dpr: window.devicePixelRatio || 1,
  };
}

/** A new view of a scan (or of a comparison); `settings` start it like another view (split, reconnect). */
export async function openView(scanId: string, settings?: ViewSettingsParams): Promise<string> {
  const upd = await call("view.open", { scan_id: scanId, viewport: defaultViewport(), ...(settings ? { settings } : {}) });
  const viewId = upd.state.view_id;
  set((s) => ({
    views: {
      ...s.views,
      [viewId]: { state: upd.state, info: null, frame: null, filter: null, parse: null, filterText: "", hoverKey: null },
    },
  }));
  if (upd.frame) await applyFrame(upd.frame);
  return viewId;
}

type ViewMethod =
  | "view.zoom"
  | "view.back"
  | "view.forward"
  | "view.up"
  | "view.home"
  | "view.select"
  | "view.reveal"
  | "view.set"
  | "view.tag"
  | "view.render";

export async function viewCall<M extends ViewMethod>(
  viewId: string | null,
  method: M,
  params: Omit<Methods[M]["params"], "view_id"> = {} as Omit<Methods[M]["params"], "view_id">,
): Promise<ViewUpdate | null> {
  if (!viewId || !get().views[viewId]) return null;
  try {
    const upd = (await call(method, { view_id: viewId, ...params } as Methods[M]["params"])) as ViewUpdate;
    await applyUpdate(viewId, upd);
    return upd;
  } catch (err) {
    if (err instanceof RpcError) toast(err.message);
    return null;
  }
}

export const setViewSettings = (viewId: string | null, settings: ViewSettingsParams) =>
  viewCall(viewId, "view.set", { settings });

/** What a view passes on to one opened like it: its visualization and what it shows. */
export function carried(st: ViewState): ViewSettingsParams {
  return {
    visual: st.visual,
    colors: st.colors,
    color_mode: st.color_mode,
    detail: st.detail,
    show_free: st.show_free,
    show_unknown: st.show_unknown,
    show_trash: st.show_trash,
  };
}

/** A new view started like another one (split, reconnect); a plain one when the server refuses that. */
async function openLike(scanId: string, like: ViewState | undefined): Promise<string> {
  if (!like) return openView(scanId);
  try {
    return await openView(scanId, carried(like));
  } catch {
    return openView(scanId);
  }
}

// ------------------------------------------------------------------ delete basket (W5)

/** Delete key / 「加入删除篮」 (UI-W3-33, UI-MAIN-46, UI-W5-18/28): the backend dedups by path. */
export function basketAdd(scanId: string, node: number): Promise<void> {
  return basketPut({ scan_id: scanId, nodes: [node] });
}

/** An aggregate block of a view goes in as one item with its members (UI-W5-29). */
export function basketAddBlock(scanId: string, viewId: string, key: number): Promise<void> {
  return basketPut({ scan_id: scanId, view_id: viewId, key });
}

async function basketPut(params: Methods["basket.add"]["params"]): Promise<void> {
  try {
    const r = await call("basket.add", params);
    set({ basket: r.state });
    if (r.refused.length) toast(t("不能删除：{reason}", { reason: r.refused[0]!.reason }));
    else if (r.added === 0) toast(t("已经在删除篮里"));
    else if (r.absorbed > 0)
      toast(plural(r.absorbed, t("已加入删除篮（并入了篮里的 {n} 项）", { n: fmtInt(r.absorbed) }), "Added to the delete basket (1 item already in it was merged)"));
    else toast(t("已加入删除篮"));
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
  }
}

export async function basketRemove(ids: string[]): Promise<void> {
  try {
    set({ basket: await call("basket.remove", { ids }) });
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
  }
}

export async function basketClear(): Promise<void> {
  set({ basket: await call("basket.clear", {}) });
}

/** Run the basket: into the recycle bin, or deleted for good after the undo window (W5-41…47). */
export async function basketExecute(mode: "trash" | "delete"): Promise<void> {
  try {
    const task = await call("basket.execute", { mode, ids: null });
    // an ops.progress event may already be here: it wins
    set((s) =>
      s.op?.task_id === task.task_id
        ? s
        : { op: { task_id: task.task_id, mode, done: 0, total: task.total, current: "", phase: task.delay_s > 0 ? "waiting" : "running", results: [], freed: 0, can_undo: task.delay_s > 0 } },
    );
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
  }
}

export async function opUndo(taskId: string): Promise<void> {
  try {
    await call("ops.undo", { task_id: taskId });
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
  }
}

export const opCancel = (taskId: string) => call("ops.cancel", { task_id: taskId }).catch(() => undefined);

function onOpProgress(ev: OpProgressEvent): void {
  set({ op: ev });
  if (ev.phase === "done") {
    const failed = ev.results.filter((r) => !r.ok);
    if (failed.length) {
      const fields = { n: fmtInt(failed.length), reason: failed[0]!.message };
      addLocalLog("WARNING", plural(failed.length, t("{n} 项没有删除：{reason}", fields), "1 item was not deleted: {reason}", fields));
    }
  }
}

// filter: colouring comes back at once (filter.parse); evaluation is debounced and cancellable
const filterTimers = new Map<string, ReturnType<typeof setTimeout>>();
const filterCalls = new Map<string, () => void>();

/** Leave filtering: the filter box empties, the side panel goes back to the details (the
 * 「匹配结果」 header's 「清除过滤」, the floater's ×, Esc on the map with nothing selected). */
export function clearFilter(viewId: string): void {
  void editFilter(viewId, "", null, true);
}

export async function editFilter(viewId: string, text: string, cursor: number | null, immediate = false): Promise<void> {
  patchView(viewId, { filterText: text });
  try {
    const parse = await call("filter.parse", { text, cursor });
    if (get().views[viewId]?.filterText === text) patchView(viewId, { parse });
  } catch {
    /* not connected */
  }
  clearTimeout(filterTimers.get(viewId));
  const run = () => {
    filterCalls.get(viewId)?.();
    const job = cancellable("view.filter", { view_id: viewId, text, cursor });
    filterCalls.set(viewId, job.cancel);
    void job.promise.then((upd) => upd && applyUpdate(viewId, upd)).catch(() => {});
  };
  if (immediate) run();
  else filterTimers.set(viewId, setTimeout(run, 140));
}

// ------------------------------------------------------------------ scans and tabs

export function scanTitle(paths: string[]): string {
  const vols = get().volumes;
  const p = paths[0] ?? "";
  const vol = vols.find((v) => v.mount_point.replace(/[\\/]+$/, "").toLowerCase() === p.replace(/[\\/]+$/, "").toLowerCase());
  if (vol) return vol.label ? `${vol.mount_point.replace(/[\\/]+$/, "")} ${vol.label}` : vol.mount_point;
  const parts = p.split(/[\\/]+/).filter(Boolean);
  return (parts[parts.length - 1] ?? p) + (paths.length > 1 ? ` +${paths.length - 1}` : "");
}

export async function watchScan(scanId: string): Promise<void> {
  try {
    await call("$/subscribe", { topic: "scan", params: { scan_id: scanId } });
  } catch {
    /* ignore */
  }
}

export type ProtectedChoice = "settings" | "skip" | "allow" | "cancel";
let protectedAnswer: ((choice: ProtectedChoice) => void) | null = null;

/** Show the Full Disk Access dialog and wait for the answer (one at a time: a second ask cancels the first). */
function askProtectedPlaces(): Promise<ProtectedChoice> {
  return new Promise((resolve) => {
    protectedAnswer?.("cancel");
    protectedAnswer = resolve;
    set({ fdaPrompt: true });
  });
}

export function answerProtectedPlaces(choice: ProtectedChoice, remember: boolean): void {
  set({ fdaPrompt: false });
  if (remember && (choice === "skip" || choice === "allow")) void updateSettings({ "scan.protected_places": choice });
  const answer = protectedAnswer;
  protectedAnswer = null;
  answer?.(choice);
}

/** macOS before a scan: the places that would each ask for consent. undefined: the backend applies
 *  the scan.protected_places setting; true / false: skip them or not (the user chose now); null:
 *  no scan (System Settings opened, or the dialog closed). */
async function protectedPlaces(): Promise<boolean | null | undefined> {
  if (window.sk.platform !== "darwin" || setting(get(), "scan.protected_places", "ask") !== "ask") return undefined;
  const info = await call("system.info", {}).catch(() => null);
  if (info?.full_disk_access !== false) return undefined;
  const choice = await askProtectedPlaces();
  if (choice === "settings") {
    window.sk.shell.openFullDiskAccess();
    toast(t("在列表里打开 SpaceKnife 的开关，再按提示重新打开 SpaceKnife"));
    return null;
  }
  return choice === "cancel" ? null : choice === "skip";
}

/** Start tabs whose scan is being started: a second Enter or click while the first one opens its
 * view (which can take a while on a busy backend) must not start the same scan again. */
const startingTabs = new Set<string>();

export async function startScan(params: ScanStartParams, replaceTab?: string): Promise<string | null> {
  if (replaceTab) {
    if (startingTabs.has(replaceTab)) return null;
    startingTabs.add(replaceTab);
  }
  try {
    return await startScanNow(params, replaceTab);
  } finally {
    if (replaceTab) startingTabs.delete(replaceTab);
  }
}

async function startScanNow(params: ScanStartParams, replaceTab?: string): Promise<string | null> {
  if (params.skip_protected == null) {
    const skip = await protectedPlaces();
    if (skip === null) return null;
    if (skip !== undefined) params = { ...params, skip_protected: skip };
  }
  try {
    const status = await call("scan.start", params);
    // a new scan re-arms "open on warnings" after the user closed the panel (W8 待定 5)
    set((s) => ({ scans: { ...s.scans, [status.scan_id]: status }, logDismissed: false }));
    void watchScan(status.scan_id);
    await showScan(status.scan_id, scanTitle(params.roots), replaceTab);
    return status.scan_id;
  } catch (err) {
    toast(err instanceof RpcError ? t("无法开始扫描：{error}", { error: err.message }) : String(err));
    return null;
  }
}

/** W2 scan controls: pause / resume (UI-W2-19/20), load level and concurrency (UI-W2-28…45), MFT (UI-W2-24). */
type ScanControl = "scan.pause" | "scan.resume" | "scan.use_mft" | "scan.set_load" | "scan.continue" | "scan.rescan";

async function scanControl<M extends ScanControl>(method: M, params: Methods[M]["params"]): Promise<boolean> {
  try {
    const status = (await call(method, params)) as ScanStatus;
    set((s) => ({ scans: { ...s.scans, [status.scan_id]: status } }));
    return true;
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
    return false;
  }
}

export const pauseScan = (scanId: string) => scanControl("scan.pause", { scan_id: scanId });
export const resumeScan = (scanId: string) => scanControl("scan.resume", { scan_id: scanId });
export const useMft = (scanId: string) => scanControl("scan.use_mft", { scan_id: scanId });
/** A stopped hard scan reads the folders it had left (W2 待定 16). */
export const continueScan = (scanId: string) => scanControl("scan.continue", { scan_id: scanId });
/** Read a folder of the tree again with everything below it (F5, M5); node 0, the top of a scan of
 * several roots, reads every root again. */
export async function rescanFolder(scanId: string, node: number): Promise<void> {
  if (await scanControl("scan.rescan", { scan_id: scanId, node })) toast(node === 0 ? t("正在重新扫描所有位置…") : t("正在重新扫描这个文件夹…"));
}
export const setScanLoad = (scanId: string, level: LoadLevel, custom?: number | null, concurrency?: number | null) =>
  scanControl("scan.set_load", { scan_id: scanId, level, custom: custom ?? null, concurrency: concurrency ?? null });

export async function openSnapshot(path: string, replaceTab?: string): Promise<void> {
  try {
    const status = await call("snapshot.load", { path });
    set((s) => ({ scans: { ...s.scans, [status.scan_id]: status } }));
    await showScan(status.scan_id, scanTitle(status.roots.map((r) => r.path)), replaceTab);
  } catch (err) {
    toast(err instanceof RpcError ? t("无法打开快照：{error}", { error: err.message }) : String(err));
  }
}

async function showScan(scanId: string, title: string, replaceTab?: string): Promise<void> {
  const viewId = await openView(scanId);
  const tab: Tab = { id: newTabId(), kind: "scan", scanId, viewIds: [viewId], focusedPane: 0, title };
  const old = replaceTab ? get().tabs.find((t) => t.id === replaceTab) : undefined;
  set((s) => {
    const tabs = replaceTab ? s.tabs.map((t) => (t.id === replaceTab ? tab : t)) : [...s.tabs, tab];
    return { tabs, active: tab.id };
  });
  if (old) releaseTab(old);
}

/** Close a tab's views and, when no other tab shows it any more, stop and free its scan (UI-W2-51, W8). */
function releaseTab(tab: Tab): void {
  if (tab.compare) releaseCompare(tab.compare);
  for (const v of tab.viewIds) void call("view.close", { view_id: v }).catch(() => {});
  set((s) => {
    const views = { ...s.views };
    for (const v of tab.viewIds) delete views[v];
    return { views };
  });
  const scanId = tab.scanId;
  if (scanId && !get().tabs.some((t) => t.scanId === scanId)) {
    void call("scan.close", { scan_id: scanId }).catch(() => {});
    set((s) => {
      const scans = { ...s.scans };
      delete scans[scanId];
      return { scans };
    });
  }
}

export function newStartTab(): void {
  const tab: Tab = { id: newTabId(), kind: "start", viewIds: [], focusedPane: 0, title: t("新扫描") };
  set((s) => ({ tabs: [...s.tabs, tab], active: tab.id }));
}

/** The settings tab is a singleton (UI-W7-01); 「完成」 returns to the tab it was opened from. */
export function openSettingsTab(page?: SettingsPageId): void {
  const s = get();
  const from = s.tabs.find((t) => t.id === s.active && t.kind !== "settings")?.id ?? s.settingsReturnTo;
  const existing = s.tabs.find((t) => t.kind === "settings");
  if (existing) set({ active: existing.id, settingsReturnTo: from });
  else {
    const tab: Tab = { id: newTabId(), kind: "settings", viewIds: [], focusedPane: 0, title: t("设置") };
    set((x) => ({ tabs: [...x.tabs, tab], active: tab.id, settingsReturnTo: from }));
  }
  if (typeof page === "string") settingsGo(page); // callers may be event handlers: ignore event objects
}

/** 「本机磁盘」: one tab at most (the start page's button, the palette). */
export function openMachineTab(): void {
  const existing = get().tabs.find((x) => x.kind === "machine");
  if (existing) return set({ active: existing.id });
  const tab: Tab = { id: newTabId(), kind: "machine", viewIds: [], focusedPane: 0, title: t("本机磁盘") };
  set((x) => ({ tabs: [...x.tabs, tab], active: tab.id }));
}

/** Show a settings page; recorded in the tab's page history (UI-W7-02, UI-W7-03). */
export function settingsGo(page: SettingsPageId): void {
  const s = get();
  if (s.settingsPage === page && !s.settingsQuery) return;
  set({ settingsPage: page, settingsBack: [...s.settingsBack, s.settingsPage], settingsForward: [], settingsQuery: "" });
  writeText("sk.settingsPage", page);
}

export function settingsHistory(dir: -1 | 1): void {
  const s = get();
  const from = dir < 0 ? s.settingsBack : s.settingsForward;
  const page = from[from.length - 1];
  if (!page) return;
  if (dir < 0) set({ settingsPage: page, settingsBack: from.slice(0, -1), settingsForward: [...s.settingsForward, s.settingsPage] });
  else set({ settingsPage: page, settingsForward: from.slice(0, -1), settingsBack: [...s.settingsBack, s.settingsPage] });
  writeText("sk.settingsPage", page);
}

/** 「完成」: close the settings tab and go back to where it was opened from (UI-W7-24). */
export async function closeSettings(): Promise<void> {
  const s = get();
  const tab = s.tabs.find((t) => t.kind === "settings");
  const back = s.settingsReturnTo && s.tabs.some((t) => t.id === s.settingsReturnTo) ? s.settingsReturnTo : null;
  if (tab) await closeTab(tab.id);
  if (back) set({ active: back });
}

// ------------------------------------------------------------------ settings (W7)

let accentApplied = "";

/** The title of a tab named by a word of the interface, not by a place (start, settings, help). */
function fixedTitle(tab: Tab): string | null {
  if (tab.kind === "start") return t("新扫描");
  if (tab.kind === "settings") return t("设置");
  if (tab.kind === "help") return t("帮助");
  if (tab.kind === "machine") return t("本机磁盘");
  return null;
}

/** Take a settings document from the backend and apply what the frontend itself renders. */
export function applySettings(state: SettingsState): void {
  const prev = get().settings;
  const base = state.unit_base === 1000 ? 1000 : 1024;
  setUnitBase(base);
  const accent = String(state.values["appearance.accent"] ?? "");
  if (accent && accent !== accentApplied) {
    document.documentElement.style.setProperty("--sk-color-accent-default", accent);
    accentApplied = accent;
  }
  const motion = state.values["appearance.reduce_motion"];
  document.documentElement.dataset.motion = motion === "on" ? "reduce" : motion === "off" ? "full" : "";
  // 「界面语言」 (M7.5): every t() follows at the next render; texts the backend made (volume notes,
  // class names) are fetched again, painted labels redrawn
  const langChanged = state.language !== getLang() || state.language !== get().lang;
  // always: index.html starts as zh-CN, and English at the first launch is no "change"
  document.documentElement.lang = state.language;
  if (langChanged) {
    setLang(state.language);
    window.sk.setLanguage(state.language);
  }
  set((s) => ({
    settings: state,
    logAuto: Boolean(state.values["general.log_auto_open"] ?? true),
    unitEpoch: prev?.unit_base !== state.unit_base || langChanged ? s.unitEpoch + 1 : s.unitEpoch,
    lang: state.language,
    // the start, settings and help tabs carry a made-up title: it follows the language
    tabs: langChanged ? s.tabs.map((tab) => (fixedTitle(tab) == null ? tab : { ...tab, title: fixedTitle(tab)! })) : s.tabs,
  }));
  if (langChanged && prev) {
    void refreshLists();
    void call("filter.vocab", {})
      .then((vocab) => set({ vocab }))
      .catch(() => undefined);
    // the visualizations' names are in the session's language
    void call("visual.list", {})
      .then((r) => set({ visuals: r.visuals }))
      .catch(() => undefined);
  }
}

/** Change settings; saved by the backend at once (UI-W7-22). Retries once after a version conflict. */
export async function updateSettings(values: Record<string, boolean | number | string>): Promise<boolean> {
  set({ settingsSave: { state: "saving", message: "" } });
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const state = await call("settings.update", { values, version: get().settings?.version ?? null });
      applySettings(state);
      set({ settingsSave: { state: "saved", message: "" } });
      return true;
    } catch (err) {
      if (err instanceof RpcError && err.code === -32013 && attempt === 0) {
        applySettings(await call("settings.get", {}));
        continue;
      }
      set({ settingsSave: { state: "error", message: err instanceof RpcError ? err.message : String(err) } });
      return false;
    }
  }
  return false;
}

export async function resetSettings(category: SettingsPageId | null): Promise<void> {
  try {
    applySettings(await call("settings.reset", { category }));
    set({ settingsSave: { state: "saved", message: "" } });
  } catch (err) {
    set({ settingsSave: { state: "error", message: String(err) } });
  }
}

/** Scan status updates; a scan that just finished in a background window asks for attention. */
function onScanStatus(status: ScanStatus): void {
  const before = get().scans[status.scan_id]?.state;
  set((s) => ({ scans: { ...s.scans, [status.scan_id]: status } }));
  const finished = status.state === "done" && (before === "scanning" || before === "queued");
  if (finished) compareWhenDone(status.scan_id); // 「与现在对比」 from the start page (UI-W6-01)
  if (finished && !document.hasFocus() && setting(get(), "general.flash_on_done", true)) window.sk.window.flash();
}

export async function closeTab(id: string): Promise<void> {
  const s = get();
  const idx = s.tabs.findIndex((t) => t.id === id);
  const tab = s.tabs[idx];
  if (!tab) return;
  let tabs = s.tabs.filter((t) => t.id !== id);
  if (!tabs.length) tabs = [{ id: newTabId(), kind: "start", viewIds: [], focusedPane: 0, title: t("新扫描") }];
  const active = s.active === id ? tabs[Math.min(idx, tabs.length - 1)]!.id : s.active;
  const startForms = { ...s.startForms };
  delete startForms[id];
  set({ tabs, active, startForms });
  releaseTab(tab); // closing a scanning tab stops that scan (UI-W2-51)
}

export function activate(id: string): void {
  set({ active: id });
}

/** W8: split the active scan tab into two panes (a second view of the same scan). */
export async function toggleSplit(): Promise<void> {
  const tab = activeTab();
  if (tab.kind !== "scan" || !tab.scanId) return;
  if (tab.viewIds.length > 1) {
    // leaving split keeps the focused pane (W8 待定 12)
    closePane(tab.id, tab.viewIds[tab.focusedPane === 0 ? 1 : 0]!);
    return;
  }
  // the new pane starts like the focused one
  const viewId = await openLike(tab.scanId, get().views[tab.viewIds[tab.focusedPane] ?? tab.viewIds[0]!]?.state);
  set((s) => ({ tabs: s.tabs.map((t) => (t.id === tab.id ? { ...t, viewIds: [...t.viewIds, viewId], focusedPane: 1 } : t)) }));
}

/** Close one pane of a split tab; the other one takes the whole area and the focus (UI-W8-19). */
export function closePane(tabId: string, viewId: string): void {
  const tab = get().tabs.find((t) => t.id === tabId);
  if (!tab || tab.viewIds.length < 2 || !tab.viewIds.includes(viewId)) return;
  void call("view.close", { view_id: viewId }).catch(() => {});
  set((s) => {
    const views = { ...s.views };
    delete views[viewId];
    return { views, tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, viewIds: t.viewIds.filter((v) => v !== viewId), focusedPane: 0 } : t)) };
  });
}

export function focusPane(tabId: string, pane: number): void {
  set((s) => ({ tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, focusedPane: pane } : t)) }));
}

/** Volumes only (the start page polls this so inserted / removed drives appear, UI-W1-21). */
export async function refreshVolumes(): Promise<void> {
  try {
    const volumes = await call("volume.list", {});
    set({ volumes: volumes.volumes, volumesElevated: volumes.elevated });
  } catch {
    /* offline */
  }
}

/** Read this computer's disks (machine.disks) for the start page's card and 「本机磁盘」: at most
 * once a minute unless `force` (the page's 「刷新」); a failure keeps what was read before. */
export async function refreshMachine(force = false): Promise<void> {
  const s = get();
  if (s.machineLoading || s.conn !== "ready" || (!force && s.machine && Date.now() - s.machineAt < 60_000)) return;
  set({ machineLoading: true });
  try {
    set({ machine: await call("machine.disks", {}), machineAt: Date.now() });
  } catch (err) {
    if (force) toast(t("读取磁盘失败：{error}", { error: err instanceof Error ? err.message : String(err) }));
  } finally {
    set({ machineLoading: false });
  }
}

/** Read the disks' own records that need administrator rights (machine.read_health; Linux:
 * polkit's password prompt). Only from the button on a disk card, never by itself; one prompt
 * reads every such disk. Why nothing was read shows on the disk whose button was clicked. */
export async function readDiskRecords(disk: string): Promise<void> {
  const s = get();
  if (s.machineReading || s.conn !== "ready") return;
  set({ machineReading: disk, machineReadNote: null });
  try {
    const r = await call("machine.read_health", { disks: [] });
    const failed = r.outcome === "denied" || r.outcome === "unavailable";
    set({ machine: r.machine, machineAt: Date.now(), machineReadNote: failed ? { disk, text: r.message } : null });
  } catch (err) {
    set({ machineReadNote: { disk, text: err instanceof Error ? err.message : String(err) } });
  } finally {
    set({ machineReading: null });
  }
}

export async function refreshLists(): Promise<void> {
  try {
    const [volumes, snapshots] = await Promise.all([call("volume.list", {}), call("snapshot.list", {})]);
    set({ volumes: volumes.volumes, volumesElevated: volumes.elevated, snapshots: snapshots.snapshots });
  } catch {
    /* offline */
  }
}

// ------------------------------------------------------------------ updates (updates.v1)

/** Versions whose card was closed here: a status the backend sent before it heard of it stays closed. */
const closedVersions = new Set<string>();

/** A status from the backend (an answer, or update.changed). A download that ends while no update
 * card is on screen says so in a toast; the badge shows the rest. */
function applyUpdateStatus(next: UpdateStatus): void {
  if (next.latest && !next.dismissed && closedVersions.has(next.latest.version)) next = { ...next, dismissed: true };
  const before = downloadOf(get().update);
  set((s) => ({ update: next, updateOpen: s.updateOpen && hasUpdate(next) }));
  const after = downloadOf(next);
  if (!downloading(before) || downloading(after) || visibleSurface(next, get().updateOpen) !== "none") return;
  if (after.state === "done") toast(t("SpaceKnife {version} 已下载", { version: after.version ?? "" }));
  else if (after.state === "failed") toast(t("更新下载失败：{error}", { error: after.error ?? "" }));
}

async function updateCall<M extends "update.dismiss" | "update.download" | "update.cancel">(method: M, params: Methods[M]["params"]): Promise<void> {
  try {
    applyUpdateStatus((await call(method, params)) as UpdateStatus);
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
  }
}

/** 「检查更新」 (app.check_updates): now, also when automatic checks are off; a newer release opens
 * its card under the badge. */
export async function checkUpdates(): Promise<void> {
  toast(t("正在检查更新…"));
  try {
    const st = await call("update.check", {});
    applyUpdateStatus(st);
    if (st.error) toast(t("检查更新失败：{error}", { error: st.error }));
    else if (hasUpdate(st)) {
      set({ updateOpen: true });
      toast(t("发现新版本 {version}", { version: st.latest.version }));
    } else toast(t("已是最新版本"));
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
  }
}

/** The badge and 「查看更新」 (app.show_update): the card under the badge; nothing known yet: check. */
export function showUpdate(): Promise<void> {
  if (!hasUpdate(get().update)) return checkUpdates();
  set({ updateOpen: true });
  return Promise.resolve();
}

/** Close the update card or dialog (×, Esc, 「以后再说」): neither opens by itself again for this
 * version; the badge stays. */
export function closeUpdate(): void {
  const st = get().update;
  set({ updateOpen: false });
  if (!hasUpdate(st) || st.dismissed) return;
  closedVersions.add(st.latest.version);
  set({ update: { ...st, dismissed: true } }); // closed at once, whatever the backend answers
  void updateCall("update.dismiss", { version: st.latest.version });
}

export function toggleUpdateCard(): void {
  if (get().updateOpen) closeUpdate();
  else set({ updateOpen: true });
}

/** 「一键下载」 / 「重试」: the file for this kind of install, into the backend's updates folder. */
export const downloadUpdate = (version: string) => updateCall("update.download", { version });
export const cancelUpdate = () => updateCall("update.cancel", {});

/** 「重启并更新」 / 「安装」: main puts the downloaded version in place (main/update.ts restartMode); the
 * app quits once that has started, else the reason is shown. */
export async function installUpdate(path: string): Promise<void> {
  const reason = await window.sk.update.install(path);
  if (reason) toast(reason);
}

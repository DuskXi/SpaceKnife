// Application state (zustand). Backend data arrives through state/actions.ts; components read here.

import { create } from "zustand";

import type {
  BasketState,
  CompareInfo,
  CompareProgressEvent,
  CompareSourcesResult,
  FilterInfo,
  FilterParse,
  FilterVocab,
  FrameInfo,
  LogRecordEvent,
  OpProgressEvent,
  PathCheckResult,
  ScanStatus,
  SettingsState,
  SnapshotInfo,
  ViewState,
  VolumeInfo,
} from "@/protocol/generated";
import type { DrawFrame } from "@/treemap/frame";
import { getLang, type Lang, t } from "@/lib/i18n";

export type TabKind = "start" | "scan" | "settings";

export interface Tab {
  id: string;
  kind: TabKind;
  scanId?: string;
  /** primary view; a second one exists while split (W8) */
  viewIds: string[];
  focusedPane: number;
  title: string;
  /** compare mode (W6): the tab shows a change tree instead of its scan */
  compare?: TabCompare;
}

/** A tab's comparison: the one shown (viewId) and, while switching ends, the one being computed. */
export interface TabCompare {
  /** snapshot file or "now" */
  base: string;
  target: string;
  /** the comparison being computed or shown */
  compareId: string | null;
  phase: CompareProgressEvent["phase"];
  progress: number | null;
  message: string;
  /** the comparison on screen and its view (kept while a new one computes, UI-W6-41) */
  shownId: string | null;
  viewId: string | null;
  info: CompareInfo | null;
  sources: CompareSourcesResult | null;
}

export interface ViewData {
  state: ViewState;
  info: FrameInfo | null;
  frame: DrawFrame | null;
  filter: FilterInfo | null;
  parse: FilterParse | null;
  filterText: string;
  /** previous frame, kept for the zoom animation */
  previous: DrawFrame | null;
  /** block highlighted from outside the treemap (W3 result list hover, UI-W3-33) */
  hoverKey: number | null;
}

export type ConnState = "starting" | "ready" | "lost" | "failed";

export const SETTINGS_PAGES = ["general", "appearance", "treemap", "scan", "classes", "tags", "keys", "advanced"] as const;
export type SettingsPageId = (typeof SETTINGS_PAGES)[number];

/** The page shown when settings open: the last one visited, else 「常规」 (UI-W7-01). */
function lastSettingsPage(): SettingsPageId {
  const v = readText("sk.settingsPage");
  return (SETTINGS_PAGES as readonly string[]).includes(v ?? "") ? (v as SettingsPageId) : "general";
}

/** One line of the log panel (W8): a backend event, or a local one (connection). */
export type LoadLevel = "max" | "balanced" | "quiet" | "idle" | "custom";
export type ScanMethod = "auto" | "mft" | "hard";

/** The start page of one 「新扫描」 tab (W1); kept per tab so switching tabs keeps it (UI-W1-02). */
export interface StartForm {
  /** chosen volume (mount point); null = none, or the path box decides */
  picked: string | null;
  path: string;
  method: ScanMethod;
  load: LoadLevel;
  ads: boolean;
  /** last backend check of the typed paths (UI-W1-25) */
  check: PathCheckResult | null;
}

export interface LogEntry {
  seq: number;
  time: string;
  level: "INFO" | "WARNING" | "ERROR";
  /** scan / perm / monitor / delete / snapshot / export / system */
  source: string;
  message: string;
  scanId: string;
  path: string;
}

export interface AppState {
  conn: ConnState;
  connMessage: string;
  serverVersion: string;
  vocab: FilterVocab | null;
  volumes: VolumeInfo[];
  /** an MFT scan needs no administrator prompt now (rights already, or the helper is running) */
  volumesElevated: boolean;
  snapshots: SnapshotInfo[];
  scans: Record<string, ScanStatus>;
  tabs: Tab[];
  active: string;
  views: Record<string, ViewData>;
  paletteOpen: boolean;
  inspectorHidden: boolean;
  logOpen: boolean;
  logTab: "log" | "stats";
  /** open the panel by itself when a warning arrives (UI-W8-30) */
  logAuto: boolean;
  /** the user closed the panel: no automatic opening until the next scan starts (W8 待定 5) */
  logDismissed: boolean;
  logs: LogEntry[];
  /** W7: the backend's settings document (null until connected) */
  settings: SettingsState | null;
  settingsPage: SettingsPageId;
  settingsBack: SettingsPageId[];
  settingsForward: SettingsPageId[];
  settingsQuery: string;
  settingsSave: { state: "saved" | "saving" | "error"; message: string };
  /** the tab to go back to when the settings tab is closed with 「完成」 */
  settingsReturnTo: string | null;
  /** bumped when the size unit base changes, so painted labels are redrawn */
  unitEpoch: number;
  startForms: Record<string, StartForm>;
  toast: string | null;
  /** the delete basket, held by the backend (W5); null until it was read */
  basket: BasketState | null;
  basketOpen: boolean;
  /** the unified context menu (W5-01…24) */
  menu: MenuState | null;
  /** the latest file operation (progress, results, undo window) */
  op: OpProgressEvent | null;
  /** 「导出报告…」 dialog */
  exportOpen: boolean;
  /** interface language (M7.5): the root re-renders on change, every t() follows */
  lang: Lang;
}

/** Where the context menu opens and for what (a treemap block or a result row). */
export interface MenuState {
  viewId: string;
  scanId: string;
  /** node id, or an aggregate block's key */
  key: number;
  node: number | null;
  x: number;
  y: number;
  /** the target block in client px: the menu keeps clear of it */
  avoid?: { x: number; y: number; w: number; h: number };
}

let nextTab = 1;
export const newTabId = () => `tab-${nextTab++}`;

// in the language guessed at start; applySettings retitles it when the backend's differs
const firstTab: Tab = { id: newTabId(), kind: "start", viewIds: [], focusedPane: 0, title: t("新扫描") };

export const useApp = create<AppState>(() => ({
  conn: "starting",
  connMessage: "",
  serverVersion: "",
  vocab: null,
  volumes: [],
  volumesElevated: false,
  snapshots: [],
  scans: {},
  tabs: [firstTab],
  active: firstTab.id,
  views: {},
  paletteOpen: false,
  inspectorHidden: false,
  logOpen: false,
  logTab: "log",
  logAuto: true,
  logDismissed: false,
  logs: [],
  settings: null,
  settingsPage: lastSettingsPage(),
  settingsBack: [],
  settingsForward: [],
  settingsQuery: "",
  settingsSave: { state: "saved", message: "" },
  settingsReturnTo: null,
  unitEpoch: 0,
  startForms: {},
  toast: null,
  basket: null,
  basketOpen: false,
  menu: null,
  op: null,
  exportOpen: false,
  lang: getLang(),
}));

export const set = useApp.setState;
export const get = useApp.getState;

export function activeTab(s: AppState = get()): Tab {
  return s.tabs.find((t) => t.id === s.active) ?? s.tabs[0]!;
}

/** The view the toolbar acts on: the focused pane of the active scan tab (G7). */
export function focusedViewId(s: AppState = get()): string | null {
  const tab = activeTab(s);
  if (tab.kind !== "scan") return null;
  if (tab.compare) return tab.compare.viewId; // compare mode: the change tree (W6)
  return tab.viewIds[tab.focusedPane] ?? tab.viewIds[0] ?? null;
}

export function patchView(viewId: string, patch: Partial<ViewData>): void {
  set((s) => {
    const cur = s.views[viewId];
    if (!cur) return s;
    return { views: { ...s.views, [viewId]: { ...cur, ...patch } } };
  });
}

const LOG_KEEP = 10000;
let lastLogSeq = 0;
let localSeq = 0;

/** Backend records reach the panel when they are user-facing events or warnings and worse. */
export function addLog(rec: LogRecordEvent): void {
  if (rec.seq <= lastLogSeq) return; // replayed after a reconnect
  lastLogSeq = rec.seq;
  const event = rec.extra.event;
  const level = rec.level === "WARNING" ? "WARNING" : rec.level === "ERROR" || rec.level === "CRITICAL" ? "ERROR" : "INFO";
  if (!event && level === "INFO") return;
  pushLog({ seq: rec.seq, time: rec.time, level, source: event || "system", message: rec.message, scanId: rec.extra.scan_id ?? "", path: rec.extra.path ?? "" });
}

/** A line made by the frontend itself (connection lost / restored). */
export function addLocalLog(level: LogEntry["level"], message: string): void {
  pushLog({ seq: -++localSeq, time: new Date().toISOString(), level, source: "system", message, scanId: "", path: "" });
}

/** A new backend started: its record numbers begin again. */
export function resetLogSeq(): void {
  lastLogSeq = 0;
}

function pushLog(e: LogEntry): void {
  set((s) => {
    const logs = s.logs.length >= LOG_KEEP ? s.logs.slice(-(LOG_KEEP - 1000)) : s.logs.slice();
    logs.push(e);
    const open = !s.logOpen && e.level !== "INFO" && s.logAuto && !s.logDismissed;
    return open ? { logs, logOpen: true, logTab: "log" } : { logs };
  });
}

/** Show / hide the log panel; hiding it by hand stops automatic opening until the next scan (W8 待定 5). */
export function toggleLog(open?: boolean): void {
  const s = get();
  const next = open ?? !s.logOpen;
  set({ logOpen: next, logDismissed: !next });
}

export function readFlag(key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v == null ? fallback : v === "1";
  } catch {
    return fallback;
  }
}

export function readText(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeText(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* storage unavailable */
  }
}

/** A setting's current value (the default until the backend answered). */
export function setting<T extends boolean | number | string>(s: AppState, key: string, fallback: T): T {
  const v = s.settings?.values[key];
  return (v === undefined ? fallback : v) as T;
}

export function writeFlag(key: string, value: boolean): void {
  try {
    localStorage.setItem(key, value ? "1" : "0");
  } catch {
    /* storage unavailable */
  }
}

let toastTimer: ReturnType<typeof setTimeout> | null = null;
export function toast(message: string): void {
  set({ toast: message });
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => set({ toast: null }), 1800);
}

/** Items in the delete basket (toolbar badge, palette). */
export const basketCount = (s: AppState) => s.basket?.items.length ?? 0;

/** The start form of a tab, created from the settings' defaults and the system volume (UI-W1-03, UI-W1-19). */
const defaultForms = new Map<string, { sig: string; form: StartForm }>();

export function startFormOf(s: AppState, tabId: string): StartForm {
  const hit = s.startForms[tabId];
  if (hit) return hit;
  const system = s.volumes.find((v) => v.is_system && !v.reason) ?? s.volumes.find((v) => !v.reason);
  // the same object while nothing it depends on changed: selectors must return stable values
  const sig = `${s.settings?.version ?? -1}|${system?.mount_point ?? ""}`;
  const cached = defaultForms.get(tabId);
  if (cached && cached.sig === sig) return cached.form;
  const form: StartForm = {
    picked: system?.mount_point ?? null,
    path: "",
    method: setting<ScanMethod>(s, "scan.method", "auto"),
    load: setting<LoadLevel>(s, "scan.load_manual", "balanced"),
    ads: setting(s, "scan.ads", false),
    check: null,
  };
  defaultForms.set(tabId, { sig, form });
  return form;
}

export function patchStartForm(tabId: string, patch: Partial<StartForm>): void {
  set((s) => ({ startForms: { ...s.startForms, [tabId]: { ...startFormOf(s, tabId), ...patch } } }));
}

export function patchCompare(tabId: string, patch: Partial<TabCompare>): void {
  set((s) => ({ tabs: s.tabs.map((t) => (t.id === tabId && t.compare ? { ...t, compare: { ...t.compare, ...patch } } : t)) }));
}

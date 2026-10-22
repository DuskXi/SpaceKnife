// The single key table (docs/plugins.md 「键位」): every command's keys as palette.commands gives
// them (the app's own, the plugins', and what key packs rebind; the backend settles conflicts), and
// the global key handler that runs them. A few keys are the window's own and are no commands: the
// palette (Ctrl+K), switching tabs, the help and settings tabs' search and history, and in a view
// Escape, Enter, Shift+Backspace and the context-menu key. Keys inside components (arrows in a view,
// a menu's keys) stay where they are.
//
// While typing in a text field only combinations with Ctrl / Alt and F-keys of the app-wide commands
// (and the plugins') reach the table, never Ctrl+C, Ctrl+V and the other editing keys; a focused
// button keeps Enter, Space, Delete and Escape; a focused drop-down keeps single letters.

import type { PaletteCommand } from "@/protocol/generated";
import { clearFilter, newStartTab, settingsHistory, viewCall } from "@/state/actions";
import { exitCompare } from "@/state/compare";
import { runShortcut } from "@/state/commands";
import { BUILTIN_ACTIONS } from "./actions";
import { helpHistory, openHelpTab } from "@/state/help";
import { activeTab, focusedViewId, get, set, setting, type TabKind } from "@/state/store";
import { visibleSurface } from "@/state/updates";
import { hosts } from "@/visual/hosts";
import { HIT_ROOT, type VisualFrame } from "@/visual/types";

/** What a key event carries that the table looks at. */
export interface KeyLike {
  key: string;
  code?: string;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

const NAMES: Record<string, string> = {
  arrowleft: "Left",
  left: "Left",
  arrowright: "Right",
  right: "Right",
  arrowup: "Up",
  up: "Up",
  arrowdown: "Down",
  down: "Down",
  pageup: "PageUp",
  pgup: "PageUp",
  pagedown: "PageDown",
  pgdn: "PageDown",
  home: "Home",
  end: "End",
  enter: "Enter",
  return: "Enter",
  escape: "Escape",
  esc: "Escape",
  backspace: "Backspace",
  delete: "Delete",
  del: "Delete",
  insert: "Insert",
  ins: "Insert",
  tab: "Tab",
  " ": "Space",
  space: "Space",
  spacebar: "Space",
  contextmenu: "ContextMenu",
  apps: "ContextMenu",
};

const MODS: Record<string, "Ctrl" | "Alt" | "Shift" | "Meta"> = {
  ctrl: "Ctrl",
  control: "Ctrl",
  cmd: "Ctrl",
  command: "Ctrl",
  alt: "Alt",
  option: "Alt",
  shift: "Shift",
  meta: "Meta",
  win: "Meta",
  super: "Meta",
};

/** One key's canonical name: a character in upper case, F1–F24, or one of NAMES. */
function canon(base: string): string {
  const named = NAMES[base.toLowerCase()];
  if (named) return named;
  if (base.length === 1) return base.toUpperCase();
  if (/^f\d{1,2}$/i.test(base)) return base.toUpperCase();
  return base.slice(0, 1).toUpperCase() + base.slice(1);
}

const join = (mods: Set<string>, base: string) => [...(["Ctrl", "Alt", "Shift", "Meta"] as const).filter((m) => mods.has(m)), base].join("+");

/** A key as commands write it ("ctrl+alt+d", "Alt+Left") in the form chordOf gives ("Ctrl+Alt+D"). */
export function normalizeKey(text: string): string {
  const raw = text.replace(/\s+/g, "");
  const parts = raw.endsWith("++") ? [...raw.slice(0, -2).split("+").filter(Boolean), "+"] : raw.split("+");
  const mods = new Set<string>();
  let base = "";
  for (const p of parts) {
    if (!p) continue;
    const m = MODS[p.toLowerCase()];
    if (m) mods.add(m);
    else base = canon(p);
  }
  return join(mods, base);
}

const PRINTABLE_OK = /^[A-Z0-9]$/;

/** The chord of a key event, in the commands' notation; null for a modifier on its own. Ctrl is
 * ⌘ on macOS as well (the neutral notation). Shift counts for letters, digits and named keys; for
 * other characters it is part of the character ("?" is Shift+/ on one layout and not on another).
 * A letter or digit typed on a non-Latin layout, or with Option on macOS, is read from its key code. */
export function chordOf(e: KeyLike): string | null {
  let key = e.key;
  if (!key || ["Control", "Shift", "Alt", "Meta", "AltGraph", "Dead", "Process", "Unidentified", "CapsLock", "OS"].includes(key)) return null;
  const code = e.code ?? "";
  const fromCode = /^Key([A-Z])$/.exec(code)?.[1] ?? /^Digit([0-9])$/.exec(code)?.[1];
  if (fromCode && key.length === 1 && (e.altKey || !/^[\x20-\x7e]$/.test(key))) key = fromCode;
  const base = canon(key);
  const mods = new Set<string>();
  if (e.ctrlKey || e.metaKey) mods.add("Ctrl");
  if (e.altKey) mods.add("Alt");
  if (e.shiftKey && (base.length > 1 || PRINTABLE_OK.test(base))) mods.add("Shift");
  return join(mods, base);
}

export type KeyTable = Map<string, PaletteCommand[]>;

/** chord → the commands bound to it, in palette.commands order. */
export function buildTable(commands: readonly PaletteCommand[]): KeyTable {
  const table: KeyTable = new Map();
  for (const c of commands) {
    for (const k of c.keys) {
      const chord = normalizeKey(k);
      const list = table.get(chord) ?? [];
      if (!list.includes(c)) list.push(c);
      table.set(chord, list);
    }
  }
  return table;
}

let cache: { commands: readonly PaletteCommand[]; table: KeyTable } | null = null;

/** The table of the commands in the store (built again when they change). */
export function keyTable(commands: readonly PaletteCommand[] = get().commands): KeyTable {
  if (cache?.commands !== commands) cache = { commands, table: buildTable(commands) };
  return cache.table;
}

/** Where the key was pressed. */
export interface KeyEnv {
  tab: TabKind;
  /** the focused pane has a view (a scan tab) */
  view: boolean;
  compare: boolean;
  /** something is selected in that view */
  selected: boolean;
  /** the focus is in a text field */
  typing: boolean;
  /** a button, menu item, radio or similar has the focus */
  onControl: boolean;
  /** a drop-down has the focus (it picks options by letter) */
  typeAhead: boolean;
  /** 「树图有焦点时可省略 Ctrl」: bare 1–4 / 0 tag */
  bareTags: boolean;
  palette: boolean;
  /** a modal dialog that takes every key (a major release's update dialog) */
  modal: boolean;
}

/** The window's own keys (no commands). */
export type HostKey =
  | "palette"
  | "paletteHelp"
  | "nextTab"
  | "prevTab"
  | "helpSearch"
  | "helpBack"
  | "helpForward"
  | "settingsSearch"
  | "settingsBack"
  | "settingsForward"
  | "viewForward"
  | "menu"
  | "zoom"
  | "escape";

export type KeyDecision =
  | { host: HostKey }
  | {
      command: string;
      /** say nothing when it is unavailable here */
      quiet: boolean;
      /** a cycle the other way (Shift with a cycling key) */
      reverse: boolean;
      /** held down, it runs once */
      once: boolean;
    }
  | null;

/** Commands of the whole window: they run on every tab (and say why not where they can't). */
const APP_WIDE = /^(app|tab|scan)\./;
const APP_COMMANDS = new Set(["snapshot.save", "view.rescan", "basket.open"]);
/** Held down, these run once (F5, Shift+F5, Ctrl+S); plugins' commands too. */
const ONCE = new Set(["view.rescan", "scan.rescan_all", "snapshot.save"]);
/** With Ctrl these edit text: never taken from a text field. */
const EDITING = new Set(["A", "C", "V", "X", "Z", "Y", "Home", "End", "Left", "Right", "Up", "Down", "Backspace", "Delete", "Insert"]);

export const appWide = (c: Pick<PaletteCommand, "id" | "plugin">): boolean => c.plugin != null || APP_WIDE.test(c.id) || APP_COMMANDS.has(c.id);

/** A chord that may leave a text field: with Ctrl / Alt or an F-key, and not an editing key. */
function typingOk(chord: string): boolean {
  const parts = chord.split("+");
  const base = parts[parts.length - 1]!;
  const ctrl = parts.includes("Ctrl");
  const alt = parts.includes("Alt");
  if (/^F\d{1,2}$/.test(base)) return true;
  if (!ctrl && !alt) return false;
  if (ctrl && EDITING.has(base)) return false;
  if (alt && !ctrl && (base === "Left" || base === "Right" || base === "Backspace")) return false;
  return true;
}

const bare = (chord: string) => !chord.includes("+") || chord === "+";

function asCommand(c: PaletteCommand, quiet: boolean, reverse = false): KeyDecision {
  return { command: c.id, quiet, reverse, once: c.plugin != null || ONCE.has(c.id) };
}

/** Chords that are another one on most keyboards: Ctrl and the + of the number pad (or Shift+=) is
 * Ctrl+= (「放大细节」), as the app always took it. */
const SAME_AS: Record<string, string> = { "Ctrl++": "Ctrl+=" };

/** What a chord does where `env` says the user is (pure: the tests drive it). */
export function decide(chord: string, env: KeyEnv, table: KeyTable): KeyDecision {
  if (env.modal) return null;
  if (!table.has(chord) && SAME_AS[chord]) chord = SAME_AS[chord]!;
  if (chord === "Ctrl+K") return { host: "palette" };
  if (env.palette) return chord === "F1" ? { host: "paletteHelp" } : null; // the palette handles its own keys
  if (chord === "Ctrl+Tab" || chord === "Ctrl+PageDown") return { host: "nextTab" };
  if (chord === "Ctrl+Shift+Tab" || chord === "Ctrl+PageUp") return { host: "prevTab" };
  const bound = table.get(chord) ?? [];
  // the window's commands, on every tab; from a text field only those that may leave it
  const wide = bound.find((c) => appWide(c) && (!env.typing || typingOk(chord)));
  if (wide) return asCommand(wide, false);
  if (env.tab === "help" || env.tab === "settings") {
    if (env.typing) return null;
    const help = env.tab === "help";
    if (chord === "/" || chord === "Ctrl+F") return { host: help ? "helpSearch" : "settingsSearch" };
    if (chord === "Backspace" || chord === "Alt+Left") return { host: help ? "helpBack" : "settingsBack" };
    if (chord === "Alt+Right") return { host: help ? "helpForward" : "settingsForward" };
    return null;
  }
  if (env.typing || !env.view) return null;
  if (chord === "Shift+Backspace") return { host: "viewForward" };
  if ((chord === "ContextMenu" || chord === "Shift+F10") && !env.onControl) return { host: "menu" };
  if (chord === "Escape" && !env.onControl) return { host: "escape" };
  if (chord === "Enter") return env.onControl || !env.selected ? null : { host: "zoom" };
  if (env.onControl && (chord === "Space" || chord === "Delete")) return null;
  if (env.typeAhead && bare(chord) && chord.length === 1) return null;
  const view = bound.filter((c) => !appWide(c) && !(bare(chord) && /^[0-9]$/.test(chord) && c.id.startsWith("tag.") && !env.bareTags));
  if (view.length) return asCommand(view[0]!, true);
  // Shift with a cycling key: the other way (V / Shift+V)
  if (chord.startsWith("Shift+") && /^[A-Z]$/.test(chord.slice(6)) && !env.typeAhead) {
    const plain = (table.get(chord.slice(6)) ?? []).find((c) => c.action?.kind === "cycle");
    if (plain) return asCommand(plain, true, true);
  }
  return null;
}

/** Where the keyboard event happened. */
function envOf(e: KeyboardEvent): KeyEnv {
  const s = get();
  const target = e.target as HTMLElement | null;
  const tab = activeTab(s);
  const viewId = focusedViewId(s);
  const st = viewId ? s.views[viewId]?.state : undefined;
  return {
    tab: tab.kind,
    view: !!st,
    compare: !!st?.compare,
    selected: st?.selected != null,
    typing: !!target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable),
    onControl: !!target?.closest?.("button, a, select, [role=button], [role=radio], [role=menuitem], [role=menuitemradio], [role=option], [role=switch], [role=tab]"),
    typeAhead: target?.tagName === "SELECT",
    bareTags: setting(s, "tags.omit_ctrl", true),
    palette: s.paletteOpen,
    modal: visibleSurface(s.update, s.updateOpen) === "dialog",
  };
}

function runHost(key: HostKey): void {
  const s = get();
  const tab = activeTab(s);
  const viewId = focusedViewId(s);
  const view = viewId ? s.views[viewId] : undefined;
  const st = view?.state;
  switch (key) {
    case "palette":
      return set({ paletteOpen: !s.paletteOpen });
    case "paletteHelp":
      set({ paletteOpen: false });
      return openHelpTab("command-palette");
    case "nextTab":
    case "prevTab": {
      if (!s.tabs.length) return newStartTab();
      const idx = s.tabs.findIndex((x) => x.id === tab.id);
      const step = key === "prevTab" ? -1 : 1;
      return set({ active: s.tabs[(idx + step + s.tabs.length) % s.tabs.length]!.id });
    }
    case "helpSearch":
      return void document.getElementById("sk-help-search")?.focus();
    case "helpBack":
      return helpHistory(-1);
    case "helpForward":
      return helpHistory(1);
    case "settingsSearch":
      return void document.getElementById("sk-settings-search")?.focus();
    case "settingsBack":
      return settingsHistory(-1);
    case "settingsForward":
      return settingsHistory(1);
    case "viewForward":
      return void viewCall(viewId, "view.forward");
    case "menu":
      if (viewId && st && view?.frame) keyboardMenu(viewId, st.scan_id, view.frame);
      return;
    case "zoom":
      if (st?.selected != null) void viewCall(viewId, "view.zoom", { key: st.selected });
      return;
    case "escape":
      if (!viewId || !st) return;
      // compare mode: Esc clears the selection, then leaves the comparison (W6 待定 16)
      if (tab.compare && st.selected == null) return void exitCompare(tab.id);
      // then, with nothing selected: leave filtering (the 「匹配结果」 panel goes back to the details)
      if (st.selected == null && view?.filter?.active) return clearFilter(viewId);
      return void viewCall(viewId, "view.select", { key: null });
  }
}

/** The global key handler (UI-MAIN-58), on window keydown (App.tsx). */
export function onKey(e: KeyboardEvent): void {
  const chord = chordOf(e);
  if (!chord) return;
  const d = decide(chord, envOf(e), keyTable());
  if (!d) return;
  // a command this window does nothing with yet (Space's preview): the key stays the page's
  if ("command" in d && !handled(d.command)) return;
  e.preventDefault();
  if ("host" in d) return runHost(d.host);
  if (d.once && e.repeat) return;
  void runShortcut(d.command, { quiet: d.quiet, reverse: d.reverse });
}

/** Whether running the command does something here: an action of its own, or the app's handler. */
function handled(id: string): boolean {
  const c = get().commands.find((x) => x.id === id);
  return !!c?.action || !!c?.plugin || id in BUILTIN_ACTIONS;
}

/** The keyboard's context menu: for the selected row, or for the zoom root when nothing is selected
 * (the flame graph's root bar, the polar flame graph's centre disc). The view's host says where the
 * row is drawn; nothing opens for a row it doesn't draw. */
function keyboardMenu(viewId: string, scanId: string, frame: Pick<VisualFrame, "info" | "key" | "node">): void {
  const sel = frame.info.selected_row;
  const target = sel != null ? { key: frame.key[sel]!, node: frame.node[sel]! } : frame.info.root;
  const at = target && hosts.get(viewId)?.menuAnchor(sel ?? HIT_ROOT);
  if (!target || !at) return;
  set({ menu: { viewId, scanId, key: target.key, node: target.node > 0 ? target.node : null, ...at } });
}

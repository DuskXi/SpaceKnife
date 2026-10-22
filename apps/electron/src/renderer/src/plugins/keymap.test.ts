// The single key table (docs/plugins.md 「键位」): chords as commands write them and as key events
// give them, where a key may act (text fields, buttons, drop-downs, tabs), and that the keys the app
// handled by hand before keep doing what they did (F5, Shift+F5, Ctrl+S, the view's single keys).

import { describe, expect, it } from "vitest";

import type { PaletteCommand } from "@/protocol/generated";

import { buildTable, chordOf, decide, type KeyEnv, type KeyLike, normalizeKey } from "./keymap";

const cmd = (id: string, keys: string[], extra: Partial<PaletteCommand> = {}): PaletteCommand => ({
  id,
  name: id,
  keys,
  enabled: true,
  status: "",
  hits: [],
  icon: "",
  more: false,
  action: null,
  menus: [],
  plugin: null,
  ...extra,
});

// the app's commands with keys, as palette.commands lists them (palette/commands.py)
const COMMANDS: PaletteCommand[] = [
  cmd("view.rescan", ["F5"]),
  cmd("scan.rescan_all", ["Shift+F5"]),
  cmd("view.cycle_color", ["C"], { action: { kind: "cycle", what: "color", step: 1 } }),
  cmd("view.toggle_free", ["F"], { action: { kind: "toggle", name: "show_free" } }),
  cmd("snapshot.save", ["Ctrl+S"]),
  cmd("app.settings", ["Ctrl+,"]),
  cmd("nav.back", ["Backspace", "Alt+Left"]),
  cmd("nav.forward", ["Alt+Right"]),
  cmd("nav.up", ["Alt+Up"]),
  cmd("nav.home", ["Ctrl+Home"]),
  cmd("view.detail_up", ["Ctrl+="]),
  cmd("view.detail_down", ["Ctrl+-"]),
  cmd("view.cycle_visual", ["V"], { action: { kind: "cycle", what: "visual", step: 1 } }),
  cmd("filter.focus", ["/", "Ctrl+F"]),
  cmd("path.edit", ["Ctrl+L"]),
  cmd("view.zoom_in", ["Enter"]),
  cmd("basket.add", ["Delete"]),
  cmd("item.copy_path", ["Ctrl+C"]),
  cmd("item.reveal", ["Ctrl+E"]),
  cmd("item.terminal", ["Ctrl+`"]),
  cmd("tab.new", ["Ctrl+T"]),
  cmd("tab.close", ["Ctrl+W"]),
  cmd("tab.move_left", ["Ctrl+Shift+PageUp"]),
  cmd("tab.move_right", ["Ctrl+Shift+PageDown"]),
  cmd("tag.red", ["1", "Ctrl+1"]),
  cmd("tag.clear", ["0", "Ctrl+0"]),
  cmd("view.toggle_unknown", ["U"], { action: { kind: "toggle", name: "show_unknown" } }),
  cmd("app.log", ["Ctrl+J"]),
  cmd("app.help", ["F1"]),
];

const key = (k: string, mods: { ctrl?: boolean; meta?: boolean; shift?: boolean; alt?: boolean; code?: string } = {}): KeyLike => ({
  key: k,
  code: mods.code,
  ctrlKey: !!mods.ctrl,
  metaKey: !!mods.meta,
  shiftKey: !!mods.shift,
  altKey: !!mods.alt,
});

const VIEW: KeyEnv = { tab: "scan", view: true, compare: false, selected: true, typing: false, onControl: false, typeAhead: false, bareTags: true, palette: false, modal: false };
const table = buildTable(COMMANDS);
const run = (e: KeyLike, env: Partial<KeyEnv> = {}) => {
  const chord = chordOf(e);
  return chord ? decide(chord, { ...VIEW, ...env }, table) : null;
};
const command = (e: KeyLike, env: Partial<KeyEnv> = {}) => {
  const d = run(e, env);
  return d && "command" in d ? d.command : null;
};

describe("chords", () => {
  it("normalizes the commands' notation", () => {
    expect(normalizeKey("ctrl+alt+d")).toBe("Ctrl+Alt+D");
    expect(normalizeKey("Alt+Left")).toBe("Alt+Left");
    expect(normalizeKey("Shift+Ctrl+PgDn")).toBe("Ctrl+Shift+PageDown");
    expect(normalizeKey("Cmd+K")).toBe("Ctrl+K");
    expect(normalizeKey("Ctrl++")).toBe("Ctrl++");
    expect(normalizeKey("f5")).toBe("F5");
    expect(normalizeKey(" Ctrl + , ")).toBe("Ctrl+,");
  });

  it("reads key events the same way (⌘ is Ctrl, Caps Lock does not matter)", () => {
    expect(chordOf(key("s", { ctrl: true }))).toBe("Ctrl+S");
    expect(chordOf(key("S", { ctrl: true }))).toBe("Ctrl+S");
    expect(chordOf(key("s", { meta: true }))).toBe("Ctrl+S");
    expect(chordOf(key("F5", { shift: true }))).toBe("Shift+F5");
    expect(chordOf(key("ArrowLeft", { alt: true }))).toBe("Alt+Left");
    expect(chordOf(key("PageDown", { ctrl: true, shift: true }))).toBe("Ctrl+Shift+PageDown");
    expect(chordOf(key(" "))).toBe("Space");
    expect(chordOf(key("Shift", { shift: true }))).toBeNull();
  });

  it("leaves Shift out of characters it makes, keeps it for letters and named keys", () => {
    expect(chordOf(key("?", { shift: true }))).toBe("?");
    expect(chordOf(key("V", { shift: true }))).toBe("Shift+V");
    expect(chordOf(key("Backspace", { shift: true }))).toBe("Shift+Backspace");
  });

  it("reads letters from the key code on another layout or with Option", () => {
    expect(chordOf(key("с", { ctrl: true, code: "KeyC" }))).toBe("Ctrl+C"); // Cyrillic layout
    expect(chordOf(key("ß", { alt: true, code: "KeyS" }))).toBe("Alt+S"); // macOS Option
  });
});

describe("the keys the app handled by hand before", () => {
  it("F5, Shift+F5, Ctrl+S and ⌘S name their commands, held down they run once", () => {
    expect(run(key("F5"))).toEqual({ command: "view.rescan", quiet: false, reverse: false, once: true });
    expect(command(key("F5", { shift: true }))).toBe("scan.rescan_all");
    expect(command(key("s", { ctrl: true }))).toBe("snapshot.save");
    expect(command(key("S", { ctrl: true }))).toBe("snapshot.save");
    expect(command(key("s", { meta: true }))).toBe("snapshot.save");
  });

  it("leaves every other combination of those keys alone", () => {
    for (const k of [key("s"), key("S", { shift: true }), key("s", { ctrl: true, shift: true }), key("s", { ctrl: true, alt: true }), key("F5", { ctrl: true }), key("F5", { alt: true }), key("F6")]) {
      expect(command(k)).toBeNull();
    }
  });

  it("the window's keys work on every tab, the view's only in a view", () => {
    for (const tab of ["start", "settings", "help", "machine"] as const) {
      expect(command(key("t", { ctrl: true }), { tab, view: false })).toBe("tab.new");
      expect(command(key("j", { ctrl: true }), { tab, view: false })).toBe("app.log");
      expect(command(key("F5"), { tab, view: false })).toBe("view.rescan"); // says why not there
      expect(command(key("f"), { tab, view: false })).toBeNull();
    }
    expect(command(key("f"))).toBe("view.toggle_free");
    expect(command(key("Delete"))).toBe("basket.add");
  });

  it("view keys say nothing where they don't apply; window keys say why", () => {
    expect(run(key("f"))).toMatchObject({ quiet: true });
    expect(run(key("w", { ctrl: true }))).toMatchObject({ quiet: false });
  });

  it("from a text field only Ctrl / Alt chords and F-keys, never the editing keys", () => {
    const typing = { typing: true };
    expect(command(key("t", { ctrl: true }), typing)).toBe("tab.new");
    expect(command(key("F5"), typing)).toBe("view.rescan");
    expect(command(key("F1"), typing)).toBe("app.help");
    expect(command(key("c", { ctrl: true }), typing)).toBeNull(); // copy the text, not the path
    expect(command(key("f"), typing)).toBeNull();
    expect(command(key("Backspace"), typing)).toBeNull();
    expect(command(key("ArrowLeft", { alt: true }), typing)).toBeNull();
  });

  it("a focused button keeps Enter, Space, Delete and Escape; a drop-down its letters", () => {
    expect(run(key("Enter"), { onControl: true })).toBeNull();
    expect(run(key("Delete"), { onControl: true })).toBeNull();
    expect(run(key("Escape"), { onControl: true })).toBeNull();
    expect(run(key("c"), { typeAhead: true })).toBeNull();
    expect(command(key("c"))).toBe("view.cycle_color");
  });

  it("Enter zooms into the selection, nothing without one; Escape and the menu key are the view's", () => {
    expect(run(key("Enter"))).toEqual({ host: "zoom" });
    expect(run(key("Enter"), { selected: false })).toBeNull();
    expect(run(key("Escape"))).toEqual({ host: "escape" });
    expect(run(key("F10", { shift: true }))).toEqual({ host: "menu" });
    expect(run(key("ContextMenu"))).toEqual({ host: "menu" });
    expect(run(key("Backspace", { shift: true }))).toEqual({ host: "viewForward" });
  });

  it("Shift with a cycling key cycles the other way", () => {
    expect(run(key("V", { shift: true }))).toEqual({ command: "view.cycle_visual", quiet: true, reverse: true, once: false });
    expect(run(key("C", { shift: true }))).toMatchObject({ command: "view.cycle_color", reverse: true });
    expect(run(key("F", { shift: true }))).toBeNull(); // not a cycle
  });

  it("bare digits tag only with 「省略 Ctrl」 on; Ctrl+digits always", () => {
    expect(command(key("1"))).toBe("tag.red");
    expect(command(key("1"), { bareTags: false })).toBeNull();
    expect(command(key("1", { ctrl: true }), { bareTags: false })).toBe("tag.red");
    expect(command(key("0", { ctrl: true }))).toBe("tag.clear");
  });

  it("the palette key, tab switching and the help and settings tabs' own keys", () => {
    expect(run(key("k", { ctrl: true }), { palette: true })).toEqual({ host: "palette" });
    expect(run(key("F1"), { palette: true })).toEqual({ host: "paletteHelp" });
    expect(run(key("f"), { palette: true })).toBeNull();
    expect(run(key("Tab", { ctrl: true }))).toEqual({ host: "nextTab" });
    expect(run(key("Tab", { ctrl: true, shift: true }))).toEqual({ host: "prevTab" });
    expect(run(key("PageUp", { ctrl: true }))).toEqual({ host: "prevTab" });
    expect(run(key("/"), { tab: "help", view: false })).toEqual({ host: "helpSearch" });
    expect(run(key("f", { ctrl: true }), { tab: "settings", view: false })).toEqual({ host: "settingsSearch" });
    expect(run(key("Backspace"), { tab: "settings", view: false })).toEqual({ host: "settingsBack" });
    expect(run(key("ArrowRight", { alt: true }), { tab: "help", view: false })).toEqual({ host: "helpForward" });
  });

  it("Ctrl with the number pad's + is Ctrl+=", () => {
    expect(command(key("+", { ctrl: true }))).toBe("view.detail_up");
    expect(command(key("=", { ctrl: true }))).toBe("view.detail_up");
    expect(command(key("-", { ctrl: true }))).toBe("view.detail_down");
  });

  it("nothing behind a modal dialog", () => {
    expect(run(key("k", { ctrl: true }), { modal: true })).toBeNull();
    expect(run(key("F5"), { modal: true })).toBeNull();
  });
});

describe("plugins' keys", () => {
  it("run on every tab (a plugin's command says why not), once when held", () => {
    const t2 = buildTable([...COMMANDS, cmd("t.full.big", ["Ctrl+Alt+B"], { plugin: "t.full" })]);
    const d = decide("Ctrl+Alt+B", { ...VIEW, tab: "start", view: false }, t2);
    expect(d).toEqual({ command: "t.full.big", quiet: false, reverse: false, once: true });
  });

  it("take a key the order of palette.commands gives first (the backend settles conflicts)", () => {
    const t2 = buildTable([cmd("a.one.x", ["Ctrl+Alt+X"], { plugin: "a.one" }), cmd("b.two.x", ["ctrl+alt+x"], { plugin: "b.two" })]);
    expect(t2.get("Ctrl+Alt+X")!.map((c) => c.id)).toEqual(["a.one.x", "b.two.x"]);
    expect(decide("Ctrl+Alt+X", VIEW, t2)).toMatchObject({ command: "a.one.x" });
  });

  it("a key pack's rebinding of an app command is just its command with that key", () => {
    const t2 = buildTable([...COMMANDS, cmd("view.rescan", ["F5", "Ctrl+Alt+J"])]);
    expect(decide("Ctrl+Alt+J", VIEW, t2)).toMatchObject({ command: "view.rescan" });
  });
});

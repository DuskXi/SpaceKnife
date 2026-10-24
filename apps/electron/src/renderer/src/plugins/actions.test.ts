// Every kind of action does its one thing (docs/plugins.md 「动作」), with a plugin's actions held to
// that plugin: its own methods and settings only, host.open / clipboard where needed, the delete
// basket never more than a proposal.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { basketAdd, call, editFilter, setViewSettings, updateSettings, viewCall } from "@/state/actions";
import { get, set } from "@/state/store";

import { actionContext, BUILTIN_ACTIONS, COMMANDS, runAction } from "./actions";
import { startCleanup } from "./cleanup";
import { callPlugin } from "./client";
import { ask } from "./confirm";

vi.mock(import("@/state/actions"), async (original) => ({
  ...(await original()),
  call: vi.fn(),
  setViewSettings: vi.fn(),
  viewCall: vi.fn(),
  editFilter: vi.fn(),
  updateSettings: vi.fn(),
  basketAdd: vi.fn(),
}));
vi.mock(import("./client"), async (original) => ({ ...(await original()), callPlugin: vi.fn() }));
vi.mock(import("./cleanup"), async (original) => ({ ...(await original()), startCleanup: vi.fn(), reviewCleanup: vi.fn() }));
vi.mock(import("./confirm"), async (original) => ({ ...(await original()), ask: vi.fn() }));
vi.mock(import("./sources"), async (original) => ({ ...(await original()), startSourceScan: vi.fn() }));

const runShortcut = vi.fn();
const cycleVisual = vi.fn();
const pickVisual = vi.fn();
Object.assign(COMMANDS, { runShortcut, cycleVisual, pickVisual });

const shell = { copyText: vi.fn(), showItemInFolder: vi.fn(), openPath: vi.fn(), openTerminal: vi.fn(), openUrl: vi.fn() };

const view = (over: Record<string, unknown> = {}) => ({
  state: { scan_id: "s1", selected: 7, color_mode: "class", visual: "treemap", show_free: false, show_unknown: true, show_trash: false, has_trash: true, compare: false, zoom_node: 3, ...over },
  info: { layout: "drawlist.v1" },
});

beforeEach(() => {
  vi.stubGlobal("window", { sk: { shell, platform: "linux" } });
  set({
    tabs: [{ id: "t1", kind: "scan", scanId: "s1", viewIds: ["v1"], focusedPane: 0, title: "C:" }],
    active: "t1",
    views: { v1: view() } as never,
    visuals: [{ id: "treemap", layout: "drawlist.v1" }] as never,
    toast: null,
    extensions: null,
    plugins: { plugins: [{ id: "a.b", name: "A", granted: ["clipboard"] }], folder: "", dev_mode: false } as never,
    settings: { values: { "plugin.a.b.fast": false } } as never,
  });
});
afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

const app = { owner: null };
const plugin = { owner: "a.b" };

describe("runAction", () => {
  it("builtin: the app's own handler by command id, or says it is not wired up", async () => {
    const run = vi.fn();
    BUILTIN_ACTIONS["test.thing"] = run;
    await runAction({ kind: "builtin", name: "test.thing" }, app);
    expect(run).toHaveBeenCalledTimes(1);
    await runAction({ kind: "builtin", name: "no.such" }, app);
    expect(get().toast).toBe("这个命令还没有接入");
    delete BUILTIN_ACTIONS["test.thing"];
  });

  it("call: the plugin's method with where the user is; a report answer opens the dialog", async () => {
    vi.mocked(callPlugin).mockResolvedValue({ type: "report", title: "R", blocks: [], actions: [], subtitle: "", footnote: "", empty: "" });
    await runAction({ kind: "call", method: "plugin.a.b.find", args: { x: 1 } }, plugin);
    expect(callPlugin).toHaveBeenCalledWith("plugin.a.b.find", { scan_id: "s1", view_id: "v1", node: 7, selection: [7], compare_id: null, args: { x: 1 }, zoom_node: 3, filter: "" });
    expect(get().pluginDialog).toMatchObject({ plugin: "a.b", title: "R" });
  });

  it("call: a toast answer shows; another plugin's method is refused", async () => {
    vi.mocked(callPlugin).mockResolvedValue({ type: "toast", text: "你好", tone: "ok" });
    await runAction({ kind: "call", method: "plugin.a.b.hello", args: {} }, plugin);
    expect(get().toast).toBe("你好");
    vi.mocked(callPlugin).mockClear();
    await runAction({ kind: "call", method: "plugin.c.d.steal", args: {} }, plugin);
    expect(callPlugin).not.toHaveBeenCalled();
    expect(get().toast).toBe("插件只能调用它自己的方法");
  });

  it("view, toggle, filter: the focused view's settings and filter", async () => {
    await runAction({ kind: "view", settings: { color_mode: "time" } }, app);
    expect(setViewSettings).toHaveBeenCalledWith("v1", { color_mode: "time" });
    await runAction({ kind: "view", settings: { visual: "flame" } }, app);
    expect(pickVisual).toHaveBeenCalledWith("v1", "flame");
    await runAction({ kind: "toggle", name: "show_unknown" }, app);
    expect(setViewSettings).toHaveBeenLastCalledWith("v1", { show_unknown: false });
    await runAction({ kind: "filter", expr: "*.log" }, plugin);
    expect(editFilter).toHaveBeenCalledWith("v1", "*.log", null, true);
  });

  it("toggle: no free or unknown blocks in a comparison, no recycle bin without one", async () => {
    set({ views: { v1: view({ compare: true, has_trash: false }) } as never });
    await runAction({ kind: "toggle", name: "show_free" }, app);
    await runAction({ kind: "toggle", name: "show_trash" }, app);
    expect(setViewSettings).not.toHaveBeenCalled();
  });

  it("cycle: visualizations (Shift the other way), colour modes past the built-in ones", async () => {
    await runAction({ kind: "cycle", what: "visual", step: 1 }, { owner: null, reverse: true });
    expect(cycleVisual).toHaveBeenCalledWith("v1", -1);
    set({ extensions: { color_modes: [{ id: "a.b.media", plugin: "a.b", title: "M", palette: [], legend: [], folders: "ring", layouts: [] }] } as never, views: { v1: view({ color_mode: "branch" }) } as never });
    await runAction({ kind: "cycle", what: "color", step: 1 }, app);
    expect(setViewSettings).toHaveBeenCalledWith("v1", { color_mode: "a.b.media" });
  });

  it("command: through the palette's rules, a few levels deep at most", async () => {
    await runAction({ kind: "command", command: "view.rescan", args: {} }, app);
    expect(runShortcut).toHaveBeenCalledWith("view.rescan", { depth: 1 });
    await runAction({ kind: "command", command: "view.rescan", args: {} }, { owner: null, depth: 4 });
    expect(runShortcut).toHaveBeenCalledTimes(1);
    // 「导出报告…」 opened on a format (report.export with args.format)
    await runAction({ kind: "command", command: "report.export", args: { format: "a.b.md" } }, app);
    expect(get()).toMatchObject({ exportOpen: true, exportPick: "a.b.md" });
  });

  it("setting: a plugin changes its own settings (or a few display ones), toggles flip", async () => {
    await runAction({ kind: "setting", key: "plugin.a.b.fast", toggle: true, value: null }, plugin);
    expect(updateSettings).toHaveBeenCalledWith({ "plugin.a.b.fast": true });
    await runAction({ kind: "setting", key: "appearance.hover_highlight", toggle: false, value: false }, plugin);
    expect(updateSettings).toHaveBeenLastCalledWith({ "appearance.hover_highlight": false });
    vi.mocked(updateSettings).mockClear();
    await runAction({ kind: "setting", key: "scan.method", toggle: false, value: "hard" }, plugin);
    expect(updateSettings).not.toHaveBeenCalled();
    expect(get().toast).toBe("插件不能修改这项设置：scan.method");
  });

  it("cleanup: its own plugin's cleanup goes to review", async () => {
    await runAction({ kind: "cleanup", cleanup: "a.b.logs", method: "plugin.a.b.logs" }, plugin);
    expect(startCleanup).toHaveBeenCalledWith("plugin.a.b.logs", "a.b", expect.objectContaining({ scan_id: "s1", node: 7 }));
    vi.mocked(startCleanup).mockClear();
    await runAction({ kind: "cleanup", cleanup: "c.d.all", method: "plugin.c.d.all" }, plugin);
    expect(startCleanup).not.toHaveBeenCalled();
  });

  it("node: select and zoom in the view; basket proposes with the plugin as origin", async () => {
    await runAction({ kind: "node", op: "select" }, plugin);
    expect(viewCall).toHaveBeenCalledWith("v1", "view.reveal", { key: 7 });
    await runAction({ kind: "node", op: "zoom" }, { owner: "a.b", node: 9 });
    expect(viewCall).toHaveBeenLastCalledWith("v1", "view.zoom", { key: 9 });
    await runAction({ kind: "node", op: "basket" }, plugin);
    expect(basketAdd).toHaveBeenCalledWith("s1", 7, "a.b");
  });

  it("node: copying needs clipboard, opening needs host.open", async () => {
    vi.mocked(call).mockResolvedValue({ path: "/data/x.log", on_disk: true, node: { kind: "file" } } as never);
    await runAction({ kind: "node", op: "copy_path" }, plugin);
    expect(shell.copyText).toHaveBeenCalledWith("/data/x.log");
    await runAction({ kind: "node", op: "reveal" }, plugin);
    expect(shell.showItemInFolder).not.toHaveBeenCalled();
    expect(get().toast).toBe("插件「A」没有「host.open」权限");
    await runAction({ kind: "node", op: "reveal" }, app);
    expect(shell.showItemInFolder).toHaveBeenCalledWith("/data/x.log");
  });

  it("url: https only, needs host.open, and asks first", async () => {
    set({ plugins: { plugins: [{ id: "a.b", name: "A", granted: ["host.open"] }], folder: "", dev_mode: false } as never });
    vi.mocked(ask).mockResolvedValue(false);
    await runAction({ kind: "url", url: "https://example.invalid/" }, plugin);
    expect(ask).toHaveBeenCalledTimes(1);
    expect(shell.openUrl).not.toHaveBeenCalled();
    vi.mocked(ask).mockResolvedValue(true);
    await runAction({ kind: "url", url: "https://example.invalid/" }, plugin);
    expect(shell.openUrl).toHaveBeenCalledWith("https://example.invalid/");
    await runAction({ kind: "url", url: "http://example.invalid/" } as never, plugin);
    expect(shell.openUrl).toHaveBeenCalledTimes(1);
  });

  it("filter and view from a report's tab bring the scan's tab forward", async () => {
    set({ tabs: [...get().tabs, { id: "t2", kind: "plugin", scanId: "s1", viewIds: [], focusedPane: 0, title: "R", plugin: { kind: "report", id: "a.b.r" } }] as never, active: "t2" });
    await runAction({ kind: "filter", expr: "*.log" }, { owner: "a.b", viewId: "v1" });
    expect(editFilter).toHaveBeenCalledWith("v1", "*.log", null, true);
    expect(get().active).toBe("t1");
  });

  it("scan_source: lists the selected file with the plugin's source", async () => {
    const start = vi.mocked((await import("./sources")).startSourceScan).mockResolvedValue("s9");
    set({ extensions: { sources: [{ id: "a.b.zip", plugin: "a.b", title: "Zip" }] } as never });
    vi.mocked(call).mockResolvedValue({ path: "/data/pack.zip", on_disk: true } as never);
    await runAction({ kind: "scan_source", source: "a.b.zip", uri: "$node" }, plugin);
    expect(call).toHaveBeenCalledWith("tree.node", { scan_id: "s1", node: 7 });
    expect(start).toHaveBeenCalledWith(expect.objectContaining({ id: "a.b.zip" }), "/data/pack.zip");
    vi.mocked(call).mockResolvedValue({ path: "pack.zip/x", on_disk: false } as never);
    await runAction({ kind: "scan_source", source: "a.b.zip", uri: "$node" }, plugin);
    expect(start).toHaveBeenCalledTimes(1);
    expect(get().toast).toBe("这个位置不在磁盘上");
  });

  it("an unknown kind says so instead of throwing", async () => {
    await runAction({ kind: "teleport" } as never, app);
    expect(get().toast).toBe("这个界面不认识这种动作：teleport");
  });
});

describe("actionContext", () => {
  it("the node a menu or a row names wins over the selection", () => {
    expect(actionContext({ owner: null, node: 3, selection: [3, 4] })).toMatchObject({ node: 3, selection: [3, 4], scan_id: "s1" });
    // an aggregate block (a synthetic key) is not a node
    set({ views: { v1: view({ selected: 0xc0000001 }) } as never });
    expect(actionContext({ owner: null })).toMatchObject({ node: null, selection: [] });
  });
});

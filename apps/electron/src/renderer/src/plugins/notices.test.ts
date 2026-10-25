// Plugins' notices (ctx.notify): kept on screen (at most three), a system notification asked for each
// (the main process shows it only while the window is minimized), the button runs the plugin's own
// command with the notice's args, the time on screen runs only while the window is looked at; and
// the other round-4 additions on the panel side: sk.host.compare needs snapshots.read, panels hear
// of new snapshots.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import bridgeJs from "../../../../resources/plugin-sdk/bridge.js?raw";

import type { PluginNoticeEvent } from "@/protocol/generated";
import { get, set } from "@/state/store";

import { COMMANDS } from "./actions";
import { dismissNotice, MAX_NOTICES, NOTICE_MS, noticeButton, onNotice, runNotice, systemTitle, tickNotices } from "./notices";
import { PanelBridge, PERMISSION_DENIED } from "./panel/bridge";
import { FakePort, mockHost, settle } from "./testing/mockBridge";

const runShortcut = vi.fn();
Object.assign(COMMANDS, { runShortcut });
const notify = vi.fn();

const notice = (over: Partial<PluginNoticeEvent> = {}): PluginNoticeEvent => ({
  plugin: "a.b",
  id: "a.b#1",
  title: "磁盘涨了",
  body: "下载文件夹多了 3 GB",
  tone: "warn",
  command: "a.b.show",
  button: "",
  args: { folder: "Downloads" },
  ...over,
});

beforeEach(() => {
  vi.stubGlobal("window", { sk: { platform: "linux", window: { notify } } });
  set({ notices: [], commands: [{ id: "a.b.show", name: "看看增长" }] as never });
});
afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("notices", () => {
  it("are kept (the newest three) and each asks for a system notification", () => {
    for (let i = 1; i <= MAX_NOTICES + 1; i++) onNotice(notice({ id: `a.b#${i}` }), "增长监视");
    expect(get().notices.map((n) => n.id)).toEqual(["a.b#2", "a.b#3", "a.b#4"]);
    expect(notify).toHaveBeenCalledTimes(MAX_NOTICES + 1);
    expect(notify).toHaveBeenLastCalledWith({ id: "a.b#4", title: "增长监视：磁盘涨了", body: "下载文件夹多了 3 GB" });
    expect(systemTitle(notice(), "")).toBe("磁盘涨了");
  });

  it("the button says its own text, else its command's title; none without a command", () => {
    expect(noticeButton(notice())).toBe("看看增长");
    expect(noticeButton(notice({ button: "查看" }))).toBe("查看");
    expect(noticeButton(notice({ command: null }))).toBe("");
  });

  it("the button runs the plugin's own command with the notice's args and closes it", async () => {
    onNotice(notice());
    await runNotice("a.b#1");
    expect(runShortcut).toHaveBeenCalledWith("a.b.show", { args: { folder: "Downloads" } });
    expect(get().notices).toEqual([]);
    // another plugin's command never runs from a notice
    onNotice(notice({ id: "a.b#2", command: "other.plugin.delete" }));
    await runNotice("a.b#2");
    expect(runShortcut).toHaveBeenCalledTimes(1);
    dismissNotice("a.b#9"); // gone already: nothing happens
  });

  it("stay for NOTICE_MS of the window being looked at", () => {
    const shown = new Map<string, number>();
    expect(tickNotices(shown, ["x", "y"], NOTICE_MS - 1)).toEqual([]);
    expect(tickNotices(shown, ["x"], 1)).toEqual(["x"]);
    expect([...shown.keys()]).toEqual(["x"]); // y left meanwhile: forgotten
  });
});

describe("panels", () => {
  function connected(permissions: string[]) {
    const port = new FakePort();
    const host = mockHost({ permissions });
    const bridge = new PanelBridge(host, port, { base: "skplugin://example.panel/" });
    port.panel({ type: "ready", href: "skplugin://example.panel/index.html" });
    return { port, host, bridge };
  }

  it("open a comparison with snapshots.read only", async () => {
    const without = connected([]);
    without.port.panel({ type: "host", id: 1, op: "compare", args: { base: "/s/a.sks", target: "now" } });
    const withIt = connected(["snapshots.read"]);
    withIt.port.panel({ type: "host", id: 2, op: "compare", args: { base: "/s/a.sks", target: "now" } });
    await settle();
    expect(without.port.of("error")).toMatchObject([{ id: 1, code: PERMISSION_DENIED }]);
    expect(withIt.host.hostOps).toEqual([{ op: "compare", args: { base: "/s/a.sks", target: "now" } }]);
  });

  it("hear of new snapshots (bridge.js reads the message)", () => {
    const c = connected(["snapshots.read"]);
    c.bridge.push("snapshots", { saved: ["/s/b.sks"], removed: [] });
    expect(c.port.of("snapshots")).toEqual([{ type: "snapshots", saved: ["/s/b.sks"], removed: [] }]);
    expect(bridgeJs).toContain('case "snapshots":');
    expect(bridgeJs).toContain('host("compare")');
  });
});

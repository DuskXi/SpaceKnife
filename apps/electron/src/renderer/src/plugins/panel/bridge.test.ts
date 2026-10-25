// The web panel bridge (docs/plugins.md 「消息桥」): only that iframe's hello counts, only the
// plugin's own documents get a port, calls go to its own methods or the allowed core methods with
// their permissions, host operations need theirs, limits hold, columns are handed over, not copied.

import { describe, expect, it } from "vitest";

import bridgeJs from "../../../../../resources/plugin-sdk/bridge.js?raw";

import { FakePort, mockHost, settle } from "../testing/mockBridge";
import { themeVars } from "./theme";
import { CORE_METHODS, isHello, MAX_CALLS, MAX_MESSAGE, MAX_RATE, PanelBridge, PERMISSION_DENIED, PLUGIN_LIMIT, transferable } from "./bridge";

const BASE = "skplugin://example.panel/";

function connected(permissions: string[] = []) {
  const port = new FakePort();
  const host = mockHost({ permissions });
  const resized: number[] = [];
  let ready = 0;
  let refused = "";
  const bridge = new PanelBridge(host, port, { base: BASE, onResize: (h) => resized.push(h), onReady: () => ready++, onRefused: (h) => (refused = h) });
  port.panel({ type: "ready", href: `${BASE}index.html?x=1` });
  return { port, host, bridge, resized, ready: () => ready, refused: () => refused };
}

describe("handshake", () => {
  it("takes a hello only from that iframe's window", () => {
    const frame = {};
    expect(isHello({ source: frame, data: { sk: 1, type: "hello" } }, frame)).toBe(true);
    expect(isHello({ source: {}, data: { sk: 1, type: "hello" } }, frame)).toBe(false);
    expect(isHello({ source: frame, data: { sk: 2, type: "hello" } }, frame)).toBe(false);
    expect(isHello({ source: frame, data: "hello" }, frame)).toBe(false);
    expect(isHello({ source: null, data: { sk: 1, type: "hello" } }, null)).toBe(false);
  });

  it("is ready when the panel says where it is, within its plugin's files", () => {
    const c = connected();
    expect(c.bridge.ready).toBe(true);
    expect(c.ready()).toBe(1);
    c.bridge.push("lang", { lang: "en" });
    expect(c.port.of("lang")).toEqual([{ type: "lang", lang: "en" }]);
  });

  it("refuses a document that is not one of the plugin's own (another plugin's panel)", () => {
    const port = new FakePort();
    let refused = "";
    const bridge = new PanelBridge(mockHost(), port, { base: BASE, onRefused: (h) => (refused = h) });
    port.panel({ type: "ready", href: "skplugin://other.plugin/index.html" });
    expect(refused).toBe("skplugin://other.plugin/index.html");
    expect(bridge.ready).toBe(false);
    expect(port.closed).toBe(true);
  });

  it("answers nothing before ready, sends nothing before ready", () => {
    const port = new FakePort();
    const host = mockHost();
    const bridge = new PanelBridge(host, port, { base: BASE });
    bridge.push("context", { scan_id: "s1" });
    port.panel({ type: "call", id: 1, method: "find", params: {} });
    expect(host.calls).toEqual([]);
    expect(port.of("context")).toEqual([]);
    expect(port.of("error")).toMatchObject([{ id: 1 }]);
  });
});

describe("calls", () => {
  it("names the plugin's own method by its local name (the plugin is the frame's, not the message's)", async () => {
    const c = connected();
    c.port.panel({ type: "call", id: 1, method: "find", params: { min_size: 1 }, plugin: "evil.plugin" });
    await settle();
    expect(c.host.calls.map((x) => x.method)).toEqual(["plugin.example.panel.find"]);
    expect(c.port.of("result")).toEqual([{ type: "result", id: 1, value: { ok: true } }]);
  });

  it("refuses dotted names and other plugins' methods", async () => {
    const c = connected();
    for (const method of ["other.plugin.find", "plugin.other.plugin.find", "Find", "../x", 5]) c.port.panel({ type: "call", id: 2, method, params: {} });
    await settle();
    expect(c.host.calls).toEqual([]);
    expect(c.port.of("error").every((e) => e.code === PERMISSION_DENIED || e.code === -32602)).toBe(true);
  });

  it("lets core methods through only from the list, each with its permission", async () => {
    const c = connected(["tree.read"]);
    c.port.panel({ type: "call", id: 1, method: "core:tree.summary", params: { scan_id: "s1", node: 0 } });
    c.port.panel({ type: "call", id: 2, method: "core:snapshot.list", params: {} });
    c.port.panel({ type: "call", id: 3, method: "core:basket.execute", params: {} });
    c.port.panel({ type: "call", id: 4, method: "core:settings.update", params: {} });
    c.port.panel({ type: "call", id: 5, method: "core:scan.list", params: {} });
    await settle();
    expect(c.host.calls.map((x) => x.method)).toEqual(["tree.summary", "scan.list"]);
    const errors = Object.fromEntries(c.port.of("error").map((e) => [e.id, e]));
    expect(errors[2]).toMatchObject({ code: PERMISSION_DENIED, message: "needs the snapshots.read permission" });
    expect(errors[3]).toMatchObject({ code: PERMISSION_DENIED });
    expect(errors[4]).toMatchObject({ code: PERMISSION_DENIED });
  });

  it("never lets a panel near deleting: no basket.execute, remove, clear or ops", () => {
    for (const m of ["basket.execute", "basket.remove", "basket.clear", "ops.undo", "ops.cancel", "app.shutdown", "settings.update", "scan.start", "snapshot.save", "session.hello"]) {
      expect(Object.hasOwn(CORE_METHODS, m)).toBe(false);
    }
  });

  it("marks what a panel puts in the basket as the panel's", async () => {
    const c = connected(["basket.propose"]);
    c.port.panel({ type: "call", id: 1, method: "core:basket.add", params: { scan_id: "s1", nodes: [3], origin: "someone.else", key: 5, view_id: "v1" } });
    await settle();
    expect(c.host.calls[0]).toMatchObject({ method: "basket.add", params: { scan_id: "s1", nodes: [3], reasons: null, origin: "example.panel.board" } });
    expect(c.host.calls[0]!.params).not.toHaveProperty("key");
  });

  it("sends the panel's context with its own plugin's calls and streams, apart from their params", async () => {
    const where = { scan_id: "s1", view_id: "v1", node: 4, selection: [4], zoom_node: 1, filter: "", args: { a: 1 } };
    const subs: { topic: string; params: Record<string, unknown> | null }[] = [];
    const port = new FakePort();
    const host = mockHost({
      permissions: ["tree.read"],
      where: () => where,
      subscribe: (topic, params) => {
        subs.push({ topic, params });
        return () => {};
      },
    });
    new PanelBridge(host, port, { base: BASE });
    port.panel({ type: "ready", href: `${BASE}index.html` });
    port.panel({ type: "call", id: 1, method: "histogram", params: { under: 3 } });
    port.panel({ type: "call", id: 2, method: "core:tree.summary", params: { scan_id: "s1", node: 0 } });
    port.panel({ type: "sub", id: 3, topic: "ticks", params: { n: 1 } });
    port.panel({ type: "sub", id: 4, topic: "ticks", params: null });
    await settle();
    // the plugin's ctx.tree is the panel's scan even when the params name none (sk.call("x", {}))
    expect(host.calls[0]!.params).toEqual({ under: 3, $where: where });
    expect(host.calls[1]!.params).toEqual({ scan_id: "s1", node: 0 }); // a core method's params as given
    expect(subs.map((s) => s.params)).toEqual([
      { n: 1, $where: where },
      { $where: where },
    ]);
  });

  it("cancels a call on the panel's word and when the bridge closes", async () => {
    const c = connected();
    let seen: AbortSignal | null = null;
    c.host.answer = (_m: string, _p: unknown, signal: AbortSignal) => {
      seen = signal;
      return new Promise((resolve) => signal.addEventListener("abort", () => resolve(null)));
    };
    c.port.panel({ type: "call", id: 7, method: "slow", params: {} });
    await settle();
    c.port.panel({ type: "cancel", id: 7 });
    await settle();
    expect(seen!.aborted).toBe(true);
    expect(c.port.of("error")).toMatchObject([{ id: 7, code: -32800 }]);
    c.port.panel({ type: "call", id: 8, method: "slow", params: {} });
    await settle();
    c.bridge.close();
    expect(seen!.aborted).toBe(true);
    expect(c.port.closed).toBe(true);
  });

  it("passes the plugin's error code and message on", async () => {
    const c = connected();
    c.host.answer = () => Promise.reject(Object.assign(new Error("没有找到"), { code: -32019 }));
    c.port.panel({ type: "call", id: 1, method: "find", params: {} });
    await settle();
    expect(c.port.of("error")).toEqual([{ type: "error", id: 1, code: -32019, message: "没有找到" }]);
  });

  it("hands columns over without copying, each buffer once", async () => {
    const c = connected();
    const buf = new ArrayBuffer(64);
    const value = { columns: { a: new Int32Array(buf, 0, 4), b: new Int32Array(buf, 16, 4), c: new Float64Array(2) }, blobs: {}, layout: "x", count: 4 };
    c.host.answer = value;
    c.port.panel({ type: "call", id: 1, method: "columns", params: {} });
    await settle();
    const sent = c.port.sent.find((s) => s.message.type === "result")!;
    expect(sent.message.value).toBe(value);
    expect(sent.transfer).toHaveLength(2);
    expect(sent.transfer).toContain(buf);
    expect(transferable({ x: 1 }).transfer).toEqual([]);
  });
});

describe("limits", () => {
  it("refuses a message over 1 MiB", async () => {
    const c = connected();
    c.port.panel({ type: "call", id: 1, method: "find", params: { blob: "x".repeat(MAX_MESSAGE) } });
    await settle();
    expect(c.host.calls).toEqual([]);
    expect(c.port.of("error")).toMatchObject([{ id: 1, code: PLUGIN_LIMIT }]);
  });

  it("runs at most eight calls at once", async () => {
    const c = connected();
    c.host.answer = () => new Promise(() => {});
    for (let i = 0; i < MAX_CALLS + 2; i++) c.port.panel({ type: "call", id: i, method: "find", params: {} });
    await settle();
    expect(c.host.calls).toHaveLength(MAX_CALLS);
    expect(c.port.of("error").map((e) => e.code)).toEqual([PLUGIN_LIMIT, PLUGIN_LIMIT]);
  });

  it("takes at most 50 messages a second", async () => {
    const c = connected();
    c.host.clock = 1000;
    for (let i = 0; i < MAX_RATE + 5; i++) c.port.panel({ type: "host", id: i, op: "toast", args: { text: String(i) } });
    await settle();
    // the ready message counted at time 0, a second earlier
    expect(c.host.hostOps).toHaveLength(MAX_RATE);
    expect(c.port.of("error")).toHaveLength(5);
    c.host.clock = 2500;
    c.port.panel({ type: "host", id: 99, op: "toast", args: { text: "late" } });
    await settle();
    expect(c.host.hostOps).toHaveLength(MAX_RATE + 1);
  });
});

describe("events, states, streams", () => {
  it("follows the plugin's own events and states by local name", () => {
    const c = connected();
    c.port.panel({ type: "on", name: "progress" });
    c.port.panel({ type: "state", name: "status" });
    c.port.panel({ type: "on", name: "other.plugin.x" });
    expect([...c.host.events.keys()]).toEqual(["plugin.example.panel.progress"]);
    expect([...c.host.topics.keys()]).toEqual(["plugin.example.panel.status"]);
    c.host.events.get("plugin.example.panel.progress")!({ done: 1 });
    c.host.topics.get("plugin.example.panel.status")!({ busy: true });
    expect(c.port.of("event")).toEqual([{ type: "event", name: "progress", payload: { done: 1 } }]);
    expect(c.port.of("state")).toEqual([{ type: "state", name: "status", value: { busy: true } }]);
    c.port.panel({ type: "off", name: "progress" });
    expect(c.host.events.size).toBe(0);
  });

  it("subscribes to its streams and stops on unsub or close", () => {
    const c = connected();
    c.port.panel({ type: "sub", id: 3, topic: "ticks", params: { n: 1 } });
    expect([...c.host.topics.keys()]).toEqual(["plugin.example.panel.ticks"]);
    c.host.topics.get("plugin.example.panel.ticks")!({ i: 1 });
    expect(c.port.of("item")).toEqual([{ type: "item", id: 3, value: { i: 1 } }]);
    c.port.panel({ type: "unsub", id: 3 });
    expect(c.host.topics.size).toBe(0);
    c.port.panel({ type: "sub", id: 4, topic: "ticks", params: null });
    c.bridge.close();
    expect(c.host.topics.size).toBe(0);
  });
});

describe("host operations", () => {
  it("allows the ones without a permission, the others with theirs", async () => {
    const c = connected(["clipboard"]);
    c.port.panel({ type: "host", id: 1, op: "select", args: { node: 5 } });
    c.port.panel({ type: "host", id: 2, op: "copy", args: { text: "x" } });
    c.port.panel({ type: "host", id: 3, op: "reveal", args: { node: 5 } });
    c.port.panel({ type: "host", id: 4, op: "basket", args: { nodes: [5] } });
    c.port.panel({ type: "host", id: 5, op: "format_disk", args: {} });
    await settle();
    expect(c.host.hostOps.map((x) => x.op)).toEqual(["select", "copy"]);
    const codes = Object.fromEntries(c.port.of("error").map((e) => [e.id, e.code]));
    expect(codes).toEqual({ 3: PERMISSION_DENIED, 4: PERMISSION_DENIED, 5: -32601 });
  });

  it("runs only its own plugin's commands", async () => {
    const c = connected();
    c.port.panel({ type: "host", id: 1, op: "command", args: { command: "example.panel.refresh" } });
    c.port.panel({ type: "host", id: 2, op: "command", args: { command: "basket.open" } });
    await settle();
    expect(c.host.hostOps).toEqual([{ op: "command", args: { command: "example.panel.refresh" } }]);
    expect(c.port.of("error")).toMatchObject([{ id: 2, code: PERMISSION_DENIED }]);
  });

  it("resizes within 40–800 px", () => {
    const c = connected();
    c.port.panel({ type: "resize", height: 5000 });
    c.port.panel({ type: "resize", height: 10 });
    c.port.panel({ type: "resize", height: "x" });
    expect(c.resized).toEqual([800, 40]);
  });
});

/** A file of the shipped SDK, read from disk (vitest runs under node; the renderer has no node types). */
async function sdkFile(name: string): Promise<string> {
  const fs = (await import(/* @vite-ignore */ ["node", "fs"].join(":"))) as { readFileSync(path: URL, enc: "utf8"): string };
  return fs.readFileSync(new URL(`../../../../../resources/plugin-sdk/${name}`, import.meta.url), "utf8");
}

describe("the panel SDK", () => {
  it("base.css starts with the theme the bridge sends, dark and light", async () => {
    const css = await sdkFile("base.css");
    const at = css.indexOf(':root[data-sk-theme="light"]');
    expect(at).toBeGreaterThan(0);
    for (const [theme, block] of [["dark", css.slice(0, at)], ["light", css.slice(at)]] as const) {
      for (const [name, value] of Object.entries(themeVars(theme))) {
        if (theme === "light" && (name === "--sk-font" || name === "--sk-mono")) continue; // the same in both
        const m = new RegExp(`${name}:\\s*([^;]+);`).exec(block);
        expect(m?.[1]?.trim(), `${theme} ${name}`).toBe(value);
      }
    }
  });

  it("bridge.js speaks the messages this bridge reads", () => {
    const js = bridgeJs;
    for (const type of ["ready", "call", "cancel", "on", "off", "state", "sub", "unsub", "host", "resize"]) expect(js).toContain(`type: "${type}"`);
    expect(js).toContain('d.type !== "connect"');
    expect(js).toContain('type: "hello"');
  });
});

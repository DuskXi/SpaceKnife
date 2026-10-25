// The host side of a web panel's message bridge (docs/plugins.md 「Web 面板」). A panel is a
// sandboxed iframe (an opaque origin: no tokens, storage or cookies of the app, no way to the
// backend); everything it does goes through one MessagePort, and this class decides:
//
// - calls: its own plugin's methods by local name (plugin.<id>.<name>), or core:<method> for the few
//   built-in methods a plugin may use, each with the permission it needs (the same list as plugin
//   code, plugins/allow.py); basket.add always carries the panel as its origin;
// - events, shared states and streams of its own plugin only;
// - host operations (select, zoom, filter, its own commands and its view's, toast; the view's colours
//   with tree.read; reveal / open / terminal / url with host.open; copy with clipboard; basket with
//   basket.propose; a comparison of snapshots with snapshots.read);
// - limits: a message ≤ 1 MiB, at most 8 calls at once and 50 messages a second (PLUGIN_LIMIT).
//
// The plugin is bound to the iframe by the host (PanelFrame), never taken from a message. Pure: the
// tests drive it with a fake port and a fake host (bridge.test.ts, testing/mockBridge.ts).

export const PLUGIN_LIMIT = -32020;
export const PERMISSION_DENIED = -32014;
export const INVALID_PARAMS = -32602;
export const METHOD_NOT_FOUND = -32601;
export const CANCELLED = -32800;

/** A message at most this long (JSON text), the calls one panel may run at once, its message rate. */
export const MAX_MESSAGE = 1 << 20;
export const MAX_CALLS = 8;
export const MAX_RATE = 50;

/** Built-in methods a panel may call (core:<method>) and the permission each needs. */
export const CORE_METHODS: Readonly<Record<string, string | null>> = {
  "tree.node": "tree.read",
  "tree.children": "tree.read",
  "tree.summary": "tree.read",
  "tree.find": "tree.read",
  "scan.list": null,
  "scan.status": null,
  "volume.list": null,
  "view.list": null,
  "visual.list": null,
  "filter.parse": null,
  "filter.vocab": null,
  "classes.get": null,
  "export.targets": null,
  "snapshot.list": "snapshots.read",
  "compare.info": "tree.read",
  "compare.summary": "tree.read",
  "compare.node": "tree.read",
  "basket.list": null,
  "basket.add": "basket.propose",
  "insight.node": "tree.read",
  "insight.nodes": "tree.read",
};

/** Host operations and the permission each needs. */
export const HOST_OPS: Readonly<Record<string, string | null>> = {
  select: null,
  zoom: null,
  filter: null,
  command: null,
  toast: null,
  reveal: "host.open",
  open: "host.open",
  terminal: "host.open",
  url: "host.open",
  copy: "clipboard",
  basket: "basket.propose",
  // the colours the panel's view paints nodes with (classes, ages: what tree.read shows anyway)
  colors: "tree.read",
  // show a comparison of snapshots (or of one and the live tree)
  compare: "snapshots.read",
};

const LOCAL_NAME = /^[a-z][a-z0-9_]{0,23}$/;

/** The port the bridge talks over (a MessagePort, or a fake in the tests). */
export interface PortLike {
  postMessage(message: unknown, transfer: Transferable[]): void;
  onmessage: ((ev: MessageEvent) => void) | null;
  close(): void;
}

/** What the bridge may do in the app: the panel's plugin and permissions, and the calls behind it. */
export interface BridgeHost {
  /** the panel's plugin id (bound to the iframe) */
  plugin: string;
  /** the panel's full id: the origin of what it puts in the basket */
  panel: string;
  permissions: readonly string[];
  /** a backend method by its wire name; columns come back as { columns: {name: TypedArray}, … } */
  call(method: string, params: unknown, signal: AbortSignal): Promise<unknown>;
  /** a plugin event by its wire name */
  onEvent(wire: string, cb: (payload: unknown) => void): () => void;
  /** $/subscribe to a topic (a state or a stream) */
  subscribe(topic: string, params: Record<string, unknown> | null, cb: (value: unknown) => void): () => void;
  /** a host operation, permission already checked */
  host(op: string, args: Record<string, unknown>): Promise<unknown> | unknown;
  /** milliseconds, for the rate limit */
  now(): number;
}

export interface BridgeOptions {
  /** the address prefix of the panel's own files: a document elsewhere is refused (bridge.js says where it is) */
  base: string;
  onResize?(height: number): void;
  /** the panel turned out to be somewhere else than its plugin's files */
  onRefused?(href: string): void;
  /** the panel said it is ready (its first context, theme and language go out then) */
  onReady?(): void;
}

type Msg = { type?: unknown; id?: unknown; [k: string]: unknown };

/** The app's commands a panel may run (sk.host.command): what the view shows, where it is, the
 * filter box and the export dialog; never the basket, operations, settings or the app. */
const PANEL_COMMANDS = /^(?:view\.(?:visual|color)\.[\w.-]+|view\.(?:cycle_color|cycle_visual|detail_down|detail_up|zoom_in|zoom_out|toggle_free|toggle_trash|toggle_unknown)|nav\.(?:back|forward|home|up)|filter\.focus|report\.export)$/;

export function panelMayRun(command: string, plugin: string): boolean {
  return command.startsWith(`${plugin}.`) || PANEL_COMMANDS.test(command);
}

export class PanelBridge {
  private calls = new Map<number, AbortController>();
  private subs = new Map<number, () => void>();
  private events = new Map<string, () => void>();
  private states = new Map<string, () => void>();
  private stamps: number[] = [];
  private closed = false;
  ready = false;

  constructor(
    private host: BridgeHost,
    private port: PortLike,
    private opts: BridgeOptions,
  ) {
    port.onmessage = (ev) => this.receive(ev.data);
  }

  /** Stop: every call cancelled, every subscription dropped, the port closed. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const ac of this.calls.values()) ac.abort();
    for (const off of [...this.subs.values(), ...this.events.values(), ...this.states.values()]) off();
    this.calls.clear();
    this.subs.clear();
    this.events.clear();
    this.states.clear();
    this.port.onmessage = null;
    this.port.close();
  }

  /** Something for the panel (context, theme, language, its view): only once it said it is ready. */
  push(type: "context" | "theme" | "lang" | "tree" | "view" | "snapshots", payload: Record<string, unknown>): void {
    if (this.ready && !this.closed) this.post({ type, ...payload });
  }

  private post(msg: Record<string, unknown>, transfer: Transferable[] = []): void {
    if (this.closed) return;
    try {
      this.port.postMessage(msg, transfer);
    } catch {
      // a value that can't be cloned: say so instead
      if (typeof msg.id === "number") this.port.postMessage({ type: "error", id: msg.id, code: INVALID_PARAMS, message: "result could not be sent" }, []);
    }
  }

  private fail(id: unknown, code: number, message: string): void {
    if (typeof id === "number") this.post({ type: "error", id, code, message });
  }

  /** Over the rate: the last second held MAX_RATE messages already. */
  private limited(): boolean {
    const now = this.host.now();
    while (this.stamps.length && now - this.stamps[0]! >= 1000) this.stamps.shift();
    if (this.stamps.length >= MAX_RATE) return true;
    this.stamps.push(now);
    return false;
  }

  private allowed(permission: string | null | undefined): boolean {
    return !permission || this.host.permissions.includes(permission);
  }

  /** One message from the panel. */
  receive(data: unknown): void {
    if (this.closed || !data || typeof data !== "object") return;
    const msg = data as Msg;
    let size = 0;
    try {
      size = JSON.stringify(msg).length;
    } catch {
      return this.fail(msg.id, INVALID_PARAMS, "message could not be read");
    }
    if (size > MAX_MESSAGE) return this.fail(msg.id, PLUGIN_LIMIT, "message too large");
    if (this.limited()) return this.fail(msg.id, PLUGIN_LIMIT, "too many messages");
    if (msg.type !== "ready" && !this.ready) return this.fail(msg.id, INVALID_PARAMS, "not ready");
    switch (msg.type) {
      case "ready":
        return this.onReady(msg);
      case "call":
        return void this.onCall(msg);
      case "cancel":
        if (typeof msg.id === "number") this.calls.get(msg.id)?.abort();
        return;
      case "on":
        return this.onOn(msg);
      case "off":
        return this.onOff(msg);
      case "state":
        return this.onState(msg);
      case "sub":
        return this.onSub(msg);
      case "unsub":
        if (typeof msg.id === "number") {
          this.subs.get(msg.id)?.();
          this.subs.delete(msg.id);
        }
        return;
      case "host":
        return void this.onHost(msg);
      case "resize": {
        const h = Number(msg.height);
        if (Number.isFinite(h)) this.opts.onResize?.(Math.max(40, Math.min(800, Math.round(h))));
        return;
      }
      default:
        return this.fail(msg.id, METHOD_NOT_FOUND, `unknown message: ${String(msg.type)}`);
    }
  }

  private onReady(msg: Msg): void {
    const href = typeof msg.href === "string" ? msg.href : "";
    // the document in the frame must be one of the plugin's own: a panel that navigated elsewhere
    // (another plugin's panel) does not get this one's bridge
    if (!href.startsWith(this.opts.base)) {
      this.opts.onRefused?.(href);
      this.close();
      return;
    }
    this.ready = true;
    this.opts.onReady?.();
  }

  /** The wire name of a call: the plugin's own method, or an allowed core method; else why not. */
  route(method: unknown): { wire: string; core: boolean } | { code: number; message: string } {
    if (typeof method !== "string") return { code: INVALID_PARAMS, message: "method must be a string" };
    if (method.startsWith("core:")) {
      const name = method.slice(5);
      if (!Object.hasOwn(CORE_METHODS, name)) return { code: PERMISSION_DENIED, message: `not allowed for panels: ${name}` };
      if (!this.allowed(CORE_METHODS[name])) return { code: PERMISSION_DENIED, message: `needs the ${CORE_METHODS[name]} permission` };
      return { wire: name, core: true };
    }
    if (!LOCAL_NAME.test(method)) return { code: PERMISSION_DENIED, message: `not a method of this plugin: ${method}` };
    return { wire: `plugin.${this.host.plugin}.${method}`, core: false };
  }

  private async onCall(msg: Msg): Promise<void> {
    const id = msg.id;
    if (typeof id !== "number") return;
    if (this.calls.size >= MAX_CALLS) return this.fail(id, PLUGIN_LIMIT, "too many calls at once");
    const r = this.route(msg.method);
    if ("code" in r) return this.fail(id, r.code, r.message);
    let params = msg.params && typeof msg.params === "object" ? (msg.params as Record<string, unknown>) : {};
    // what a panel puts in the basket is marked as the panel's (shown in the basket), always
    if (r.core && r.wire === "basket.add") params = { scan_id: params.scan_id, nodes: params.nodes, reasons: params.reasons ?? null, origin: this.host.panel };
    const ac = new AbortController();
    this.calls.set(id, ac);
    try {
      const value = await this.host.call(r.wire, params, ac.signal);
      if (ac.signal.aborted) return this.fail(id, CANCELLED, "cancelled");
      const { value: out, transfer } = transferable(value);
      this.post({ type: "result", id, value: out }, transfer);
    } catch (err) {
      const e = err as { code?: unknown; message?: unknown };
      this.fail(id, typeof e?.code === "number" ? e.code : -32603, typeof e?.message === "string" ? e.message : String(err));
    } finally {
      this.calls.delete(id);
    }
  }

  private onOn(msg: Msg): void {
    const name = msg.name;
    if (typeof name !== "string" || !LOCAL_NAME.test(name)) return this.fail(msg.id, INVALID_PARAMS, "bad event name");
    if (this.events.has(name)) return;
    this.events.set(
      name,
      this.host.onEvent(`plugin.${this.host.plugin}.${name}`, (payload) => this.post({ type: "event", name, payload })),
    );
  }

  private onOff(msg: Msg): void {
    const name = msg.name;
    if (typeof name !== "string") return;
    this.events.get(name)?.();
    this.events.delete(name);
  }

  private onState(msg: Msg): void {
    const name = msg.name;
    if (typeof name !== "string" || !LOCAL_NAME.test(name)) return this.fail(msg.id, INVALID_PARAMS, "bad state name");
    if (this.states.has(name)) return;
    this.states.set(
      name,
      this.host.subscribe(`plugin.${this.host.plugin}.${name}`, null, (value) => this.post({ type: "state", name, value })),
    );
  }

  private onSub(msg: Msg): void {
    const id = msg.id;
    const topic = msg.topic;
    if (typeof id !== "number") return;
    if (typeof topic !== "string" || !LOCAL_NAME.test(topic)) return this.fail(id, INVALID_PARAMS, "bad topic");
    if (this.subs.size >= 16) return this.fail(id, PLUGIN_LIMIT, "too many subscriptions");
    const params = msg.params && typeof msg.params === "object" ? (msg.params as Record<string, unknown>) : null;
    this.subs.get(id)?.();
    this.subs.set(
      id,
      this.host.subscribe(`plugin.${this.host.plugin}.${topic}`, params, (value) => {
        const { value: out, transfer } = transferable(value);
        this.post({ type: "item", id, value: out }, transfer);
      }),
    );
  }

  private async onHost(msg: Msg): Promise<void> {
    const id = msg.id;
    const op = msg.op;
    if (typeof op !== "string" || !Object.hasOwn(HOST_OPS, op)) return this.fail(id, METHOD_NOT_FOUND, `unknown host operation: ${String(op)}`);
    if (!this.allowed(HOST_OPS[op])) return this.fail(id, PERMISSION_DENIED, `needs the ${HOST_OPS[op]} permission`);
    const args = msg.args && typeof msg.args === "object" ? (msg.args as Record<string, unknown>) : {};
    // its own commands, and the app's that only change what the view shows (a key pack's, the
    // basket's, the settings' are not a panel's to run)
    if (op === "command" && (typeof args.command !== "string" || !panelMayRun(args.command, this.host.plugin))) {
      return this.fail(id, PERMISSION_DENIED, "a panel runs its own plugin's commands and the app's view, filter and navigation ones");
    }
    try {
      const value = await this.host.host(op, args);
      if (typeof id === "number") this.post({ type: "result", id, value: value ?? null });
    } catch (err) {
      this.fail(id, -32603, err instanceof Error ? err.message : String(err));
    }
  }
}

/** A value with typed-array columns, and the buffers to hand over without copying (each once). */
export function transferable(value: unknown): { value: unknown; transfer: Transferable[] } {
  if (!value || typeof value !== "object" || !("columns" in value)) return { value, transfer: [] };
  const cols = (value as { columns: unknown }).columns;
  if (!cols || typeof cols !== "object") return { value, transfer: [] };
  const buffers = new Set<ArrayBuffer>();
  for (const c of Object.values(cols as Record<string, unknown>)) {
    if (ArrayBuffer.isView(c) && c.buffer instanceof ArrayBuffer) buffers.add(c.buffer);
  }
  return { value, transfer: [...buffers] };
}

/** The first message of a panel: from that iframe's window, saying hello. */
export function isHello(ev: { source: unknown; data: unknown }, frameWindow: unknown): boolean {
  if (!frameWindow || ev.source !== frameWindow) return false;
  const d = ev.data as { sk?: unknown; type?: unknown } | null;
  return !!d && typeof d === "object" && d.sk === 1 && d.type === "hello";
}

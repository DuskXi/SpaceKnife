// Calling plugins from the renderer (docs/plugins.md, 「统一通讯层」): a plugin's methods, events,
// shared states and streams by their wire names (plugin.<id>.<name>), over the session the rest of
// the app uses. Nothing here knows a plugin's types: pluginClient<Api>(id) gives them to callers
// that have them (the generated sk-plugin.d.ts of a plugin), callPlugin works without.
//
// Results with columns: a plugin method returning numpy arrays (Columns) sends them as binary frames
// stamped with the request's id before its JSON answer {columns: true}; they come back here as typed
// arrays (zero copy). Streams send their frames first, then a notification with value null.

import type { BinaryFrame, Column, Rpc } from "@/protocol/rpc";
import { RpcError } from "@/protocol/rpc";
import { t } from "@/lib/i18n";

import { isPluginName } from "./caps";

/** What a plugin method answered: its JSON value, or its columns. */
export interface PluginColumns {
  columns: Record<string, Column>;
  blobs: Record<string, Uint8Array>;
  layout: string;
  count: number;
}

export const isColumns = (v: unknown): v is PluginColumns => !!v && typeof v === "object" && "columns" in v && typeof (v as PluginColumns).columns === "object";

let rpc: Rpc | null = null;

/** The session's connection (state/actions.ts installPlugins); the subscriptions are made again on it. */
export function attachPlugins(r: Rpc): void {
  rpc = r;
  for (const l of listeners.values()) l.attach(r);
  for (const s of subs) void s.open(r);
  r.onBinary(onStreamFrame);
}

/** A connection that is gone: calls fail until the next one. */
export function detachPlugins(r: Rpc): void {
  if (rpc === r) rpc = null;
}

function live(): Rpc {
  if (!rpc || !rpc.open) throw new RpcError(-32006, t("后端未连接"));
  return rpc;
}

/** The wire name of a plugin's method, event, state or stream. */
export const wireName = (plugin: string, name: string): string => `plugin.${plugin}.${name}`;

/** The plugin id in a wire name ("plugin.example.dupes.find" → "example.dupes"). */
export function pluginOf(wire: string): string | null {
  if (!isPluginName(wire)) return null;
  const rest = wire.slice("plugin.".length);
  return rest.slice(0, rest.lastIndexOf("."));
}

function columnsOf(value: unknown, frames: BinaryFrame[]): unknown {
  if (value && typeof value === "object" && (value as { columns?: unknown }).columns === true && frames.length) {
    const f = frames[frames.length - 1]!;
    return { columns: f.columns, blobs: f.blobs, layout: f.header.layout, count: f.header.count } satisfies PluginColumns;
  }
  return value;
}

/** Call plugin.<id>.<name>; an aborted signal cancels it on the backend ($/cancel) and rejects with
 * code -32800. Never throws synchronously. */
export async function callPlugin(method: string, params: unknown = {}, opts: { signal?: AbortSignal; onId?: (id: number) => void } = {}): Promise<unknown> {
  const r = live();
  const { value, frames } = await r.callFrames(method, params ?? {}, opts);
  return columnsOf(value, frames);
}

// ------------------------------------------------------------------ events

interface Listener {
  name: string;
  cbs: Set<(payload: unknown) => void>;
  off: (() => void) | null;
  attach(r: Rpc): void;
}

const listeners = new Map<string, Listener>();

/** A plugin's event (plugin.<id>.<name>), across reconnects; returns the unsubscribe. */
export function onPluginEvent(name: string, cb: (payload: unknown) => void): () => void {
  let l = listeners.get(name);
  if (!l) {
    const entry: Listener = {
      name,
      cbs: new Set(),
      off: null,
      attach(r) {
        entry.off?.();
        entry.off = r.onRaw(name, (p) => {
          for (const f of entry.cbs) f(p);
        });
      },
    };
    l = entry;
    listeners.set(name, entry);
    if (rpc) entry.attach(rpc);
  }
  l.cbs.add(cb);
  return () => {
    l.cbs.delete(cb);
    if (!l.cbs.size) {
      l.off?.();
      listeners.delete(name);
    }
  };
}

// ------------------------------------------------------------------ states and streams

interface Sub {
  topic: string;
  params: Record<string, unknown> | null;
  cb: (value: unknown) => void;
  id: string | null;
  off: (() => void) | null;
  closed: boolean;
  open(r: Rpc): Promise<void>;
}

const subs = new Set<Sub>();
/** a stream's binary frames, until the notification that follows them */
const streamFrames = new Map<string, BinaryFrame[]>();

function onStreamFrame(f: BinaryFrame): void {
  const name = f.header.stream;
  if (!isPluginName(name) || f.header.request_id != null) return;
  const list = streamFrames.get(name) ?? [];
  list.push(f);
  if (list.length > 8) list.shift();
  streamFrames.set(name, list);
}

/** A plugin's shared state or stream ($/subscribe {topic: plugin.<id>.<name>}): `cb` gets the current
 * value first (a state), then every change; a stream's columns come as PluginColumns. Made again
 * after a reconnect; returns the unsubscribe. */
export function subscribePlugin(topic: string, params: Record<string, unknown> | null, cb: (value: unknown) => void): () => void {
  const sub: Sub = {
    topic,
    params,
    cb,
    id: null,
    off: null,
    closed: false,
    async open(r) {
      sub.off?.();
      sub.id = null;
      // a state's first value comes before the subscribe answer: take values for this topic until the
      // id is known, then only that subscription's
      sub.off = r.onRaw(topic, (p) => {
        const msg = p as { sub?: string; value?: unknown } | null;
        if (!msg || sub.closed || (sub.id && msg.sub && msg.sub !== sub.id)) return;
        if (msg.value == null) {
          const frames = streamFrames.get(topic);
          if (frames?.length) {
            streamFrames.delete(topic);
            const f = frames[frames.length - 1]!;
            cb({ columns: f.columns, blobs: f.blobs, layout: f.header.layout, count: f.header.count } satisfies PluginColumns);
          }
          return;
        }
        cb(msg.value);
      });
      try {
        const res = await r.call("$/subscribe", { topic, params: params ?? null });
        sub.id = res.sub;
        if (sub.closed) void r.call("$/unsubscribe", { sub: res.sub }).catch(() => undefined);
      } catch {
        // the plugin is off or gone: no values until it comes back (extensions.changed re-renders)
      }
    },
  };
  subs.add(sub);
  if (rpc?.open) void sub.open(rpc);
  return () => {
    sub.closed = true;
    subs.delete(sub);
    sub.off?.();
    if (sub.id && rpc?.open) void rpc.call("$/unsubscribe", { sub: sub.id }).catch(() => undefined);
  };
}

// ------------------------------------------------------------------ typed clients

/** The shape of a plugin's API (generated per plugin, docs/plugins.md 「生成的类型」). */
export interface PluginApi {
  methods: Record<string, { params: unknown; result: unknown }>;
  events?: Record<string, unknown>;
  states?: Record<string, unknown>;
}

export type PluginClient<A extends PluginApi> = {
  [M in keyof A["methods"]]: (params: A["methods"][M]["params"], opts?: { signal?: AbortSignal }) => Promise<A["methods"][M]["result"]>;
} & {
  on<E extends keyof NonNullable<A["events"]> & string>(event: E, cb: (payload: NonNullable<A["events"]>[E]) => void): () => void;
  state<S extends keyof NonNullable<A["states"]> & string>(name: S, cb: (value: NonNullable<A["states"]>[S]) => void): () => void;
};

/** `pluginClient<DupesApi>("example.dupes").find({min_size: 1})` → plugin.example.dupes.find. */
export function pluginClient<A extends PluginApi>(plugin: string): PluginClient<A> {
  const on = (event: string, cb: (p: unknown) => void) => onPluginEvent(wireName(plugin, event), cb);
  const state = (name: string, cb: (v: unknown) => void) => subscribePlugin(wireName(plugin, name), null, cb);
  return new Proxy({} as PluginClient<A>, {
    get(_target, prop) {
      if (prop === "on") return on;
      if (prop === "state") return state;
      if (typeof prop !== "string" || prop === "then") return undefined;
      return (params: unknown, opts?: { signal?: AbortSignal }) => callPlugin(wireName(plugin, prop), params, opts);
    },
  });
}

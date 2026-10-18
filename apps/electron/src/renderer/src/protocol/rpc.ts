// Typed client for protocol v1 (docs/protocol.md): handshake, requests, events, binary frames.

import {
  type EventName,
  type Events,
  type HelloResult,
  type MethodName,
  type Methods,
  PROTOCOL_VERSION,
  SUBPROTOCOL,
} from "./generated";
import { t } from "@/lib/i18n";

export class RpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
    readonly data?: unknown,
  ) {
    super(message);
  }
}

export interface BinaryHeader {
  stream: string;
  request_id?: number | string | null;
  seq: number;
  final: boolean;
  layout: string;
  count: number;
  columns: Record<string, { offset: number; dtype: string }>;
  blobs?: Record<string, { offset: number; length: number }>;
}

export type Column =
  | Uint8Array
  | Int8Array
  | Uint16Array
  | Int16Array
  | Uint32Array
  | Int32Array
  | BigUint64Array
  | BigInt64Array
  | Float32Array
  | Float64Array;

export interface BinaryFrame {
  header: BinaryHeader;
  columns: Record<string, Column>;
  blobs: Record<string, Uint8Array>;
}

const DTYPES: Record<string, new (buf: ArrayBuffer, off: number, len: number) => Column> = {
  "<u1": Uint8Array,
  "<i1": Int8Array,
  "<u2": Uint16Array,
  "<i2": Int16Array,
  "<u4": Uint32Array,
  "<i4": Int32Array,
  "<u8": BigUint64Array,
  "<i8": BigInt64Array,
  "<f4": Float32Array,
  "<f8": Float64Array,
};

/** Zero-copy decode of one binary frame (typed arrays view the received buffer). */
export function decodeFrame(buf: ArrayBuffer): BinaryFrame {
  const view = new DataView(buf);
  const hlen = view.getUint32(0, true);
  const header = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 4, hlen))) as BinaryHeader;
  let start = 4 + hlen;
  start += (8 - (start % 8)) % 8;
  const columns: Record<string, Column> = {};
  for (const [name, spec] of Object.entries(header.columns)) {
    const Ctor = DTYPES[spec.dtype];
    if (!Ctor) throw new Error(`unsupported dtype ${spec.dtype}`);
    columns[name] = new Ctor(buf, start + spec.offset, header.count);
  }
  const blobs: Record<string, Uint8Array> = {};
  for (const [name, spec] of Object.entries(header.blobs ?? {})) {
    blobs[name] = new Uint8Array(buf, start + spec.offset, spec.length);
  }
  return { header, columns, blobs };
}

type Pending = { resolve: (v: unknown) => void; reject: (e: unknown) => void; method: string };

export interface ClientInfo {
  name: string;
  version: string;
}

/** One WebSocket session. Reconnection is the owner's job (see state/connection.ts). */
export class Rpc {
  private ws: WebSocket | null = null;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private handlers = new Map<string, Set<(params: unknown) => void>>();
  private binaryHandlers = new Set<(frame: BinaryFrame) => void>();
  private closeHandlers = new Set<(ev: CloseEvent) => void>();
  hello: HelloResult | null = null;

  async connect(url: string, token: string, client: ClientInfo): Promise<HelloResult> {
    const ws = new WebSocket(url, SUBPROTOCOL);
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    ws.onmessage = (ev) => this.onMessage(ev);
    ws.onclose = (ev) => {
      for (const p of this.pending.values()) p.reject(new RpcError(-32006, "connection closed"));
      this.pending.clear();
      for (const h of this.closeHandlers) h(ev);
    };
    await new Promise<void>((resolve, reject) => {
      ws.onopen = () => resolve();
      ws.onerror = () => reject(new Error(t("无法连接后端")));
    });
    this.hello = (await this.callRaw("session.hello", {
      token,
      protocol: PROTOCOL_VERSION,
      client: { ...client, kind: "electron" },
      capabilities: ["binary-frames.v1", "subscriptions", "cancel", "log-stream"],
    })) as HelloResult;
    return this.hello;
  }

  close(): void {
    this.ws?.close(1000);
  }

  get open(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  call<M extends MethodName>(method: M, params: Methods[M]["params"]): Promise<Methods[M]["result"]> {
    return this.callRaw(method, params) as Promise<Methods[M]["result"]>;
  }

  /** A call that can be cancelled with `$/cancel` (resolves to null when cancelled). */
  cancellable<M extends MethodName>(
    method: M,
    params: Methods[M]["params"],
  ): { promise: Promise<Methods[M]["result"] | null>; cancel: () => void } {
    const id = this.nextId++;
    const promise = this.send(id, method, params).catch((err: unknown) => {
      if (err instanceof RpcError && err.code === -32800) return null;
      throw err;
    }) as Promise<Methods[M]["result"] | null>;
    return { promise, cancel: () => this.notify("$/cancel", { id }) };
  }

  private callRaw(method: string, params: unknown): Promise<unknown> {
    return this.send(this.nextId++, method, params);
  }

  private send(id: number, method: string, params: unknown): Promise<unknown> {
    return new Promise((resolve, reject) => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        reject(new RpcError(-32006, "not connected"));
        return;
      }
      this.pending.set(id, { resolve, reject, method });
      this.ws.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }

  notify(method: string, params: unknown): void {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }

  on<E extends EventName>(event: E, handler: (params: Events[E]) => void): () => void {
    let set = this.handlers.get(event);
    if (!set) this.handlers.set(event, (set = new Set()));
    set.add(handler as (p: unknown) => void);
    return () => set!.delete(handler as (p: unknown) => void);
  }

  onBinary(handler: (frame: BinaryFrame) => void): () => void {
    this.binaryHandlers.add(handler);
    return () => this.binaryHandlers.delete(handler);
  }

  onClose(handler: (ev: CloseEvent) => void): () => void {
    this.closeHandlers.add(handler);
    return () => this.closeHandlers.delete(handler);
  }

  private onMessage(ev: MessageEvent): void {
    if (typeof ev.data !== "string") {
      const frame = decodeFrame(ev.data as ArrayBuffer);
      for (const h of this.binaryHandlers) h(frame);
      return;
    }
    const msg = JSON.parse(ev.data) as {
      id?: number;
      result?: unknown;
      error?: { code: number; message: string; data?: unknown };
      method?: string;
      params?: unknown;
    };
    if (msg.id !== undefined && (msg.result !== undefined || msg.error !== undefined)) {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.error) p.reject(new RpcError(msg.error.code, msg.error.message, msg.error.data));
      else p.resolve(msg.result);
    } else if (msg.method) {
      for (const h of this.handlers.get(msg.method) ?? []) h(msg.params);
    }
  }
}

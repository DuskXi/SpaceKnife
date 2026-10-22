// A fake host and port for the web panel bridge (bridge.test.ts): records what the bridge sends,
// what it asks the app for, and lets a test play the panel.

import type { BridgeHost, PortLike } from "../panel/bridge";

export interface Sent {
  message: Record<string, unknown>;
  transfer: Transferable[];
}

export class FakePort implements PortLike {
  sent: Sent[] = [];
  closed = false;
  onmessage: ((ev: MessageEvent) => void) | null = null;

  postMessage(message: unknown, transfer: Transferable[]): void {
    this.sent.push({ message: message as Record<string, unknown>, transfer });
  }

  close(): void {
    this.closed = true;
  }

  /** The panel sends a message. */
  panel(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }

  /** What the bridge sent, of one type. */
  of(type: string): Record<string, unknown>[] {
    return this.sent.map((s) => s.message).filter((m) => m.type === type);
  }
}

export interface MockHost extends BridgeHost {
  calls: { method: string; params: unknown; signal: AbortSignal }[];
  hostOps: { op: string; args: Record<string, unknown> }[];
  events: Map<string, (payload: unknown) => void>;
  topics: Map<string, (value: unknown) => void>;
  /** the answer of the next calls (a function gets the method and params) */
  answer: unknown;
  clock: number;
}

export function mockHost(over: Partial<BridgeHost> = {}): MockHost {
  const host: MockHost = {
    plugin: "example.panel",
    panel: "example.panel.board",
    permissions: [],
    calls: [],
    hostOps: [],
    events: new Map(),
    topics: new Map(),
    answer: { ok: true },
    clock: 0,
    async call(method, params, signal) {
      host.calls.push({ method, params, signal });
      const a = host.answer;
      return typeof a === "function" ? (a as (m: string, p: unknown, s: AbortSignal) => unknown)(method, params, signal) : a;
    },
    onEvent(wire, cb) {
      host.events.set(wire, cb);
      return () => host.events.delete(wire);
    },
    subscribe(topic, _params, cb) {
      host.topics.set(topic, cb);
      return () => host.topics.delete(topic);
    },
    host(op, args) {
      host.hostOps.push({ op, args });
      return null;
    },
    now: () => host.clock,
    ...over,
  };
  return host;
}

/** Let the bridge's promises settle. */
export const settle = () => new Promise<void>((r) => setTimeout(r, 0));

// @vitest-environment happy-dom
// basket.execute answered BASKET_CHANGED (a confirmed item changed since, nothing ran) before the
// basket.changed event of that change reached this window: answers go out before events. The
// confirmation is asked again with the basket the answer carries, not with this window's copy,
// which would name the items as they were and be refused again.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BasketItem, BasketState } from "@/protocol/generated";
import type { SkApi } from "@/shared/api";

const wire = vi.hoisted(() => ({
  calls: [] as { method: string; params: unknown }[],
  answer: new Map<string, (params: unknown) => unknown>(),
}));

// the connection: answers what boot() asks, and basket.execute as the test says
vi.mock("@/protocol/rpc", async (original) => {
  const real = await original<typeof import("@/protocol/rpc")>();
  class FakeRpc {
    open = true;
    hello = { server: { version: "0.3.5" }, capabilities: ["basket-confirm.v1"] };
    async connect() {
      return this.hello;
    }
    on() {}
    onBinary() {}
    onClose() {}
    async call(method: string, params: unknown) {
      wire.calls.push({ method, params });
      const answer = wire.answer.get(method);
      if (!answer) throw new real.RpcError(-32601, `no ${method} here`);
      return answer(params);
    }
    cancellable(method: string, params: unknown) {
      return { promise: this.call(method, params), cancel: () => {} };
    }
  }
  return { ...real, Rpc: FakeRpc };
});

import { RpcError } from "@/protocol/rpc";

import { basketExecute, boot } from "./actions";
import { get, resetState } from "./store";

const item = (id: string, rev: number, name: string): BasketItem => ({
  id,
  scan_id: "s1",
  path: `C:\\d\\${name}`,
  name,
  kind: "file",
  members: 0,
  alloc: 100,
  size: 100,
  files: 1,
  dirs: 0,
  volume: "C:\\",
  status: "ready",
  note: "",
  tag: null,
  rev,
});
const basket = (...items: BasketItem[]): BasketState => ({ items, volumes: [], freeable: items.reduce((n, i) => n + i.alloc, 0) });

let port = 1000;

describe("BASKET_CHANGED", () => {
  beforeEach(async () => {
    resetState();
    wire.calls.length = 0;
    wire.answer.clear();
    const sk = {
      platform: "win32",
      version: "0.3.5",
      host: "app",
      remote: null,
      justUpdated: false,
      // a backend of its own each time: boot() connects to a new one only
      backend: async () => ({ state: "ready", info: { url: `ws://127.0.0.1:${++port}/`, token: "t" } }),
      onBackend: () => () => {},
      setTheme: () => {},
      setLanguage: () => {},
    } as unknown as SkApi;
    vi.stubGlobal("window", Object.assign(window, { sk }));
    wire.answer.set("filter.vocab", () => ({ classes: [] }));
    wire.answer.set("volume.list", () => ({ volumes: [] }));
    wire.answer.set("snapshot.list", () => ({ snapshots: [] }));
    wire.answer.set("scan.list", () => ({ scans: [] }));
    wire.answer.set("settings.get", () => ({ values: {}, language: "zh-CN", unit_base: 1024, version: 1 }));
    // what this window was told last: the item as it was confirmed
    wire.answer.set("basket.list", () => basket(item("a", 1, "a.tmp"), item("b", 1, "b.tmp")));
    wire.answer.set("$/subscribe", () => ({ sub: "x" }));
    await boot();
    expect(get().conn).toBe("ready");
  });
  afterEach(() => vi.unstubAllGlobals());

  it("asks again with the basket the answer carries", async () => {
    expect(get().basket?.items.map((i) => [i.id, i.rev])).toEqual([
      ["a", 1],
      ["b", 1],
    ]);
    // meanwhile another session took b out and grew a: the answer says so, its event is not here yet
    const now = basket(item("a", 2, "a.tmp"));
    wire.answer.set("basket.execute", () => {
      throw new RpcError(-32014, "删除篮在确认之后有了变化", { state: now });
    });
    const confirmed = get().basket!.items.map((i) => ({ id: i.id, rev: i.rev }));
    expect(await basketExecute("delete", confirmed)).toBe("changed");
    // what the drawer asks again from (BasketDrawer askAgain: the store's basket)
    expect(get().basket).toEqual(now);
    // and the second confirmation names the item as it is now
    let sent: unknown = null;
    wire.answer.set("basket.execute", (params) => {
      sent = params;
      return { task_id: "op-1", mode: "delete", total: 1, delay_s: 5 };
    });
    const again = get().basket!.items.map((i) => ({ id: i.id, rev: i.rev }));
    expect(await basketExecute("delete", again)).toBe("started");
    expect(sent).toEqual({ mode: "delete", confirm: [{ id: "a", rev: 2 }] });
  });

  it("keeps the window's basket when the answer carries none (an older backend)", async () => {
    const before = get().basket;
    wire.answer.set("basket.execute", () => {
      throw new RpcError(-32014, "changed");
    });
    expect(await basketExecute("delete", [{ id: "a", rev: 1 }])).toBe("changed");
    expect(get().basket).toBe(before);
  });
});

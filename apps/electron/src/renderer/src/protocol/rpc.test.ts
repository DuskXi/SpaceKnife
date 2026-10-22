import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { PROTOCOL_VERSION, SUBPROTOCOL } from "./generated";
import { type BinaryFrame, type Column, decodeFrame, Rpc, RpcError } from "./rpc";

// ------------------------------------------------------------------ helpers

const DTYPE = new Map<unknown, string>([
  [Uint8Array, "<u1"],
  [Int8Array, "<i1"],
  [Uint16Array, "<u2"],
  [Int16Array, "<i2"],
  [Uint32Array, "<u4"],
  [Int32Array, "<i4"],
  [BigUint64Array, "<u8"],
  [BigInt64Array, "<i8"],
  [Float32Array, "<f4"],
  [Float64Array, "<f8"],
]);

const pad8 = (n: number) => (8 - (n % 8)) % 8;

/** Same wire layout as spaceknife_protocol.binary.encode_frame (docs/protocol.md §4). */
function encodeFrame(
  header: Record<string, unknown>,
  columns: Record<string, Column>,
  blobs: Record<string, Uint8Array> = {},
  dtypeOverride: Record<string, string> = {},
): ArrayBuffer {
  const chunks: Uint8Array[] = [];
  let offset = 0;
  const put = (bytes: Uint8Array): number => {
    const at = offset;
    chunks.push(bytes, new Uint8Array(pad8(bytes.length)));
    offset += bytes.length + pad8(bytes.length);
    return at;
  };
  let count = 0;
  const colSpecs: Record<string, { offset: number; dtype: string }> = {};
  for (const [name, col] of Object.entries(columns)) {
    count = col.length;
    colSpecs[name] = {
      offset: put(new Uint8Array(col.buffer, col.byteOffset, col.byteLength)),
      dtype: dtypeOverride[name] ?? DTYPE.get(col.constructor)!,
    };
  }
  const blobSpecs: Record<string, { offset: number; length: number }> = {};
  for (const [name, blob] of Object.entries(blobs)) blobSpecs[name] = { offset: put(blob), length: blob.length };
  const head = new TextEncoder().encode(JSON.stringify({ final: true, ...header, count, columns: colSpecs, blobs: blobSpecs }));
  const dataStart = 4 + head.length + pad8(4 + head.length);
  const out = new Uint8Array(dataStart + offset);
  new DataView(out.buffer).setUint32(0, head.length, true);
  out.set(head, 4);
  let at = dataStart;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out.buffer;
}

function fromHex(hex: string): ArrayBuffer {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  return out.buffer;
}

// Produced by the Python reference encoder (packages/protocol, spaceknife_protocol.binary):
//   encode_frame(stream="view:v1", layout="drawlist.v1", seq=3, request_id=9, columns={
//     "key": u4 [1, 0xC0000002], "parent": i4 [-1, 0], "kind": u1 [1, 6],
//     "size": i8 [2**40, -5], "flags": u2 [32, 0x8001], "ratio": f8 [0.5, -1.25],
//     "name_len": u4 [1, 6]}, blobs={"names": "a中文".encode()})
const PYTHON_FRAME =
  "8a0100007b2273747265616d223a22766965773a7631222c22726571756573745f6964223a392c22736571223a332c2266696e616c223a74727565" +
  "2c226c61796f7574223a22647261776c6973742e7631222c22636f756e74223a322c22636f6c756d6e73223a7b226b6579223a7b226f666673" +
  "6574223a302c226474797065223a223c7534227d2c22706172656e74223a7b226f6666736574223a382c226474797065223a223c6934227d2c" +
  "226b696e64223a7b226f6666736574223a31362c226474797065223a223c7531227d2c2273697a65223a7b226f6666736574223a32342c2264" +
  "74797065223a223c6938227d2c22666c616773223a7b226f6666736574223a34302c226474797065223a223c7532227d2c22726174696f223a" +
  "7b226f6666736574223a34382c226474797065223a223c6638227d2c226e616d655f6c656e223a7b226f6666736574223a36342c2264747970" +
  "65223a223c7534227d7d2c22626c6f6273223a7b226e616d6573223a7b226f6666736574223a37322c226c656e677468223a377d7d7d000001" +
  "000000020000c0ffffffff0000000001060000000000000000000000010000fbffffffffffffff2000018000000000000000000000e03f0000" +
  "00000000f4bf010000000600000061e4b8ade6968700";

// ------------------------------------------------------------------ decodeFrame

describe("decodeFrame", () => {
  it("decodes a frame from the Python reference encoder", () => {
    const buf = fromHex(PYTHON_FRAME);
    expect(buf.byteLength).toBe(480);
    const { header, columns, blobs } = decodeFrame(buf);
    expect(header).toMatchObject({ stream: "view:v1", request_id: 9, seq: 3, final: true, layout: "drawlist.v1", count: 2 });
    expect(columns.key).toEqual(Uint32Array.of(1, 0xc0000002));
    expect(columns.parent).toEqual(Int32Array.of(-1, 0));
    expect(columns.kind).toEqual(Uint8Array.of(1, 6));
    expect(columns.size).toEqual(BigInt64Array.of(2n ** 40n, -5n));
    expect(columns.flags).toEqual(Uint16Array.of(32, 0x8001));
    expect(columns.ratio).toEqual(Float64Array.of(0.5, -1.25));
    expect(columns.name_len).toEqual(Uint32Array.of(1, 6));
    expect(new TextDecoder().decode(blobs.names)).toBe("a中文");
  });

  it("returns zero-copy views over the received buffer", () => {
    const buf = fromHex(PYTHON_FRAME);
    const { columns, blobs } = decodeFrame(buf);
    for (const col of Object.values(columns)) expect(col.buffer).toBe(buf);
    expect(blobs.names!.buffer).toBe(buf);
  });

  it("round-trips every allowed dtype, including extremes", () => {
    const sent: Record<string, Column> = {
      u1: Uint8Array.of(0, 1, 255),
      i1: Int8Array.of(-128, 0, 127),
      u2: Uint16Array.of(0, 1, 65535),
      i2: Int16Array.of(-32768, 0, 32767),
      u4: Uint32Array.of(0, 1, 0xffffffff),
      i4: Int32Array.of(-(2 ** 31), 0, 2 ** 31 - 1),
      u8: BigUint64Array.of(0n, 1n, 2n ** 64n - 1n),
      i8: BigInt64Array.of(-(2n ** 63n), 0n, 2n ** 63n - 1n),
      f4: Float32Array.of(-1.5, 0, 3.25),
      f8: Float64Array.of(Number.MIN_VALUE, -0, Number.MAX_VALUE),
    };
    const buf = encodeFrame({ stream: "s", seq: 0, layout: "rect.v1" }, sent, { a: Uint8Array.of(1, 2, 3), b: new Uint8Array(0) });
    const { header, columns, blobs } = decodeFrame(buf);
    expect(header.count).toBe(3);
    expect(Object.keys(columns)).toEqual(Object.keys(sent));
    for (const [name, col] of Object.entries(sent)) {
      expect(columns[name]!.constructor).toBe(col.constructor);
      expect(columns[name]).toEqual(col);
    }
    expect(Object.is(columns.f8![1], -0)).toBe(true);
    expect(blobs.a).toEqual(Uint8Array.of(1, 2, 3));
    expect(blobs.b).toEqual(new Uint8Array(0));
  });

  it("finds the data section after every header padding length", () => {
    const seen = new Set<number>();
    for (let n = 1; n <= 16; n++) {
      const buf = encodeFrame({ stream: "x".repeat(n), seq: n, layout: "rect.v1" }, { v: Float64Array.of(n, -n), k: Uint8Array.of(n, 2 * n) });
      seen.add(new DataView(buf).getUint32(0, true) % 8);
      const { header, columns } = decodeFrame(buf);
      expect(header.stream).toHaveLength(n);
      expect(columns.v).toEqual(Float64Array.of(n, -n));
      expect(columns.k).toEqual(Uint8Array.of(n, 2 * n));
    }
    expect(seen.size).toBe(8);
  });

  it("decodes UTF-8 in the header", () => {
    const buf = encodeFrame({ stream: "view:视图", seq: 1, layout: "rect.v1" }, { v: Uint8Array.of(9) });
    const { header, columns } = decodeFrame(buf);
    expect(header.stream).toBe("view:视图");
    expect(columns.v).toEqual(Uint8Array.of(9));
  });

  it("decodes an empty frame", () => {
    const buf = encodeFrame({ stream: "s", seq: 0, layout: "rect.v1" }, { a: new Uint32Array(0), b: new BigInt64Array(0) });
    const { header, columns, blobs } = decodeFrame(buf);
    expect(header.count).toBe(0);
    expect(columns.a).toEqual(new Uint32Array(0));
    expect(columns.b).toEqual(new BigInt64Array(0));
    expect(blobs).toEqual({});
  });

  it("treats a header without blobs as having none", () => {
    const head = new TextEncoder().encode(JSON.stringify({ stream: "s", seq: 0, final: true, layout: "rect.v1", count: 1, columns: { v: { offset: 0, dtype: "<u4" } } }));
    const dataStart = 4 + head.length + pad8(4 + head.length);
    const bytes = new Uint8Array(dataStart + 8);
    new DataView(bytes.buffer).setUint32(0, head.length, true);
    bytes.set(head, 4);
    new DataView(bytes.buffer).setUint32(dataStart, 0xdeadbeef, true);
    const frame: BinaryFrame = decodeFrame(bytes.buffer);
    expect(frame.blobs).toEqual({});
    expect(frame.columns.v).toEqual(Uint32Array.of(0xdeadbeef));
  });

  it("rejects dtypes outside the protocol's list", () => {
    for (const dtype of ["<f2", ">u4", "u4", "<c8"]) {
      const buf = encodeFrame({ stream: "s", seq: 0, layout: "rect.v1" }, { v: Uint32Array.of(1) }, {}, { v: dtype });
      expect(() => decodeFrame(buf)).toThrow(`unsupported dtype ${dtype}`);
    }
  });
});

// ------------------------------------------------------------------ RpcError

describe("RpcError", () => {
  it("is an Error carrying the JSON-RPC code and data", () => {
    const e = new RpcError(-32602, "bad params", { field: "x" });
    expect(e).toBeInstanceOf(Error);
    expect(e.message).toBe("bad params");
    expect(e.code).toBe(-32602);
    expect(e.data).toEqual({ field: "x" });
  });
});

// ------------------------------------------------------------------ Rpc over a fake socket

class FakeSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static last: FakeSocket | null = null;

  readyState = FakeSocket.CONNECTING;
  binaryType = "blob";
  sent: string[] = [];
  closedWith: number | undefined = undefined;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;

  constructor(
    readonly url: string,
    readonly protocols: string,
  ) {
    FakeSocket.last = this;
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(code?: number): void {
    this.closedWith = code;
  }

  // test-side controls
  accept(): void {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  fail(): void {
    this.onerror?.();
  }

  drop(code = 1006): void {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.({ code });
  }

  receive(msg: unknown): void {
    this.onmessage?.({ data: msg instanceof ArrayBuffer ? msg : JSON.stringify(msg) });
  }

  message(i: number): { jsonrpc: string; id?: number; method: string; params: unknown } {
    return JSON.parse(this.sent[i]!);
  }
}

const tick = () => new Promise<void>((r) => setTimeout(r, 0));

const HELLO = {
  capabilities: ["binary-frames.v1"],
  limits: { hello_timeout_s: 5, max_inbound_message_bytes: 1 << 20, max_outbound_chunk_bytes: 1 << 20 },
  protocol: "1.0",
  schema_hash: "abc",
  server: { name: "spaceknife", version: "0.3.0" },
  session_id: "s1",
};

async function connected(): Promise<{ rpc: Rpc; ws: FakeSocket }> {
  const rpc = new Rpc();
  const done = rpc.connect("ws://127.0.0.1:9/rpc", "secret", { name: "test", version: "1.2.3" });
  const ws = FakeSocket.last!;
  ws.accept();
  await tick();
  ws.receive({ jsonrpc: "2.0", id: ws.message(0).id, result: HELLO });
  await done;
  return { rpc, ws };
}

describe("Rpc", () => {
  beforeEach(() => {
    FakeSocket.last = null;
    vi.stubGlobal("WebSocket", FakeSocket);
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("opens the socket with the subprotocol and shakes hands", async () => {
    const rpc = new Rpc();
    const done = rpc.connect("ws://127.0.0.1:9/rpc", "secret", { name: "test", version: "1.2.3" });
    const ws = FakeSocket.last!;
    expect(ws.url).toBe("ws://127.0.0.1:9/rpc");
    expect(ws.protocols).toBe(SUBPROTOCOL);
    expect(ws.binaryType).toBe("arraybuffer");
    expect(ws.sent).toEqual([]); // nothing before the socket is open
    ws.accept();
    await tick();
    expect(ws.sent).toHaveLength(1);
    const hello = ws.message(0);
    expect(hello).toMatchObject({
      jsonrpc: "2.0",
      method: "session.hello",
      params: { token: "secret", protocol: PROTOCOL_VERSION, client: { name: "test", version: "1.2.3", kind: "electron" } },
    });
    expect((hello.params as { capabilities: string[] }).capabilities).toContain("binary-frames.v1");
    expect((hello.params as { capabilities: string[] }).capabilities).toContain("updates.v1"); // update.changed
    expect(rpc.hello).toBeNull();
    ws.receive({ jsonrpc: "2.0", id: hello.id, result: HELLO });
    await expect(done).resolves.toEqual(HELLO);
    expect(rpc.hello).toEqual(HELLO);
    expect(rpc.open).toBe(true);
  });

  it("rejects connect when the socket cannot open", async () => {
    const rpc = new Rpc();
    const done = rpc.connect("ws://127.0.0.1:9/rpc", "secret", { name: "test", version: "1" });
    FakeSocket.last!.fail();
    await expect(done).rejects.toThrow("无法连接后端");
  });

  it("rejects connect when the hello is refused", async () => {
    const rpc = new Rpc();
    const done = rpc.connect("ws://127.0.0.1:9/rpc", "bad", { name: "test", version: "1" });
    const ws = FakeSocket.last!;
    ws.accept();
    await tick();
    ws.receive({ jsonrpc: "2.0", id: ws.message(0).id, error: { code: -32001, message: "bad token" } });
    await expect(done).rejects.toMatchObject({ code: -32001, message: "bad token" });
  });

  it("matches responses to requests by id, in any order", async () => {
    const { rpc, ws } = await connected();
    const a = rpc.call("filter.vocab", {});
    const b = rpc.call("app.shutdown", {});
    const [ma, mb] = [ws.message(1), ws.message(2)];
    expect(ma).toMatchObject({ jsonrpc: "2.0", method: "filter.vocab", params: {} });
    expect(mb.method).toBe("app.shutdown");
    expect(ma.id).not.toBe(mb.id);
    ws.receive({ jsonrpc: "2.0", id: mb.id, result: { ok: "b" } });
    ws.receive({ jsonrpc: "2.0", id: ma.id, result: { ok: "a" } });
    await expect(a).resolves.toEqual({ ok: "a" });
    await expect(b).resolves.toEqual({ ok: "b" });
  });

  it("rejects with an RpcError carrying the error object", async () => {
    const { rpc, ws } = await connected();
    const p = rpc.call("filter.vocab", {});
    ws.receive({ jsonrpc: "2.0", id: ws.message(1).id, error: { code: -32602, message: "invalid", data: { at: 3 } } });
    const err = await p.catch((e: unknown) => e);
    expect(err).toBeInstanceOf(RpcError);
    expect(err).toMatchObject({ code: -32602, message: "invalid", data: { at: 3 } });
  });

  it("ignores responses nobody is waiting for", async () => {
    const { ws } = await connected();
    expect(() => ws.receive({ jsonrpc: "2.0", id: 999, result: {} })).not.toThrow();
  });

  it("fails calls immediately when not connected", async () => {
    const rpc = new Rpc();
    expect(rpc.open).toBe(false);
    await expect(rpc.call("filter.vocab", {})).rejects.toMatchObject({ code: -32006, message: "not connected" });
  });

  it("resolves a cancelled call to null and sends $/cancel", async () => {
    const { rpc, ws } = await connected();
    const { promise, cancel } = rpc.cancellable("filter.vocab", {});
    const id = ws.message(1).id;
    cancel();
    expect(ws.message(2)).toEqual({ jsonrpc: "2.0", method: "$/cancel", params: { id } });
    ws.receive({ jsonrpc: "2.0", id, error: { code: -32800, message: "cancelled" } });
    await expect(promise).resolves.toBeNull();
  });

  it("still rejects a cancellable call that fails for another reason", async () => {
    const { rpc, ws } = await connected();
    const { promise } = rpc.cancellable("filter.vocab", {});
    ws.receive({ jsonrpc: "2.0", id: ws.message(1).id, error: { code: -32603, message: "boom" } });
    await expect(promise).rejects.toMatchObject({ code: -32603 });
  });

  it("dispatches events to subscribers until they unsubscribe", async () => {
    const { rpc, ws } = await connected();
    const seen: unknown[] = [];
    const other: unknown[] = [];
    const off = rpc.on("$/progress", (p) => seen.push(p));
    rpc.on("log.record", (p) => other.push(p));
    ws.receive({ jsonrpc: "2.0", method: "$/progress", params: { token: 1, done: 2 } });
    off();
    ws.receive({ jsonrpc: "2.0", method: "$/progress", params: { token: 1, done: 3 } });
    ws.receive({ jsonrpc: "2.0", method: "unknown.event", params: {} });
    expect(seen).toEqual([{ token: 1, done: 2 }]);
    expect(other).toEqual([]);
  });

  it("decodes binary messages for binary subscribers", async () => {
    const { rpc, ws } = await connected();
    const frames: BinaryFrame[] = [];
    const off = rpc.onBinary((f) => frames.push(f));
    ws.receive(fromHex(PYTHON_FRAME));
    off();
    ws.receive(fromHex(PYTHON_FRAME));
    expect(frames).toHaveLength(1);
    expect(frames[0]!.header.stream).toBe("view:v1");
    expect(frames[0]!.columns.key).toEqual(Uint32Array.of(1, 0xc0000002));
  });

  it("rejects pending calls and notifies close handlers when the socket drops", async () => {
    const { rpc, ws } = await connected();
    const closes: unknown[] = [];
    rpc.onClose((ev) => closes.push(ev));
    const p = rpc.call("filter.vocab", {});
    ws.drop(1006);
    await expect(p).rejects.toMatchObject({ code: -32006, message: "connection closed" });
    expect(closes).toEqual([{ code: 1006 }]);
    expect(rpc.open).toBe(false);
  });

  it("sends notifications only while open, and closes normally", async () => {
    const { rpc, ws } = await connected();
    rpc.notify("$/ping", { n: 1 });
    expect(ws.message(1)).toEqual({ jsonrpc: "2.0", method: "$/ping", params: { n: 1 } });
    ws.readyState = FakeSocket.CLOSED;
    rpc.notify("$/ping", { n: 2 });
    expect(ws.sent).toHaveLength(2);
    rpc.close();
    expect(ws.closedWith).toBe(1000);
  });

  // ---------------------------------------------------------------- plugins.v1

  it("announces plugins.v1, colors.v1 and panels.v1 in the hello", async () => {
    const rpc = new Rpc();
    void rpc.connect("ws://127.0.0.1:9/rpc", "secret", { name: "test", version: "1" });
    const ws = FakeSocket.last!;
    ws.accept();
    await tick();
    const caps = (ws.message(0).params as { capabilities: string[] }).capabilities;
    for (const c of ["plugins.v1", "colors.v1", "panels.v1", "visuals.v1"]) expect(caps).toContain(c);
  });

  it("calls any method by name (a plugin's), and cancels it with an AbortSignal", async () => {
    const { rpc, ws } = await connected();
    const ok = rpc.callRaw("plugin.example.dupes.find", { min_size: 1 });
    expect(ws.message(1)).toMatchObject({ method: "plugin.example.dupes.find", params: { min_size: 1 } });
    ws.receive({ jsonrpc: "2.0", id: ws.message(1).id, result: { groups: [[1, 2]] } });
    await expect(ok).resolves.toEqual({ groups: [[1, 2]] });

    const ac = new AbortController();
    const slow = rpc.callRaw("plugin.example.dupes.find", {}, { signal: ac.signal });
    const id = ws.message(2).id;
    ac.abort();
    expect(ws.message(3)).toEqual({ jsonrpc: "2.0", method: "$/cancel", params: { id } });
    await expect(slow).rejects.toMatchObject({ code: -32800 });
    // the late answer finds nobody waiting
    expect(() => ws.receive({ jsonrpc: "2.0", id, result: {} })).not.toThrow();
    // an already aborted signal sends nothing
    await expect(rpc.callRaw("plugin.x.y.z", {}, { signal: ac.signal })).rejects.toMatchObject({ code: -32800 });
    expect(ws.sent).toHaveLength(4);
  });

  it("keeps a plugin call's binary frames for its answer, apart from the view frames", async () => {
    const { rpc, ws } = await connected();
    const views: BinaryFrame[] = [];
    rpc.onBinary((f) => views.push(f));
    let requestId = -1;
    const p = rpc.callFrames("plugin.t.full.columns", {}, { onId: (n) => (requestId = n) });
    expect(requestId).toBe(ws.message(1).id);
    ws.receive(encodeFrame({ stream: "plugin.t.full.columns", request_id: requestId, layout: "columns" }, { ids: Int32Array.of(0, 1, 2) }));
    ws.receive(encodeFrame({ stream: "view:v1", layout: "drawlist.v1", seq: 1 }, { key: Uint32Array.of(1) }));
    ws.receive({ jsonrpc: "2.0", id: requestId, result: { columns: true } });
    const r = await p;
    expect(r.value).toEqual({ columns: true });
    expect(r.frames).toHaveLength(1);
    expect(r.frames[0]!.columns.ids).toEqual(Int32Array.of(0, 1, 2));
    // the view's frame went to the view handlers, the plugin's did not
    expect(views.map((f) => f.header.stream)).toEqual(["view:v1"]);
  });

  it("passes a plugin's frames without a waiting request to the binary handlers (streams)", async () => {
    const { rpc, ws } = await connected();
    const seen: string[] = [];
    rpc.onBinary((f) => seen.push(f.header.stream));
    ws.receive(encodeFrame({ stream: "plugin.t.full.ticks", layout: "columns" }, { n: Int32Array.of(1) }));
    expect(seen).toEqual(["plugin.t.full.ticks"]);
  });

  it("dispatches any notification by name to raw handlers (plugin events)", async () => {
    const { rpc, ws } = await connected();
    const seen: unknown[] = [];
    const off = rpc.onRaw("plugin.t.full.progress", (p) => seen.push(p));
    ws.receive({ jsonrpc: "2.0", method: "plugin.t.full.progress", params: { done: 1, total: 2 } });
    off();
    ws.receive({ jsonrpc: "2.0", method: "plugin.t.full.progress", params: { done: 2, total: 2 } });
    expect(seen).toEqual([{ done: 1, total: 2 }]);
  });
});

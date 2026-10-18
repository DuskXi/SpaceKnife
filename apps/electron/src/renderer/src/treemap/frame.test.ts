import { describe, expect, it } from "vitest";

import type { FrameInfo } from "@/protocol/generated";
import { type BinaryFrame, type Column, decodeFrame } from "@/protocol/rpc";

import { ancestorsOf, decodeDraw, Flag, hitTest, isSynthetic, Kind, Label } from "./frame";

// ------------------------------------------------------------------ a drawlist.v1 encoder
// Mirrors spaceknife_protocol.binary.encode_frame (docs/protocol.md §4): u32 header length, JSON
// header, zero padding to 8 bytes, then 8-byte aligned columns followed by blobs.

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

function encodeFrame(stream: string, seq: number, columns: Record<string, Column>, blobs: Record<string, Uint8Array> = {}): ArrayBuffer {
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
    colSpecs[name] = { offset: put(new Uint8Array(col.buffer, col.byteOffset, col.byteLength)), dtype: DTYPE.get(col.constructor)! };
  }
  const blobSpecs: Record<string, { offset: number; length: number }> = {};
  for (const [name, blob] of Object.entries(blobs)) blobSpecs[name] = { offset: put(blob), length: blob.length };
  const header = new TextEncoder().encode(
    JSON.stringify({ stream, request_id: 7, seq, final: true, layout: "drawlist.v1", count, columns: colSpecs, blobs: blobSpecs }),
  );
  const dataStart = 4 + header.length + pad8(4 + header.length);
  const out = new Uint8Array(dataStart + offset);
  new DataView(out.buffer).setUint32(0, header.length, true);
  out.set(header, 4);
  let at = dataStart;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out.buffer;
}

// ------------------------------------------------------------------ a small synthetic scene
// Canvas 260 x 100 (physical px). Rows are pre-ordered: parents before children.
//
//   0 root (folder)  0,0 200x100          5 free space  200,0 60x100
//   ├ 1 a.mp4 (file) 2,20 100x78
//   └ 2 sub (folder) 104,20 94x78
//     ├ 3 中文.txt    106,40 90x30
//     └ 4 12 small    106,72 90x24

const SMALL_KEY = (1 << 30) + 3;
const FREE_KEY = 0xc0000001;

interface Row {
  key: number;
  node: number;
  parent: number;
  kind: number;
  depth: number;
  rect: [number, number, number, number];
  cls?: number;
  label?: number;
  size: bigint;
  count?: number;
  flags?: number;
  tag?: number;
  tone?: number;
  name: string;
}

const ROWS: Row[] = [
  { key: 1, node: 1, parent: -1, kind: Kind.FOLDER, depth: 1, rect: [0, 0, 200, 100], label: Label.NAME, size: 1000n, flags: Flag.ON_PATH, name: "root" },
  {
    key: 2,
    node: 2,
    parent: 0,
    kind: Kind.FILE,
    depth: 2,
    rect: [2, 20, 100, 78],
    cls: 1,
    label: Label.NAME | Label.SIZE,
    size: 600n,
    flags: Flag.MATCH | Flag.SELECTED,
    tag: 1,
    name: "a.mp4",
  },
  { key: 3, node: 3, parent: 0, kind: Kind.FOLDER, depth: 2, rect: [104, 20, 94, 78], label: Label.NAME, size: 350n, name: "sub" },
  { key: 4, node: 4, parent: 2, kind: Kind.FILE, depth: 3, rect: [106, 40, 90, 30], cls: 2, label: Label.NAME, size: 300n, tone: 2, name: "中文.txt" },
  { key: SMALL_KEY, node: 3, parent: 2, kind: Kind.SMALL, depth: 3, rect: [106, 72, 90, 24], size: 50n, count: 12, tone: 4, name: "" },
  { key: FREE_KEY, node: -1, parent: -1, kind: Kind.FREE, depth: 1, rect: [200, 0, 60, 100], size: 4096n, name: "" },
];

function sceneColumns(rows: Row[]): { columns: Record<string, Column>; names: Uint8Array } {
  const enc = rows.map((r) => new TextEncoder().encode(r.name));
  const names = new Uint8Array(enc.reduce((n, b) => n + b.length, 0));
  let at = 0;
  for (const b of enc) {
    names.set(b, at);
    at += b.length;
  }
  const columns: Record<string, Column> = {
    key: Uint32Array.from(rows, (r) => r.key),
    node: Int32Array.from(rows, (r) => r.node),
    parent: Int32Array.from(rows, (r) => r.parent),
    kind: Uint8Array.from(rows, (r) => r.kind),
    depth: Uint8Array.from(rows, (r) => r.depth),
    x: Int32Array.from(rows, (r) => r.rect[0]),
    y: Int32Array.from(rows, (r) => r.rect[1]),
    w: Int32Array.from(rows, (r) => r.rect[2]),
    h: Int32Array.from(rows, (r) => r.rect[3]),
    cls: Uint8Array.from(rows, (r) => r.cls ?? 0),
    label: Uint8Array.from(rows, (r) => r.label ?? 0),
    size: BigInt64Array.from(rows, (r) => r.size),
    count: Uint32Array.from(rows, (r) => r.count ?? 0),
    flags: Uint16Array.from(rows, (r) => r.flags ?? 0),
    tag: Uint8Array.from(rows, (r) => r.tag ?? 0),
    tone: Uint8Array.from(rows, (r) => r.tone ?? 0),
    name_len: Uint32Array.from(enc, (b) => b.length),
  };
  return { columns, names };
}

const INFO: FrameInfo = {
  breadcrumb: [{ key: 1, label: "root" }],
  dpr: 1,
  filter: null,
  height: 100,
  layout_ms: 0.4,
  reason: "open",
  rows: ROWS.length,
  scanned: 1000,
  selected_row: 1,
  seq: 5,
  view_id: "v1",
  width: 260,
};

function scene() {
  const { columns, names } = sceneColumns(ROWS);
  const buf = encodeFrame("view:v1", 5, columns, { names });
  const frame = decodeFrame(buf);
  return { buf, frame, draw: decodeDraw(frame, INFO) };
}

// ------------------------------------------------------------------ tests

describe("decodeDraw", () => {
  it("decodes every column of a wire frame", () => {
    const { frame, draw: f } = scene();
    expect(frame.header.layout).toBe("drawlist.v1");
    expect(f.info).toBe(INFO);
    expect(f.count).toBe(ROWS.length);
    expect(Array.from(f.key)).toEqual(ROWS.map((r) => r.key));
    expect(Array.from(f.node)).toEqual(ROWS.map((r) => r.node));
    expect(Array.from(f.parent)).toEqual(ROWS.map((r) => r.parent));
    expect(Array.from(f.kind)).toEqual(ROWS.map((r) => r.kind));
    expect(Array.from(f.depth)).toEqual(ROWS.map((r) => r.depth));
    expect(Array.from(f.x)).toEqual(ROWS.map((r) => r.rect[0]));
    expect(Array.from(f.y)).toEqual(ROWS.map((r) => r.rect[1]));
    expect(Array.from(f.w)).toEqual(ROWS.map((r) => r.rect[2]));
    expect(Array.from(f.h)).toEqual(ROWS.map((r) => r.rect[3]));
    expect(Array.from(f.cls)).toEqual(ROWS.map((r) => r.cls ?? 0));
    expect(Array.from(f.label)).toEqual(ROWS.map((r) => r.label ?? 0));
    expect(Array.from(f.size)).toEqual(ROWS.map((r) => r.size));
    expect(Array.from(f.count_)).toEqual(ROWS.map((r) => r.count ?? 0));
    expect(Array.from(f.flags)).toEqual(ROWS.map((r) => r.flags ?? 0));
    expect(Array.from(f.tag)).toEqual(ROWS.map((r) => r.tag ?? 0));
    expect(Array.from(f.tone)).toEqual(ROWS.map((r) => r.tone ?? 0));
  });

  it("uses the typed arrays the protocol specifies", () => {
    const { draw: f } = scene();
    expect(f.key).toBeInstanceOf(Uint32Array);
    expect(f.node).toBeInstanceOf(Int32Array);
    expect(f.parent).toBeInstanceOf(Int32Array);
    expect(f.kind).toBeInstanceOf(Uint8Array);
    expect(f.x).toBeInstanceOf(Int32Array);
    expect(f.size).toBeInstanceOf(BigInt64Array);
    expect(f.count_).toBeInstanceOf(Uint32Array);
    expect(f.flags).toBeInstanceOf(Uint16Array);
    // u4 keys with the high bit set stay unsigned
    expect(f.key[5]).toBe(FREE_KEY);
    expect(f.node[5]).toBe(-1);
  });

  it("views the received buffer without copying", () => {
    const { buf, draw: f } = scene();
    for (const col of [f.key, f.node, f.parent, f.kind, f.x, f.size, f.flags, f.tone]) expect(col.buffer).toBe(buf);
  });

  it("splits the names blob by byte length (UTF-8), empty for name_len 0", () => {
    const { draw: f } = scene();
    expect(f.names).toEqual(["root", "a.mp4", "sub", "中文.txt", "", ""]);
    expect(new TextEncoder().encode("中文.txt").length).toBe(10);
  });

  it("tolerates a frame without a names blob when every name is empty", () => {
    const rows = ROWS.map((r) => ({ ...r, name: "" }));
    const { columns } = sceneColumns(rows);
    const frame: BinaryFrame = {
      header: { stream: "view:v1", seq: 1, final: true, layout: "drawlist.v1", count: rows.length, columns: {} },
      columns,
      blobs: {},
    };
    const f = decodeDraw(frame, INFO);
    expect(f.names).toEqual(rows.map(() => ""));
  });

  it("decodes an empty frame", () => {
    const { columns, names } = sceneColumns([]);
    const f = decodeDraw(decodeFrame(encodeFrame("view:v1", 0, columns, { names })), { ...INFO, rows: 0 });
    expect(f.count).toBe(0);
    expect(f.names).toEqual([]);
    expect(f.rowOf(1)).toBe(-1);
    expect(hitTest(f, 10, 10)).toBe(-1);
  });

  it("maps keys to rows, including synthetic keys, and -1 for unknown keys", () => {
    const { draw: f } = scene();
    ROWS.forEach((r, i) => expect(f.rowOf(r.key)).toBe(i));
    expect(f.rowOf(SMALL_KEY)).toBe(4);
    expect(f.rowOf(FREE_KEY)).toBe(5);
    expect(f.rowOf(999)).toBe(-1);
    // the lazily built index is reused
    expect(f.rowOf(3)).toBe(2);
  });
});

describe("hitTest", () => {
  const { draw: f } = scene();

  it("finds the deepest row under a point", () => {
    expect(hitTest(f, 10, 30)).toBe(1); // a.mp4
    expect(hitTest(f, 5, 5)).toBe(0); // root header
    expect(hitTest(f, 150, 25)).toBe(2); // sub header, above its children
    expect(hitTest(f, 110, 50)).toBe(3); // 中文.txt inside sub
    expect(hitTest(f, 110, 80)).toBe(4); // the small-items block
    expect(hitTest(f, 230, 50)).toBe(5); // free space
  });

  it("treats rectangles as half-open [x, x + w)", () => {
    expect(hitTest(f, 2, 20)).toBe(1);
    expect(hitTest(f, 101, 97)).toBe(1);
    expect(hitTest(f, 102, 50)).toBe(0); // the gap between a.mp4 and sub belongs to root
    expect(hitTest(f, 101, 98)).toBe(0);
    expect(hitTest(f, 199, 99)).toBe(0);
    expect(hitTest(f, 200, 50)).toBe(5); // root ends where free space starts
    expect(hitTest(f, 10.5, 30.5)).toBe(1);
  });

  it("is -1 outside every row", () => {
    expect(hitTest(f, 260, 50)).toBe(-1);
    expect(hitTest(f, -1, 0)).toBe(-1);
    expect(hitTest(f, 0, 100)).toBe(-1);
    expect(hitTest(f, 0, -0.5)).toBe(-1);
  });
});

describe("ancestorsOf", () => {
  const { draw: f } = scene();

  it("walks up to the top, nearest parent first, excluding the row", () => {
    expect(ancestorsOf(f, 3)).toEqual([2, 0]);
    expect(ancestorsOf(f, 4)).toEqual([2, 0]);
    expect(ancestorsOf(f, 1)).toEqual([0]);
    expect(ancestorsOf(f, 2)).toEqual([0]);
  });

  it("is empty for top-level rows", () => {
    expect(ancestorsOf(f, 0)).toEqual([]);
    expect(ancestorsOf(f, 5)).toEqual([]);
  });

  it("agrees with hitTest: every ancestor contains the hit point", () => {
    const row = hitTest(f, 110, 50);
    for (const a of ancestorsOf(f, row)) {
      expect(f.x[a]! <= 110 && 110 < f.x[a]! + f.w[a]!).toBe(true);
      expect(f.y[a]! <= 50 && 50 < f.y[a]! + f.h[a]!).toBe(true);
    }
  });
});

describe("isSynthetic", () => {
  it("is true exactly when one of the top two key bits is set", () => {
    expect(isSynthetic(0)).toBe(false);
    expect(isSynthetic(1)).toBe(false);
    expect(isSynthetic(0x3fffffff)).toBe(false);
    expect(isSynthetic(0x40000000)).toBe(true);
    expect(isSynthetic(0x80000000)).toBe(true);
    expect(isSynthetic(0xc0000000)).toBe(true);
    expect(isSynthetic(0xffffffff)).toBe(true);
  });

  it("classifies the scene's rows", () => {
    const { draw: f } = scene();
    expect(Array.from(f.key, isSynthetic)).toEqual([false, false, false, false, true, true]);
  });
});

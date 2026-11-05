// Test helpers: wire frames of any layout, and small scenes laid out the way the backend lays them
// out (docs/protocol.md §4, docs/visuals.md), for geometry and paint tests under node.

import type { FlameGeometry, FrameInfo, FrameRoot, PolarGeometry } from "@/protocol/generated";
import { type BinaryFrame, type Column, decodeFrame } from "@/protocol/rpc";

import { decodeVisualFrame, Flag, GEOMETRY_COLUMNS, Kind, Label } from "../frame";
import type { PathSink, PolarFrame, Pt, RectFrame, VisualFrame } from "../types";

// ------------------------------------------------------------------ the wire encoder
// Mirrors spaceknife_protocol.binary.encode_frame: u32 header length, JSON header, zero padding to
// 8 bytes, then 8-byte aligned columns followed by blobs.

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

/** A binary view frame with the given columns; `count` is the length of the columns (0 without any). */
export function encodeBinary(layout: string, columns: Record<string, Column>, blobs: Record<string, Uint8Array> = {}, seq = 1, stream = "view:v1"): ArrayBuffer {
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
  const header = new TextEncoder().encode(JSON.stringify({ stream, request_id: 7, seq, final: true, layout, count, columns: colSpecs, blobs: blobSpecs }));
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

// ------------------------------------------------------------------ rows and frames

export interface FixtureRow {
  key: number;
  node: number;
  parent: number;
  kind: number;
  depth: number;
  /** the layout's geometry columns in order: x y w h, or a0 a1 r0 r1 */
  geo: readonly [number, number, number, number];
  cls?: number;
  label?: number;
  size: bigint;
  count?: number;
  flags?: number;
  tag?: number;
  tone?: number;
  branch?: number;
  /** compare views: the delta and dstat columns are sent when a row has either */
  delta?: bigint;
  dstat?: number;
  name?: string;
}

export const SMALL_KEY = (1 << 30) + 3;
export const FREE_KEY = 0xc0000001;

const VISUAL_OF: Record<string, string> = { "drawlist.v1": "treemap", "flame.v1": "flame", "polar.v1": "flame_polar" };

/** Wire columns of rows: the shared ones, the layout's geometry, `branch`, and delta / dstat when used. */
export function rowColumns(layout: string, rows: readonly FixtureRow[]): { columns: Record<string, Column>; names: Uint8Array } {
  const enc = rows.map((r) => new TextEncoder().encode(r.name ?? ""));
  const names = new Uint8Array(enc.reduce((n, b) => n + b.length, 0));
  let at = 0;
  for (const b of enc) {
    names.set(b, at);
    at += b.length;
  }
  const geo = GEOMETRY_COLUMNS[layout] ?? GEOMETRY_COLUMNS["drawlist.v1"]!;
  const columns: Record<string, Column> = {
    key: Uint32Array.from(rows, (r) => r.key),
    node: Int32Array.from(rows, (r) => r.node),
    parent: Int32Array.from(rows, (r) => r.parent),
    kind: Uint8Array.from(rows, (r) => r.kind),
    depth: Uint8Array.from(rows, (r) => r.depth),
  };
  geo.forEach((name, k) => (columns[name] = Int32Array.from(rows, (r) => r.geo[k]!)));
  Object.assign(columns, {
    cls: Uint8Array.from(rows, (r) => r.cls ?? 0),
    label: Uint8Array.from(rows, (r) => r.label ?? 0),
    size: BigInt64Array.from(rows, (r) => r.size),
    count: Uint32Array.from(rows, (r) => r.count ?? 0),
    flags: Uint16Array.from(rows, (r) => r.flags ?? 0),
    tag: Uint8Array.from(rows, (r) => r.tag ?? 0),
    tone: Uint8Array.from(rows, (r) => r.tone ?? 0),
    branch: Uint8Array.from(rows, (r) => r.branch ?? 0),
    name_len: Uint32Array.from(enc, (b) => b.length),
  });
  if (rows.some((r) => r.delta !== undefined || r.dstat !== undefined)) {
    columns.delta = BigInt64Array.from(rows, (r) => r.delta ?? 0n);
    columns.dstat = Uint8Array.from(rows, (r) => r.dstat ?? 0);
  }
  return { columns, names };
}

/** A FrameInfo with every field set (a treemap frame of 260 × 100 px unless patched). */
export function frameInfo(patch: Partial<FrameInfo> = {}): FrameInfo {
  return {
    breadcrumb: [{ key: 1, label: "root" }],
    dpr: 1,
    filter: null,
    flash: [],
    geometry: null,
    height: 100,
    layout: "drawlist.v1",
    layout_ms: 0.4,
    reason: "open",
    root: null,
    rows: 0,
    scanned: 1000,
    selected_row: null,
    seq: 1,
    view_id: "v1",
    visual: "treemap",
    width: 260,
    ...patch,
  };
}

export interface Fixture<F extends VisualFrame = VisualFrame> {
  /** what was encoded */
  rows: readonly FixtureRow[];
  buf: ArrayBuffer;
  bin: BinaryFrame;
  info: FrameInfo;
  frame: F;
}

/** Rows encoded as a wire frame of `layout` and decoded again; the layout must be one visual/frame.ts knows. */
export function encodeFrame<F extends VisualFrame = VisualFrame>(layout: string, rows: readonly FixtureRow[], infoPatch: Partial<FrameInfo> = {}): Fixture<F> {
  const { columns, names } = rowColumns(layout, rows);
  const info = frameInfo({ layout, visual: VISUAL_OF[layout] ?? "treemap", rows: rows.length, ...infoPatch });
  const buf = encodeBinary(layout, columns, { names }, info.seq);
  const bin = decodeFrame(buf);
  const frame = decodeVisualFrame(bin, info);
  if (!frame) throw new Error(`no decoder for layout ${layout}`);
  return { rows, buf, bin, info, frame: frame as F };
}

// ------------------------------------------------------------------ scenes

/** The treemap scene of 260 × 100 px (parents before children):
 *
 *    0 root (folder)  0,0 200x100          5 free space  200,0 60x100
 *    ├ 1 a.mp4 (file) 2,20 100x78
 *    └ 2 sub (folder) 104,20 94x78
 *      ├ 3 中文.txt    106,40 90x30
 *      └ 4 12 small    106,72 90x24
 */
export const TREEMAP_ROWS: readonly FixtureRow[] = [
  { key: 1, node: 1, parent: -1, kind: Kind.FOLDER, depth: 1, geo: [0, 0, 200, 100], label: Label.NAME, size: 1000n, flags: Flag.ON_PATH, name: "root" },
  { key: 2, node: 2, parent: 0, kind: Kind.FILE, depth: 2, geo: [2, 20, 100, 78], cls: 1, label: Label.NAME | Label.SIZE, size: 600n, flags: Flag.MATCH | Flag.SELECTED, tag: 1, branch: 1, name: "a.mp4" },
  { key: 3, node: 3, parent: 0, kind: Kind.FOLDER, depth: 2, geo: [104, 20, 94, 78], label: Label.NAME, size: 350n, branch: 2, name: "sub" },
  { key: 4, node: 4, parent: 2, kind: Kind.FILE, depth: 3, geo: [106, 40, 90, 30], cls: 2, label: Label.NAME, size: 300n, tone: 2, branch: 2, name: "中文.txt" }, // i18n-ignore
  { key: SMALL_KEY, node: 3, parent: 2, kind: Kind.SMALL, depth: 3, geo: [106, 72, 90, 24], size: 50n, count: 12, tone: 4, branch: 2 },
  { key: FREE_KEY, node: -1, parent: -1, kind: Kind.FREE, depth: 1, geo: [200, 0, 60, 100], size: 4096n },
];

/** A node of a scene laid out by flame.v1 / polar.v1; the tree's top is the zoom root (FrameInfo.root). */
export interface SceneNode {
  key: number;
  node: number;
  kind: number;
  size: bigint;
  name?: string;
  cls?: number;
  /** label bits; NAME by default */
  label?: number;
  count?: number;
  flags?: number;
  tag?: number;
  tone?: number;
  /** branch slot; by default level 1 takes 1, 2, … in order (aggregates and free space 0), the rest inherit */
  branch?: number;
  delta?: bigint;
  dstat?: number;
  children?: SceneNode[];
}

/** Zoom root of 1000 B: its children fill 90 % (the last 10 % is the root's own share, left blank).
 *
 *    a.mp4 400 · sub 350 [中文.txt 200 · docs 100 [x.pdf 100] · 12 small 50] · cache (waiting) 100 · free 50
 */
export const SCENE_TREE: SceneNode = {
  key: 1,
  node: 1,
  kind: Kind.FOLDER,
  size: 1000n,
  name: "root",
  children: [
    { key: 2, node: 2, kind: Kind.FILE, size: 400n, name: "a.mp4", cls: 1, tag: 1, flags: Flag.MATCH | Flag.SELECTED },
    {
      key: 3,
      node: 3,
      kind: Kind.FOLDER,
      size: 350n,
      name: "sub",
      flags: Flag.ON_PATH,
      children: [
        { key: 4, node: 4, kind: Kind.FILE, size: 200n, name: "中文.txt", cls: 2, tone: 2 }, // i18n-ignore
        { key: 5, node: 5, kind: Kind.FOLDER, size: 100n, name: "docs", children: [{ key: 6, node: 6, kind: Kind.FILE, size: 100n, name: "x.pdf", cls: 3 }] },
        { key: SMALL_KEY, node: 3, kind: Kind.SMALL, size: 50n, count: 12, tone: 4 },
      ],
    },
    { key: 7, node: 7, kind: Kind.PENDING, size: 100n, name: "cache", flags: Flag.SCANNING },
    { key: FREE_KEY, node: -1, kind: Kind.FREE, size: 50n },
  ],
};

/** How a scene's levels map to geometry columns (a test-sized copy of the backend's projections). */
export interface Projection {
  /** levels drawn beyond the zoom root */
  levels: number;
  /** geometry columns of [a, b) (fractions of the root) at a level; null when too small to draw */
  cell(a: number, b: number, level: number): [number, number, number, number] | null;
}

/** Pre-order partition of a scene: each container's children share its span in proportion to size. */
function partition(tree: SceneNode, proj: Projection): FixtureRow[] {
  const rows: FixtureRow[] = [];
  const walk = (c: SceneNode, u0: number, u1: number, level: number, parent: number, hue: number) => {
    if (level > proj.levels) return;
    const kids = (c.children ?? []).filter((k) => k.size > 0n);
    // the container's own share stays blank after its children
    const basis = Math.max(kids.reduce((s, k) => s + Number(k.size), 0), Number(c.size));
    if (basis <= 0) return;
    const span = u1 - u0;
    let cur = u0;
    let slot = 0;
    for (const k of kids) {
      const a = cur;
      const b = Math.min(u1, cur + (Number(k.size) * span) / basis);
      cur = b;
      const geo = proj.cell(a, b, level);
      if (!geo) continue;
      const real = k.kind <= Kind.PENDING;
      const branch = k.branch ?? (level > 1 ? hue : real ? (slot++ % 8) + 1 : 0);
      const open = k.kind === Kind.FOLDER && !!k.children?.length && level < proj.levels;
      const label = k.label ?? Label.NAME;
      const row = rows.length;
      rows.push({
        key: k.key,
        node: k.node,
        parent,
        kind: k.kind === Kind.FOLDER && !open ? Kind.COLLAPSED : k.kind,
        depth: level,
        geo,
        cls: k.cls,
        label,
        size: k.size,
        count: k.count,
        flags: k.flags,
        tag: k.tag,
        tone: k.tone,
        branch,
        delta: k.delta,
        dstat: k.dstat,
        name: real && label ? k.name : undefined,
      });
      if (open) walk(k, a, b, level + 1, row, branch);
    }
  };
  walk(tree, 0, 1, 1, -1, 0);
  return rows;
}

function rootOf(tree: SceneNode): FrameRoot {
  return {
    key: tree.key,
    node: tree.node,
    kind: tree.kind,
    size: Number(tree.size),
    count: tree.count ?? 0,
    flags: tree.flags ?? 0,
    tag: tree.tag ?? 0,
    tone: tree.tone ?? 4,
    branch: 0,
    delta: tree.delta === undefined ? null : Number(tree.delta),
    dstat: tree.dstat ?? null,
  };
}

function sceneInfo(tree: SceneNode, rows: FixtureRow[], patch: Partial<FrameInfo>): Partial<FrameInfo> {
  const sel = rows.findIndex((r) => ((r.flags ?? 0) & Flag.SELECTED) !== 0);
  return { breadcrumb: [{ key: tree.key, label: tree.name ?? "" }], root: rootOf(tree), selected_row: sel >= 0 ? sel : null, ...patch };
}

export interface SceneOptions {
  width?: number;
  height?: number;
  dpr?: number;
  /** 细节 (levels asked for) */
  detail?: number;
  tree?: SceneNode;
}

/** flame.v1 geometry of a canvas (docs/visuals.md, layout/flame.py), at font scale 1. */
export function flameProjection(width: number, height: number, dpr: number, detail: number, origin: "top" | "bottom"): Projection & { geometry: FlameGeometry } {
  const g = Math.max(1, Math.round(2 * dpr));
  const rowMin = Math.round(20 * dpr);
  const rowMax = Math.round(48 * dpr);
  const n = Math.max(0, Math.min(detail, Math.floor(height / rowMin) - 1));
  const rowH = Math.min(Math.floor(height / (n + 1)), rowMax);
  const bandY = (k: number) => (origin === "top" ? k * rowH : height - k * rowH - (rowH - g));
  return {
    levels: n,
    geometry: { kind: "flame", origin, row_h: rowH, rows: n, gap: g, root: { x: 0, y: bandY(0), w: width, h: rowH - g } },
    cell(a, b, level) {
      const x = Math.round(a * (width + g));
      const w = Math.round(b * (width + g)) - g - x;
      return w >= Math.round(2 * dpr) ? [x, bandY(level), w, rowH - g] : null;
    },
  };
}

/** polar.v1 geometry of a canvas (docs/visuals.md, layout/flame_polar.py), at font scale 1. */
export function polarProjection(width: number, height: number, dpr: number, detail: number): Projection & { geometry: PolarGeometry } {
  const U = 1 << 20;
  const g = Math.max(1, Math.round(2 * dpr));
  const R = Math.floor(Math.min(width, height) / 2) - Math.round(8 * dpr);
  const hole = Math.min(Math.max(Math.round(0.24 * R), Math.round(36 * dpr)), Math.round(0.4 * R));
  const n = R > hole ? Math.max(0, Math.min(detail, Math.floor((R - hole) / Math.round(14 * dpr)))) : 0;
  const ringW = n ? Math.floor((R - hole) / n) : 0;
  return {
    levels: n,
    geometry: { kind: "polar", cx: Math.floor(width / 2), cy: Math.floor(height / 2), radius: hole + n * ringW, hole, ring_w: ringW, rings: n, gap: g, unit: U },
    cell(a, b, level) {
      const lo = hole + (level - 1) * ringW;
      const r0 = lo + Math.ceil(g / 2);
      const r1 = lo + ringW - Math.floor(g / 2);
      const a0 = Math.round(a * U);
      const a1 = Math.round(b * U);
      return ((a1 - a0) / U) * 2 * Math.PI * ((r0 + r1) / 2) >= 2 * dpr ? [a0, a1, r0, r1] : null;
    },
  };
}

/** A rectangle scene: the treemap one (drawlist.v1, TREEMAP_ROWS) or a flame graph of a scene tree (400 × 200 px). */
export function rectScene(o: SceneOptions & { layout?: "drawlist.v1" | "flame.v1"; origin?: "top" | "bottom"; info?: Partial<FrameInfo> } = {}): Fixture<RectFrame> {
  if ((o.layout ?? "drawlist.v1") === "drawlist.v1") return encodeFrame<RectFrame>("drawlist.v1", TREEMAP_ROWS, { selected_row: 1, dpr: o.dpr ?? 1, ...o.info });
  const [width, height, dpr] = [o.width ?? 400, o.height ?? 200, o.dpr ?? 1];
  const tree = o.tree ?? SCENE_TREE;
  const proj = flameProjection(width, height, dpr, o.detail ?? 5, o.origin ?? "bottom");
  const rows = partition(tree, proj);
  return encodeFrame<RectFrame>("flame.v1", rows, sceneInfo(tree, rows, { width, height, dpr, geometry: proj.geometry, ...o.info }));
}

/** A polar flame graph of a scene tree (400 × 400 px). */
export function polarScene(o: SceneOptions & { info?: Partial<FrameInfo> } = {}): Fixture<PolarFrame> {
  const [width, height, dpr] = [o.width ?? 400, o.height ?? 400, o.dpr ?? 1];
  const tree = o.tree ?? SCENE_TREE;
  const proj = polarProjection(width, height, dpr, o.detail ?? 5);
  const rows = partition(tree, proj);
  return encodeFrame<PolarFrame>("polar.v1", rows, sceneInfo(tree, rows, { width, height, dpr, geometry: proj.geometry, ...o.info }));
}

// ------------------------------------------------------------------ outlines

export interface PathOp {
  op: "moveTo" | "lineTo" | "arc" | "closePath" | "rect";
  args: number[];
}

/** A PathSink that keeps what it was told, and samples it into points. */
export class PathRecorder implements PathSink {
  ops: PathOp[] = [];

  moveTo(x: number, y: number): void {
    this.ops.push({ op: "moveTo", args: [x, y] });
  }

  lineTo(x: number, y: number): void {
    this.ops.push({ op: "lineTo", args: [x, y] });
  }

  arc(x: number, y: number, radius: number, startAngle: number, endAngle: number, counterclockwise = false): void {
    this.ops.push({ op: "arc", args: [x, y, radius, startAngle, endAngle, counterclockwise ? 1 : 0] });
  }

  closePath(): void {
    this.ops.push({ op: "closePath", args: [] });
  }

  rect(x: number, y: number, w: number, h: number): void {
    this.ops.push({ op: "rect", args: [x, y, w, h] });
  }

  /** Vertices, rectangle corners, and arcs sampled `perArc` times along their sweep (canvas semantics). */
  points(perArc = 32): Pt[] {
    const out: Pt[] = [];
    for (const { op, args: v } of this.ops) {
      if (op === "moveTo" || op === "lineTo") out.push({ x: v[0]!, y: v[1]! });
      else if (op === "rect") {
        const [x, y, w, h] = v as [number, number, number, number];
        out.push({ x, y }, { x: x + w, y }, { x: x + w, y: y + h }, { x, y: y + h });
      } else if (op === "arc") {
        const [cx, cy, r, a0, a1, ccw] = v as [number, number, number, number, number, number];
        const turn = 2 * Math.PI;
        const raw = ccw ? a0 - a1 : a1 - a0;
        const sweep = raw >= turn ? turn : ((raw % turn) + turn) % turn;
        for (let i = 0; i <= perArc; i++) {
          const a = a0 + (ccw ? -1 : 1) * sweep * (i / perArc);
          out.push({ x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) });
        }
      }
    }
    return out;
  }
}

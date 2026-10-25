// View frames of every layout (docs/protocol.md §4): the shared columns, per-row names, and the
// layout's geometry columns by name.

import type { FrameInfo } from "@/protocol/generated";
import type { BinaryFrame } from "@/protocol/rpc";

import type { PolarFrame, RectFrame, ShapesFrame, VisualFrame } from "./types";

export const Kind = { FILE: 0, FOLDER: 1, COLLAPSED: 2, PENDING: 3, SMALL: 4, MORE: 5, FREE: 6, UNKNOWN: 7 } as const;
/** RADIAL only in polar.v1: the label runs along the radius; TOP only in shapes.v1: the name sits
 * at the top inside the shape */
export const Label = { NAME: 1, SIZE: 2, BIG: 4, SHORT: 8, RADIAL: 16, TOP: 32 } as const;
export const DStat = { SAME: 0, NEW: 1, DELETED: 2, CHANGED: 3 } as const;
export const Flag = { MATCH: 1, DIM: 2, FOLDER_HIT: 4, TAGGED: 8, SELECTED: 16, ON_PATH: 32, SCANNING: 64, TRASH: 128, TRASH_BIN: 256 } as const;

/** Geometry columns of each layout this renderer decodes (all <i4). */
export const GEOMETRY_COLUMNS: Readonly<Record<string, readonly string[]>> = {
  "drawlist.v1": ["x", "y", "w", "h"],
  "flame.v1": ["x", "y", "w", "h"],
  "polar.v1": ["a0", "a1", "r0", "r1"],
  // a plugin's own layout: bounding box, corner radius, points of a convex polygon (blob "points")
  "shapes.v1": ["x", "y", "w", "h", "r", "poly"],
};

export const isKnownLayout = (layout: string): boolean => Object.hasOwn(GEOMETRY_COLUMNS, layout);

const decoder = new TextDecoder();

/** Typed columns of a wire frame; null when the layout is unknown or a geometry column is missing. */
export function decodeVisualFrame(frame: BinaryFrame, info: FrameInfo): VisualFrame | null {
  const layout = frame.header.layout;
  const geo = isKnownLayout(layout) ? GEOMETRY_COLUMNS[layout]! : null;
  if (!geo) return null;
  const c = frame.columns;
  const geometry: Record<string, Int32Array> = {};
  for (const name of geo) {
    const col = c[name];
    if (!(col instanceof Int32Array)) return null;
    geometry[name] = col;
  }
  const n = frame.header.count;
  const lens = c.name_len as Uint32Array;
  const blob = frame.blobs.names ?? new Uint8Array(0);
  const names = new Array<string>(n);
  let off = 0;
  for (let i = 0; i < n; i++) {
    const len = lens[i]!;
    names[i] = len ? decoder.decode(blob.subarray(off, off + len)) : "";
    off += len;
  }
  let index: Map<number, number> | null = null;
  const key = c.key as Uint32Array;
  const out: VisualFrame = {
    info,
    layout,
    count: n,
    key,
    node: c.node as Int32Array,
    parent: c.parent as Int32Array,
    kind: c.kind as Uint8Array,
    depth: c.depth as Uint8Array,
    cls: c.cls as Uint8Array,
    label: c.label as Uint8Array,
    size: c.size as BigInt64Array,
    count_: c.count as Uint32Array,
    flags: c.flags as Uint16Array,
    tag: c.tag as Uint8Array,
    tone: c.tone as Uint8Array,
    branch: (c.branch as Uint8Array | undefined) ?? null,
    delta: (c.delta as BigInt64Array | undefined) ?? null,
    dstat: (c.dstat as Uint8Array | undefined) ?? null,
    pal: c.pal instanceof Uint8Array ? c.pal : null,
    names,
    geometry,
    rowOf(k: number): number {
      if (!index) {
        index = new Map();
        for (let i = 0; i < n; i++) index.set(key[i]!, i);
      }
      return index.get(k) ?? -1;
    },
  };
  // shapes.v1: the polygons' points (x, y pairs of the rows with poly > 0, in row order)
  if (layout === "shapes.v1") {
    const raw = frame.blobs.points ?? new Uint8Array(0);
    const points = new Int32Array(raw.slice().buffer, 0, raw.length >> 2);
    const poly = geometry.poly!;
    const start = new Int32Array(n + 1);
    for (let i = 0; i < n; i++) start[i + 1] = start[i]! + Math.max(0, poly[i]!);
    if (2 * start[n]! > points.length) return null; // the points do not add up: not a frame to draw
    Object.assign(out, { points, pointStart: start });
  }
  // the rectangle and sector frames read their columns by name: f.x, f.a0, …
  return Object.assign(out, geometry);
}

/** A frame whose geometry columns are x y w h (drawlist.v1, flame.v1). */
export const isRectFrame = (f: VisualFrame): f is RectFrame => f.geometry.x !== undefined && f.geometry.h !== undefined;

/** A frame of a plugin's own layout (shapes.v1): boxes, corner radii and polygons. */
export const isShapesFrame = (f: VisualFrame): f is ShapesFrame => f.layout === "shapes.v1" && f.geometry.poly !== undefined;

/** Whether a folder's rows are drawn inside it (the treemap, a nested plugin layout such as circle
 * packing) rather than beside it (the flame graphs, an icicle). */
export function isNested(f: VisualFrame): boolean {
  if (f.layout === "drawlist.v1") return true;
  const g = f.info.geometry;
  return !!g && g.kind === "shapes" && g.nested;
}

/** A frame whose geometry columns are a0 a1 r0 r1 (polar.v1). */
export const isPolarFrame = (f: VisualFrame): f is PolarFrame => f.geometry.a0 !== undefined && f.geometry.r1 !== undefined;

/** Rows from a row up to the top (excluding the row itself). */
export function ancestorsOf(f: Pick<VisualFrame, "parent">, row: number): number[] {
  const out: number[] = [];
  let p = f.parent[row]!;
  while (p >= 0) {
    out.push(p);
    p = f.parent[p]!;
  }
  return out;
}

/** Whether `row` is `top` or lies below it. */
export function isWithin(f: Pick<VisualFrame, "parent">, row: number, top: number): boolean {
  for (let r = row; r >= 0; r = f.parent[r]!) if (r === top) return true;
  return false;
}

export const isSynthetic = (key: number): boolean => key >>> 30 !== 0;

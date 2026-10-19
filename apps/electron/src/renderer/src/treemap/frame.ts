// drawlist.v1 (docs/protocol.md §4.1) decoded into typed columns plus per-row names.

import type { FrameInfo } from "@/protocol/generated";
import type { BinaryFrame } from "@/protocol/rpc";

export const Kind = { FILE: 0, FOLDER: 1, COLLAPSED: 2, PENDING: 3, SMALL: 4, MORE: 5, FREE: 6, UNKNOWN: 7 } as const;
export const Label = { NAME: 1, SIZE: 2, BIG: 4, SHORT: 8 } as const;
export const DStat = { SAME: 0, NEW: 1, DELETED: 2, CHANGED: 3 } as const;
export const Flag = { MATCH: 1, DIM: 2, FOLDER_HIT: 4, TAGGED: 8, SELECTED: 16, ON_PATH: 32, SCANNING: 64 } as const;

export interface DrawFrame {
  info: FrameInfo;
  count: number;
  key: Uint32Array;
  node: Int32Array;
  parent: Int32Array;
  kind: Uint8Array;
  depth: Uint8Array;
  x: Int32Array;
  y: Int32Array;
  w: Int32Array;
  h: Int32Array;
  cls: Uint8Array;
  label: Uint8Array;
  size: BigInt64Array;
  count_: Uint32Array;
  flags: Uint16Array;
  tag: Uint8Array;
  tone: Uint8Array;
  /** compare views (W6): signed net change and status per row (0 same, 1 new, 2 deleted, 3 changed) */
  delta: BigInt64Array | null;
  dstat: Uint8Array | null;
  names: string[];
  /** row of a key, built lazily */
  rowOf(key: number): number;
}

const decoder = new TextDecoder();

export function decodeDraw(frame: BinaryFrame, info: FrameInfo): DrawFrame {
  const c = frame.columns;
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
  return {
    info,
    count: n,
    key,
    node: c.node as Int32Array,
    parent: c.parent as Int32Array,
    kind: c.kind as Uint8Array,
    depth: c.depth as Uint8Array,
    x: c.x as Int32Array,
    y: c.y as Int32Array,
    w: c.w as Int32Array,
    h: c.h as Int32Array,
    cls: c.cls as Uint8Array,
    label: c.label as Uint8Array,
    size: c.size as BigInt64Array,
    count_: c.count as Uint32Array,
    flags: c.flags as Uint16Array,
    tag: c.tag as Uint8Array,
    tone: c.tone as Uint8Array,
    delta: (c.delta as BigInt64Array | undefined) ?? null,
    dstat: (c.dstat as Uint8Array | undefined) ?? null,
    names,
    rowOf(k: number): number {
      if (!index) {
        index = new Map();
        for (let i = 0; i < n; i++) index.set(key[i]!, i);
      }
      return index.get(k) ?? -1;
    },
  };
}

/** Deepest row under a canvas point (rows are pre-ordered: children follow their parents). */
export function hitTest(f: DrawFrame, px: number, py: number): number {
  for (let i = f.count - 1; i >= 0; i--) {
    const x = f.x[i]!;
    const y = f.y[i]!;
    if (px >= x && py >= y && px < x + f.w[i]! && py < y + f.h[i]!) return i;
  }
  return -1;
}

/** Rows from a row up to the top (excluding the row itself). */
export function ancestorsOf(f: DrawFrame, row: number): number[] {
  const out: number[] = [];
  let p = f.parent[row]!;
  while (p >= 0) {
    out.push(p);
    p = f.parent[p]!;
  }
  return out;
}

export const isSynthetic = (key: number): boolean => key >>> 30 !== 0;

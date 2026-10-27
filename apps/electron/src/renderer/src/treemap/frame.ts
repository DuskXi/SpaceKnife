// drawlist.v1 (docs/protocol.md §4.2): the treemap's frames are rectangle frames (visual/frame.ts
// decodes every layout); this module keeps the treemap's names for them and its hit test.

import type { FrameInfo } from "@/protocol/generated";
import type { BinaryFrame } from "@/protocol/rpc";
import { decodeVisualFrame, isRectFrame } from "@/visual/frame";
import type { RectFrame } from "@/visual/types";

export { ancestorsOf, DStat, Flag, isSynthetic, Kind, Label } from "@/visual/frame";

/** A frame with rectangle columns (drawlist.v1, flame.v1): the shared columns plus x y w h. */
export type DrawFrame = RectFrame;

/** Typed columns of a rectangle frame; throws on a frame of another layout. */
export function decodeDraw(frame: BinaryFrame, info: FrameInfo): DrawFrame {
  const f = decodeVisualFrame(frame, info);
  if (!f || !isRectFrame(f)) throw new Error(`not a rectangle frame: ${frame.header.layout}`);
  return f;
}

/** Deepest row under a canvas point (rows are pre-ordered: children follow their parents). */
export function hitTest(f: Pick<DrawFrame, "count" | "x" | "y" | "w" | "h">, px: number, py: number): number {
  for (let i = f.count - 1; i >= 0; i--) {
    const x = f.x[i]!;
    const y = f.y[i]!;
    if (px >= x && py >= y && px < x + f.w[i]! && py < y + f.h[i]!) return i;
  }
  return -1;
}

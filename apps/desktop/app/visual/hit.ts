// Hit-testing helpers of the view host (visual/Host in the Electron renderer), split out of the
// component so the shell (hover card, context menu, scan page) can use them without mounting it.

import type { Box, Pt } from "./types";

/** The small box a card or menu keeps clear of when it follows the pointer (CSS px). */
export const pointerBox = (p: Pt, r = 12): Box => ({ x: p.x - r, y: p.y - r, w: 2 * r, h: 2 * r });

/** A box scaled by `k` (device px → CSS px). */
export const scaled = (b: Box, k: number): Box => ({ x: b.x * k, y: b.y * k, w: b.w * k, h: b.h * k });

// hitOfKey (the row of a key) lives in visual/hover.ts, with the rest of the hovered-row logic

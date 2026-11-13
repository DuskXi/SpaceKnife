// The pure helpers of the hover cards (shell/HoverCard in the Electron renderer, also used by the
// compare card): what a hovered row is, what the card keeps clear of and where it goes.

import type { FilterInfo, FilterVocab, ViewState } from "@/protocol/generated";
import { pointerBox } from "@/visual/hit";
import { type Box, HIT_ROOT, type HoverInfo, type VisualFrame } from "@/visual/types";

/** The same target (by the row's key, so also across new pictures of the view): visual/hover.ts. */
export { sameTarget } from "@/visual/hover";

/** What a card keeps clear of: the row's box, or a small box around the pointer. */
export const cardAvoid = (h: HoverInfo): Box => (h.placement === "pointer" ? pointerBox(h.pointer) : h.avoid);

/** What a hovered row (or the zoom root) is, for the cards: one row of the frame's columns. */
export function hoverFacts(f: VisualFrame, target: number, vocab: FilterVocab | null, ext?: string) {
  const root = target === HIT_ROOT ? f.info.root : null;
  if (root) {
    // the zoom root has no class column: a file root takes the class of its extension once known
    const cls = ext === undefined ? -1 : (vocab?.classes.findIndex((c) => c.extensions.includes(ext)) ?? -1);
    const one = { kind: Uint8Array.of(root.kind), cls: Uint8Array.of(cls < 0 ? 255 : cls), tone: Uint8Array.of(root.tone), branch: Uint8Array.of(root.branch), depth: Uint8Array.of(1), delta: null };
    return { kind: root.kind, node: root.node, key: root.key, size: root.size, cls, columns: one, row: 0 };
  }
  return { kind: f.kind[target]!, node: f.node[target]!, key: f.key[target]!, size: Number(f.size[target]), cls: f.cls[target]!, columns: f, row: target };
}

/** Where the hover card goes (UI-MAIN-29): right of the box, top-aligned; else left; else below /
 * above; always inside `bounds` with a 4 px margin. */
export function placeCard(r: Box, card: { w: number; h: number }, bounds: { w: number; h: number }): { left: number; top: number } {
  const { w: cw, h: ch } = card;
  let left = r.x + r.w + 10;
  let top = r.y;
  if (left + cw > bounds.w) left = r.x - 10 - cw;
  if (left < 0) {
    left = Math.min(Math.max(0, r.x), bounds.w - cw);
    top = r.y + r.h + 10;
    if (top + ch > bounds.h) top = r.y - 10 - ch;
  }
  top = Math.max(4, Math.min(top, bounds.h - ch - 4));
  left = Math.max(4, Math.min(left, bounds.w - cw - 4));
  return { left, top };
}

/** Where the compare card goes (UI-W6-20): below right of the box's corner, flipped to stay inside. */
export function placeCompareCard(r: Box, card: { w: number; h: number }, bounds: { w: number; h: number }): { left: number; top: number } {
  let left = r.x + Math.min(r.w, 24) + 12;
  let top = r.y + Math.min(r.h, 24) + 12;
  if (left + card.w > bounds.w) left = Math.max(0, r.x - card.w - 12);
  if (top + card.h > bounds.h) top = Math.max(0, bounds.h - card.h - 8);
  return { left, top };
}

/** A translated text split at its {placeholders} (the word order is the language's): the template
 * renders `slot` parts as the named node. */
export function splitPlaceholders(text: string): { text: string; slot: boolean }[] {
  return text.split(/\{(\w+)\}/).map((part, i) => ({ text: part, slot: i % 2 === 1 }));
}

/** How much of a folder the picture leaves out because it is in hidden folders (「隐藏文件夹」 on,
 *  any in the scan): the hover card's share and counts are the whole folder's, its size the drawn
 *  part. 0 for a file, without hidden folders, and under a filter that hides (it draws its own
 *  sizes). With 「回收站」 off as well, the bin's bytes are in it too. */
export function hiddenGap(
  st: Pick<ViewState, "hide_folders" | "hidden_folders"> | undefined,
  filter: FilterInfo | null | undefined,
  dir: boolean,
  alloc: number | undefined,
  drawn: number,
): number {
  if (!st || !dir || alloc === undefined || !st.hide_folders || st.hidden_folders <= 0) return 0;
  if (filter?.active && filter.mode === "hide") return 0;
  return Math.max(0, alloc - drawn);
}

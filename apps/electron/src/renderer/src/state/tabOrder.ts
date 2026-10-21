// The order of the title bar's tabs (UI-MAIN-04): dragging a tab, or Ctrl+Shift+PgUp / PgDn, moves it.
// Pure functions over the store's tab list; the title bar measures the tabs, these decide the slot.

/** `list` with the item `id` moved so that `index` other items come before it (clamped). */
export function moveTo<T extends { id: string }>(list: readonly T[], id: string, index: number): T[] {
  const from = list.findIndex((x) => x.id === id);
  if (from < 0) return [...list];
  const rest = list.filter((x) => x.id !== id);
  const at = Math.max(0, Math.min(rest.length, index));
  if (at === from) return [...list];
  return [...rest.slice(0, at), list[from]!, ...rest.slice(at)];
}

export interface TabBox {
  id: string;
  left: number;
  width: number;
}

/** Where a dragged tab goes for a pointer at `x`: the other tabs whose middle lies left of it. */
export function dropIndex(boxes: readonly TabBox[], dragged: string, x: number): number {
  let index = 0;
  for (const b of boxes) if (b.id !== dragged && x > b.left + b.width / 2) index++;
  return index;
}

/** The slot one step left (-1) or right (+1) of the tab `id`, or null at the end of the row. */
export function stepIndex(list: readonly { id: string }[], id: string, step: -1 | 1): number | null {
  const from = list.findIndex((x) => x.id === id);
  const to = from + step;
  return from < 0 || to < 0 || to >= list.length ? null : to;
}

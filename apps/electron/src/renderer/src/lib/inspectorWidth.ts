// Width of the details column (UI-MAIN-64): dragged by its left edge, remembered per viewer.
// The stored value is the preferred width; CSS also caps the column so the map keeps `mapMin`.

import { tokens } from "@design/tokens";

const L = tokens.layout;
export const INSPECTOR_DEFAULT = L.inspector;
export const INSPECTOR_MIN = L.inspectorMin;
export const INSPECTOR_MAX = L.inspectorMax;
const KEY = "sk.inspectorWidth";

/** A width within the token range that leaves the map at least `mapMin` of `windowWidth`. */
export function clampInspector(width: number, windowWidth = Infinity): number {
  const max = Math.max(INSPECTOR_MIN, Math.min(INSPECTOR_MAX, windowWidth - L.mapMin));
  return Math.round(Math.min(max, Math.max(INSPECTOR_MIN, width)));
}

export function storedInspectorWidth(): number {
  try {
    const raw = localStorage.getItem(KEY);
    const v = raw == null ? NaN : Number(raw);
    return Number.isFinite(v) ? clampInspector(v) : INSPECTOR_DEFAULT;
  } catch {
    return INSPECTOR_DEFAULT;
  }
}

/** Remember a dragged width; the default (or null) forgets it. */
export function storeInspectorWidth(width: number | null): void {
  try {
    if (width == null || width === INSPECTOR_DEFAULT) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, String(width));
  } catch {
    /* storage unavailable: the width lasts until the window closes */
  }
}

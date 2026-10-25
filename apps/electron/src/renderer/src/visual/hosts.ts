// Mounted view hosts by view id: the shell asks a host where a row is (context menu, Shift+F10,
// hover card) instead of reading geometry columns itself.

import type { Box, Hit } from "./types";

export interface HostHandle {
  /** the host element (class "treemap", data-view-id) */
  element: HTMLElement;
  /** where a row's context menu opens and the box it keeps clear of, in client px; null when the row is not drawn */
  menuAnchor(row: Hit): { x: number; y: number; avoid: Box } | null;
  /** a row's bounds in CSS px relative to the host element; null when the row is not drawn */
  boundsCss(row: Hit): Box | null;
  /** a point (client px) where the node with this key is under the pointer (its row, or the zoom
   * root's bar, disc or shape); `null`: the zoom root itself; null when it is not drawn or covered
   * everywhere (plugin shot's click:node=… steps) */
  pointOfKey(key: number | null): { x: number; y: number } | null;
}

export const hosts = new Map<string, HostHandle>();

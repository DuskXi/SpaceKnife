// The non-visual parts of a scan tab (pages/ScanPage in the Electron renderer): the pane header's
// crumb labels, going up several levels, and opening the context menu of a picture's row.

import { fmtInt } from "@/lib/format";
import { t } from "@/lib/i18n";
import { viewCall } from "@/state/actions";
import { get, set } from "@/state/store";
import { pointerBox } from "@/visual/hit";
import { hosts } from "@/visual/hosts";
import { componentFor } from "@/visual/registry";
import { HIT_ROOT, type Hit, type VisualFrame } from "@/visual/types";

/** The details column's width for the CSS of `.inspector` (UI-MAIN-64). */
export const columnWidth = (w: number) => ({ "--inspector-w": `${w}px` });

/** A breadcrumb's label in a pane header: the root as given (without a trailing separator), a
 * folder by its last name, the merged blocks by what they hold. */
export function crumbLabel(c: { label: string }, i: number): string {
  if (c.label.startsWith("small:")) return t("{n} 个小项", { n: fmtInt(Number(c.label.slice(6))) });
  if (c.label.startsWith("more:")) return t("+{n} 项", { n: fmtInt(Number(c.label.slice(5))) });
  if (i === 0) return c.label.replace(/[\\/]+$/, "") || c.label;
  return c.label.split(/[\\/]/).filter(Boolean).pop() ?? c.label;
}

/** Up `steps` levels, one view.up after the other. */
export async function jumpUp(viewId: string, steps: number): Promise<void> {
  for (let k = 0; k < steps; k++) await viewCall(viewId, "view.up");
}

/** Right click on a block, a bar or a sector, or on the zoom root (UI-W5-01): the unified menu,
 * beside the target; where the target can be the whole disc, beside the pointer. */
export function openMenu(viewId: string, row: Hit, frame: VisualFrame, at: { clientX: number; clientY: number }): void {
  const scanId = get().views[viewId]?.state.scan_id;
  const root = frame.info.root;
  if (!scanId || (row < 0 && !(row === HIT_ROOT && root))) return;
  const key = row >= 0 ? frame.key[row]! : root!.key;
  const node = row >= 0 ? frame.node[row]! : root!.node;
  const followsPointer = componentFor(frame.layout)?.hoverPlacement === "pointer";
  const avoid = followsPointer ? pointerBox({ x: at.clientX, y: at.clientY }) : hosts.get(viewId)?.menuAnchor(row)?.avoid;
  set({ menu: { viewId, scanId, key, node: node > 0 ? node : null, x: at.clientX, y: at.clientY, avoid, generation: frame.info.generation } });
}

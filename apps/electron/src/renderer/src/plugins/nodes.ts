// A node a plugin points at (NodeRef: a report's row, a field, a cleanup candidate): shown in the
// view of its scan. The id is the plugin's; when the scan changed since (a rescan numbers its nodes
// again), the path finds the node it meant (tree.find), and an id that no longer fits says so.

import type { NodeRef } from "@/protocol/generated";
import { call, viewCall } from "@/state/actions";
import { type AppState, activeTab, focusedViewId, get, set, toast } from "@/state/store";
import { t } from "@/lib/i18n";

/** The view to show a node of `scanId` in: the focused one when it shows that scan, else the first
 * view of a tab that does (that tab is brought forward). */
export function viewFor(scanId: string, s: AppState = get()): { tabId: string; viewId: string } | null {
  const focused = focusedViewId(s);
  if (focused && s.views[focused]?.state.scan_id === scanId) return { tabId: activeTab(s).id, viewId: focused };
  for (const tab of s.tabs) {
    if (tab.kind !== "scan" || tab.compare) continue;
    const viewId = tab.viewIds.find((v) => s.views[v]?.state.scan_id === scanId);
    if (viewId) return { tabId: tab.id, viewId };
  }
  return null;
}

/** The node's id now: by path when the plugin gave one (ids change with a rescan), else its id. */
export async function resolveNode(ref: Pick<NodeRef, "scan_id" | "node" | "path">): Promise<number | null> {
  if (ref.path) {
    try {
      const found = await call("tree.find", { scan_id: ref.scan_id, path: ref.path });
      if (found.node) return found.node.id;
    } catch {
      /* the scan is gone, or no tree.find: try the id */
    }
  }
  return ref.node > 0 ? ref.node : null;
}

/** Select the node in its scan's view, navigating there (view.reveal); says why when it can't. */
export async function showNode(ref: Pick<NodeRef, "scan_id" | "node" | "path">): Promise<void> {
  const where = viewFor(ref.scan_id);
  if (!where) return toast(t("这个位置所在的扫描没有打开"));
  const node = await resolveNode(ref);
  if (node == null) return toast(t("找不到这个位置：它可能已经被删除或移动"));
  if (get().active !== where.tabId) set({ active: where.tabId });
  await viewCall(where.viewId, "view.reveal", { key: node }); // an error says itself (viewCall)
}

/** The last part of a path, for a short label. */
export const baseName = (path: string): string => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

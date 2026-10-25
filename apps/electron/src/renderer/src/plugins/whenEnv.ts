// The `when` conditions of contributions shown many at a time, outside a report's place: the
// visualizations in the switch, V / Shift+V and 「可视化：…」 (a plugin's visualization may say
// where it can be picked). Pure on the app state, so the commands and the toolbar agree.

import type { VisualInfo } from "@/protocol/generated";
import type { AppState } from "@/state/store";

import { scanSourceOf } from "./sources";
import { whenReason, type WhenEnv } from "./when";

/** The conditions' environment of a view of a scan (the selection does not matter here). */
export function viewWhenEnv(s: AppState, scanId: string | null, viewId: string | null, owner: string | null): WhenEnv {
  const scan = scanId ? s.scans[scanId] : undefined;
  return {
    scan: scan ? { state: scan.state, method: scan.method } : null,
    view: !!viewId,
    selection: viewId ? (s.views[viewId]?.state.selected ?? null) != null : false,
    compare: !!viewId && !!s.views[viewId]?.state.compare,
    setting: (key) => s.settings?.values[key],
    capabilities: s.capabilities,
    plugins: (s.extensions?.plugins ?? []).filter((p) => p.state === "enabled").map((p) => p.id),
    owner,
    sourceId: scanId ? scanSourceOf(scanId) : null,
  };
}

/** Why each visualization can't be picked in this view by its own conditions (null: it can). */
export function visualWhen(s: AppState, scanId: string | null, viewId: string | null): (info: VisualInfo) => string | null {
  return (info) => (info.when?.length ? whenReason(info.when, viewWhenEnv(s, scanId, viewId, info.plugin ?? null)) : null);
}

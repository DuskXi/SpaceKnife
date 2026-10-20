// Which visualizations a view can switch to (toolbar, V / Shift+V, 「切换可视化」) and the C key's
// colour cycle. Pure: the backend's visual.list, the layouts this renderer registered and whether
// the view is a comparison go in; nothing here reads the store or the registry.

import { icons } from "@design/icons";

import type { FrameInfo, ViewState, VisualInfo } from "@/protocol/generated";
import { t } from "@/lib/i18n";
import type { IconName } from "@/ui/Icon";

export type ColorModeName = ViewState["color_mode"];

/** The C key's order: class → flat → time → branch → class. */
export const COLOR_MODES = ["class", "flat", "time", "branch"] as const satisfies readonly ColorModeName[];

export interface Offered {
  info: VisualInfo;
  enabled: boolean;
  /** why it can't be picked here (display text); null when enabled */
  reason: string | null;
}

/** The backend's visualizations in its order, each enabled only when the session has it, this
 * renderer has a component for its layout and, in a comparison, it can show changes. Nothing the
 * backend offers is hidden: what can't be drawn here stays visible with its reason. */
export function offered(list: readonly VisualInfo[], layouts: readonly string[], compare: boolean): Offered[] {
  return [...list]
    .sort((a, b) => a.order - b.order)
    .map((info) => {
      const reason = !info.available
        ? info.reason || t("这个界面画不了这种可视化")
        : !layouts.includes(info.layout)
          ? t("这个界面画不了这种可视化")
          : compare && !info.compare
            ? t("对比视图里不能用这种可视化")
            : null;
      return { info, enabled: reason === null, reason };
    });
}

/** The visualization after (dir 1) or before (dir -1) `current` among the enabled ones, wrapping
 * around; null when there is no other one to go to. A current one that is not offered (or not
 * enabled) steps to the first enabled one in that direction from where it stands in the list. */
export function nextVisual(list: readonly VisualInfo[], layouts: readonly string[], current: string, dir: 1 | -1, compare: boolean): string | null {
  const items = offered(list, layouts, compare);
  const n = items.length;
  const at = items.findIndex((o) => o.info.id === current);
  const start = at >= 0 ? at : dir === 1 ? n - 1 : 0;
  for (let step = 1; step <= n; step++) {
    const o = items[(((start + dir * step) % n) + n) % n]!;
    if (o.enabled) return o.info.id === current ? null : o.info.id;
  }
  return null;
}

/** The C key: the next of the four colour modes. */
export function nextColor(current: ColorModeName): ColorModeName {
  return COLOR_MODES[(COLOR_MODES.indexOf(current) + 1) % COLOR_MODES.length]!;
}

/** Levels the frame's geometry has room for when that is fewer than `detail` (flame.v1 bands,
 * polar.v1 rings); null when every level fits or the layout has no such limit (drawlist.v1). */
export function levelsThatFit(info: FrameInfo | null | undefined, detail: number): number | null {
  const g = info?.geometry;
  if (!g) return null;
  const n = g.kind === "flame" ? g.rows : g.rings;
  return n < detail ? n : null;
}

/** Icon of a visual.list entry: icon keys are camelCase ("vizFlamePolar"), a kebab-case name
 * ("viz-flame-polar") is accepted too; one the icon set does not have falls back to a neutral one. */
export function visualIcon(icon: string): IconName {
  const name = icon.replace(/-([a-z0-9])/g, (_, c: string) => c.toUpperCase());
  return name in icons ? (name as IconName) : "layers";
}

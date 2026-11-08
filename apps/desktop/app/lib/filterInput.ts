// The filter box's input ids (shell/FilterBox): one filter box per view (split panes each have their
// own, W8). Kept apart from the component so commands and shortcuts can focus it.

/** One filter box per view (split panes each have their own, W8). */
export const filterInputId = (viewId: string | null) => `sk-filter-${viewId ?? "none"}`;

/** Focus the filter of the focused view (/ and Ctrl+F, 「聚焦过滤」). */
export function focusFilter(viewId: string | null): void {
  (document.getElementById(filterInputId(viewId)) as HTMLInputElement | null)?.focus();
}

// How often the details panel refreshes a subtree summary. The throttled value itself is a Vue
// composable: composables/useThrottled.ts.

const SMALL_SUBTREE = 200_000; // nodes: a summary this size takes a few ms, refresh it at once
const MS_PER_MILLION = 1500; // a summary walks ~10 million nodes a second: at most ~7% of a core
const MAX_SUMMARY_WAIT = 10_000;

/**
 * How long the details panel waits between refreshes of a subtree summary of `items` nodes
 * (files plus folders) as frames come in: every refresh walks the whole subtree, and live
 * monitoring of a busy disk changes something every few seconds. Small subtrees refresh at once
 * (once a second while the scan runs); large ones about every 1.5 s per million nodes, at most
 * every 10 s. A new selection is always fetched at once.
 */
export function summaryInterval(items: number, running: boolean): number {
  const floor = running ? 1000 : 0;
  if (!(items >= SMALL_SUBTREE)) return floor;
  return Math.min(MAX_SUMMARY_WAIT, Math.max(1000, Math.round((items / 1e6) * MS_PER_MILLION)));
}

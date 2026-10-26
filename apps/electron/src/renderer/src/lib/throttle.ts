import { useEffect, useRef, useState } from "react";

/**
 * `value`, updated at most once per `ms` (the latest value wins, nothing is lost at the end).
 * `ms <= 0` passes every change through. Used for refetches keyed on the frame sequence: frames
 * come four times a second while a scan runs, and every refetch walks the whole scan.
 */
export function useThrottled<T>(value: T, ms: number): T {
  const [shown, setShown] = useState(value);
  const last = useRef(0);
  useEffect(() => {
    const wait = ms <= 0 ? 0 : last.current + ms - Date.now();
    if (wait <= 0) {
      last.current = Date.now();
      setShown(value);
      return;
    }
    const t = setTimeout(() => {
      last.current = Date.now();
      setShown(value);
    }, wait);
    return () => clearTimeout(t);
  }, [value, ms]);
  return shown;
}

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

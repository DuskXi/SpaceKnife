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

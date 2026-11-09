// `value`, updated at most once per `ms` (the latest value wins, nothing is lost at the end).
// `ms <= 0` passes every change through. Used for refetches keyed on the frame sequence: frames
// come four times a second while a scan runs, and every refetch walks the whole scan.
// (React's useThrottled in the Electron renderer; lib/throttle.ts keeps summaryInterval.)

import { type MaybeRefOrGetter, onScopeDispose, type Ref, shallowRef, toValue, watch } from "vue";

export function useThrottled<T>(value: MaybeRefOrGetter<T>, ms: MaybeRefOrGetter<number>): Readonly<Ref<T>> {
  const shown = shallowRef(toValue(value)) as Ref<T>;
  // the first value counts as shown now, as the React hook's mount effect did
  let last = Date.now();
  let timer: ReturnType<typeof setTimeout> | null = null;
  const clear = () => {
    if (timer) clearTimeout(timer);
    timer = null;
  };
  watch([() => toValue(value), () => toValue(ms)] as const, ([v, gap]) => {
    clear();
    const wait = gap <= 0 ? 0 : last + gap - Date.now();
    if (wait <= 0) {
      last = Date.now();
      shown.value = v;
      return;
    }
    timer = setTimeout(() => {
      timer = null;
      last = Date.now();
      shown.value = v;
    }, wait);
  });
  onScopeDispose(clear);
  return shown;
}

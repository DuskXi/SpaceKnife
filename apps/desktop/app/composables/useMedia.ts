// The media queries drawing depends on (visual/scheduler.ts), followed when they change:
// 「减少动画」 and the device pixel ratio. (React hooks in the Electron renderer.)

import { computed, type ComputedRef, type MaybeRefOrGetter, onScopeDispose, type Ref, ref, toValue } from "vue";

const media = (query: string): MediaQueryList | null => (typeof window !== "undefined" && window.matchMedia ? window.matchMedia(query) : null);

/** 「减少动画」: on, off, or the system's setting, followed when the system's changes. */
export function useReducedMotion(setting: MaybeRefOrGetter<string>): ComputedRef<boolean> {
  const mq = media("(prefers-reduced-motion: reduce)");
  const system = ref(mq?.matches ?? false);
  if (mq) {
    const on = () => (system.value = mq.matches);
    mq.addEventListener("change", on);
    onScopeDispose(() => mq.removeEventListener("change", on));
  }
  return computed(() => {
    const s = toValue(setting);
    return s === "on" || (s === "system" && system.value);
  });
}

/** window.devicePixelRatio, followed when it changes without a resize (another monitor, zoom). */
export function useDevicePixelRatio(): Readonly<Ref<number>> {
  const dpr = ref(typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1);
  if (typeof window === "undefined") return dpr;
  let mq: MediaQueryList | null = null;
  const watchRatio = () => {
    // a query for the current ratio stops matching when the ratio changes
    mq = media(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
    mq?.addEventListener("change", on);
  };
  const on = () => {
    mq?.removeEventListener("change", on);
    dpr.value = window.devicePixelRatio || 1;
    watchRatio();
  };
  watchRatio();
  onScopeDispose(() => mq?.removeEventListener("change", on));
  return dpr;
}

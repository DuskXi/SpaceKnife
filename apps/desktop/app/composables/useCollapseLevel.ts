// The collapse level the toolbar needs now (lib/toolbarFit.ts; React's useCollapseLevel in the
// Electron renderer). `content` names what it shows (tab, language, path, chips…): when it changes,
// and when the window gets wider or narrower, the level starts over at 0 and climbs while the toolbar
// doesn't fit. Each step is measured after Vue patched the DOM and before the browser paints (a
// microtask), so no frame shows a clipped toolbar. A part that grows on its own (a label loading)
// only makes it climb.

import { type MaybeRefOrGetter, nextTick, onMounted, onScopeDispose, type Ref, ref, toValue, watch } from "vue";

import { MAX_LEVEL, neededWidth } from "@/lib/toolbarFit";

export function useCollapseLevel(bar: Readonly<Ref<HTMLElement | null>>, content: MaybeRefOrGetter<string>): Readonly<Ref<number>> {
  const level = ref(0);
  let climbing = false;

  async function refit(reset: boolean): Promise<void> {
    if (reset) level.value = 0;
    if (climbing) return; // the running climb measures again after this change
    climbing = true;
    try {
      await nextTick();
      for (;;) {
        const el = bar.value;
        if (!el || level.value >= MAX_LEVEL || !(neededWidth(el) > el.clientWidth + 0.5)) break;
        level.value++;
        await nextTick();
      }
    } finally {
      climbing = false;
    }
  }

  // new content: start over at 0 before it renders (flush "pre"), then climb
  watch(
    () => toValue(content),
    () => void refit(true),
  );

  let observer: ResizeObserver | null = null;
  const observe = () => {
    observer?.disconnect();
    observer = null;
    const el = bar.value;
    if (!el || typeof ResizeObserver === "undefined") return;
    let width = el.clientWidth;
    observer = new ResizeObserver((entries) => {
      if (entries.some((e) => e.target === el) && el.clientWidth !== width) {
        width = el.clientWidth;
        void refit(true);
      } else void refit(false);
    });
    observer.observe(el);
    for (const child of Array.from(el.children)) observer.observe(child);
  };
  // the parts change with the content: observe the new ones
  watch([bar, () => toValue(content)], observe, { flush: "post" });

  onMounted(() => {
    observe();
    void refit(true);
    // web fonts arriving change every label's width
    void document.fonts?.ready.then(() => refit(true));
  });
  onScopeDispose(() => observer?.disconnect());
  return level;
}

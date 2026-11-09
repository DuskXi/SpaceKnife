// What a hover card shows (shell/HoverCard and the compare card): the hovered target after a
// delay; once a card is open (「保持打开」) moving between blocks switches it in place, and the same
// target (a card that follows the pointer) moves it at once.

import { type MaybeRefOrGetter, type Ref, shallowRef, toValue, watch } from "vue";

import { sameTarget } from "@/lib/hoverCard";
import type { HoverInfo } from "@/visual/types";

export function useHoverCard(hover: MaybeRefOrGetter<HoverInfo | null>, keepOpen: MaybeRefOrGetter<boolean>, delay: number): Readonly<Ref<HoverInfo | null>> {
  // the hover carries a frame (typed arrays): kept raw
  const shown = shallowRef<HoverInfo | null>(null);
  watch(
    [() => toValue(hover), () => toValue(keepOpen)] as const,
    ([h, keep], _old, onCleanup) => {
      if (shown.value === h) return;
      if (!h) {
        shown.value = null;
        return;
      }
      if ((keep || sameTarget(shown.value, h)) && shown.value) {
        shown.value = h; // moving between blocks (or with the pointer): the open card switches in place
        return;
      }
      shown.value = null;
      const timer = setTimeout(() => (shown.value = h), delay);
      onCleanup(() => clearTimeout(timer));
    },
    { immediate: true },
  );
  return shown;
}

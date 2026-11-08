<!-- 「本页内容」: the help page's ## and ### headings; the one being read is marked (followed while
     the page scrolls). -->
<script setup lang="ts">
import { ref, watch } from "vue";

import type { Heading } from "@/help/markdown";
import { t } from "@/lib/i18n";
import { helpGo } from "@/state/help";

/** A heading counts as reached while it is this far below the top of the page's scroll box. */
const SPY_OFFSET = 96;

const props = defineProps<{ docId: string; toc: Heading[]; scroller: HTMLElement | null }>();
const current = ref<string | null>(null);

watch(
  [() => props.toc, () => props.scroller],
  ([toc, el], _, onCleanup) => {
    if (!el) return;
    let raf = 0;
    const spy = () => {
      raf = 0;
      const top = el.getBoundingClientRect().top + SPY_OFFSET;
      let at: string | null = null;
      for (const h of toc) {
        const node = el.querySelector<HTMLElement>(`[id="${CSS.escape(h.id)}"]`);
        if (node && node.getBoundingClientRect().top <= top) at = h.id;
        else if (node) break;
      }
      // at the very end the last heading may never reach the top: it is the one being read
      if (el.scrollTop + el.clientHeight >= el.scrollHeight - 2) at = toc[toc.length - 1]!.id;
      current.value = at;
    };
    const onScroll = () => void (raf ||= requestAnimationFrame(spy));
    spy();
    el.addEventListener("scroll", onScroll, { passive: true });
    onCleanup(() => {
      el.removeEventListener("scroll", onScroll);
      cancelAnimationFrame(raf);
    });
  },
  { immediate: true, flush: "post" },
);
</script>

<template>
  <aside class="help-toc" :aria-label="t('本页内容')">
    <div class="help-toc-title">{{ t("本页内容") }}</div>
    <button
      v-for="h in toc"
      :key="h.id"
      :class="['help-toc-item', `d${h.depth}`, { on: current === h.id }]"
      :aria-current="current === h.id ? 'location' : undefined"
      @click="helpGo(docId, h.id)"
    >
      {{ h.text }}
    </button>
  </aside>
</template>

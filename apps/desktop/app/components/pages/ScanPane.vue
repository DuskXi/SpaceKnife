<!-- One pane of a scan tab (Pane / ComparePane in pages/ScanPage.tsx): the picture, the filter
     floater and the hover card; in a split view (W8) its own header. -->
<script setup lang="ts">
import { onMounted, onScopeDispose, ref, shallowRef } from "vue";

import ScanFilterFloater from "@/components/pages/ScanFilterFloater.vue";
import ScanPaneHead from "@/components/pages/ScanPaneHead.vue";
import CompareHoverCard from "@/components/shell/CompareHoverCard.vue";
import HoverCard from "@/components/shell/HoverCard.vue";
import Host from "@/components/visual/Host.vue";
import { openMenu } from "@/lib/scanPage";
import type { Tab } from "@/state/store";
import type { Hit, HoverInfo, VisualFrame } from "@/visual/types";

const props = withDefaults(defineProps<{ viewId: string; tab?: Tab; index?: number; split?: boolean; focused?: boolean; compare?: boolean }>(), {
  tab: undefined,
  index: 0,
  split: false,
  focused: false,
  compare: false,
});
const emit = defineEmits<{ focus: [] }>();

// the hover carries the frame (typed arrays): kept raw
const hover = shallowRef<HoverInfo | null>(null);
const map = ref<HTMLDivElement | null>(null);
const bounds = ref({ w: 0, h: 0 });
let ro: ResizeObserver | null = null;
onMounted(() => {
  const el = map.value;
  if (!el || typeof ResizeObserver === "undefined") return;
  ro = new ResizeObserver(() => (bounds.value = { w: el.clientWidth, h: el.clientHeight }));
  ro.observe(el);
});
onScopeDispose(() => ro?.disconnect());

const onMenu = (row: Hit, frame: VisualFrame, ev: MouseEvent) => openMenu(props.viewId, row, frame, ev);
</script>

<template>
  <section v-if="compare" class="pane">
    <div ref="map" class="pane-map">
      <Host :view-id="viewId" @hover="hover = $event" @menu="onMenu" />
      <ScanFilterFloater :view-id="viewId" />
      <CompareHoverCard :hover="hover" :bounds="bounds" />
    </div>
  </section>
  <section v-else :class="['pane', { focused: focused && split }]" @mousedown.capture="emit('focus')">
    <ScanPaneHead v-if="split && tab" :tab="tab" :view-id="viewId" :index="index" />
    <div ref="map" class="pane-map">
      <Host :view-id="viewId" @hover="hover = $event" @menu="onMenu" />
      <ScanFilterFloater :view-id="viewId" />
      <HoverCard :hover="hover" :bounds="bounds" />
    </div>
  </section>
</template>

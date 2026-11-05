<!-- Visualization switch (V / Shift+V): an icon segment for each visualization the backend offers.
     One this renderer or this view can't show stays in place, disabled, with the reason as its
     tooltip. Switching keeps the zoom, the selection, the filter and the history (they live in the
     backend's view). Nothing without visual.list: the treemap only. -->
<script setup lang="ts">
import { computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import type { ViewState } from "@/protocol/generated";
import { pickVisual } from "@/state/commands";
import { useApp } from "@/state/store";
import { offered, visualIcon } from "@/state/visuals";
import { registeredLayouts } from "@/visual/registry";

const props = defineProps<{ viewId: string; st: ViewState }>();
const visuals = useApp((s) => s.visuals);
const items = computed(() => offered(visuals.value, registeredLayouts(), props.st.compare));
</script>

<template>
  <div v-if="items.length" class="segmented icon-only" role="radiogroup" :aria-label="t('可视化')" :title="t('可视化（V）')">
    <button
      v-for="{ info, enabled, reason } in items"
      :key="info.id"
      role="radio"
      :aria-checked="st.visual === info.id"
      :aria-label="info.name"
      :class="{ on: st.visual === info.id }"
      :disabled="!enabled"
      :title="reason ?? info.name"
      @click="pickVisual(viewId, info.id)"
    >
      <Icon :name="visualIcon(info.icon)" :size="14" />
    </button>
  </div>
</template>

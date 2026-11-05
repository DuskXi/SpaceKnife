<!-- 「显示的变化：全部｜增长｜减少」 (UI-W6-07); DirectionSegment in shell/Compare.tsx. -->
<script setup lang="ts">
import { DIRECTIONS, directionLabel } from "@/lib/compare";
import { t } from "@/lib/i18n";
import { setViewSettings } from "@/state/actions";
import { useApp } from "@/state/store";

const props = defineProps<{ viewId: string }>();
const dir = useApp((s) => s.views[props.viewId]?.state.direction ?? "all");
</script>

<template>
  <div class="segmented" role="radiogroup" :aria-label="t('显示的变化')" :title="t('显示的变化（C）')">
    <button v-for="d in DIRECTIONS" :key="d" role="radio" :aria-checked="dir === d" :class="dir === d ? 'on' : ''" @click="setViewSettings(viewId, { direction: d })">
      {{ directionLabel(d) }}
    </button>
  </div>
</template>

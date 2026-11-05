<!-- A sparkline of `values` (oldest first), scaled to their own range; the last point marked. Nothing
     for fewer than two values. -->
<script setup lang="ts">
import { computed } from "vue";

import { sparkPoints } from "@/lib/charts";

const props = withDefaults(defineProps<{ values: number[]; width?: number; height?: number; title?: string }>(), { width: 96, height: 26, title: undefined });
const line = computed(() => sparkPoints(props.values, props.width, props.height));
</script>

<template>
  <svg v-if="line" class="sparkline" :width="width" :height="height" :viewBox="`0 0 ${width} ${height}`" role="img" :aria-label="title">
    <title v-if="title">{{ title }}</title>
    <polyline :points="line.points" fill="none" stroke="var(--sk-color-accent-default)" :stroke-width="2" stroke-linejoin="round" stroke-linecap="round" />
    <circle :cx="line.last.x" :cy="line.last.y" :r="3" fill="var(--sk-color-accent-default)" stroke="var(--sk-color-bg-panel)" :stroke-width="2" />
  </svg>
</template>

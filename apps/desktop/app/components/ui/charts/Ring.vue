<!-- A ring meter: `share` (0-1) of the circle, from the top, clockwise; the slot in the middle. -->
<script setup lang="ts">
import { computed, useSlots } from "vue";

import { clampShare, ringArc, type Tone, TONE } from "@/lib/charts";

const props = withDefaults(defineProps<{ share: number; size?: number; stroke?: number; tone?: Tone; title?: string }>(), {
  size: 44,
  stroke: 5,
  tone: "accent",
  title: undefined,
});
const slots = useSlots();
const arc = computed(() => ringArc(props.share, props.size, props.stroke));
</script>

<template>
  <span class="ring" :style="{ width: `${size}px`, height: `${size}px` }" :title="title" role="img" :aria-label="title">
    <svg :width="size" :height="size" :viewBox="`0 0 ${size} ${size}`" aria-hidden="true">
      <circle :cx="size / 2" :cy="size / 2" :r="arc.r" fill="none" stroke="var(--sk-color-line-subtle)" :stroke-width="stroke" />
      <circle
        v-if="clampShare(share) > 0"
        :cx="size / 2"
        :cy="size / 2"
        :r="arc.r"
        fill="none"
        :stroke="TONE[tone]"
        :stroke-width="stroke"
        stroke-linecap="round"
        :stroke-dasharray="arc.dash"
        :transform="`rotate(-90 ${size / 2} ${size / 2})`"
      />
    </svg>
    <span v-if="slots.default" class="ring-label"><slot /></span>
  </span>
</template>

<!-- Part-to-whole: segments by share, 2px apart; what is left of the whole stays track. -->
<script setup lang="ts">
import type { Segment } from "@/lib/charts";

withDefaults(defineProps<{ segments: Segment[]; height?: number; title?: string }>(), { height: 10, title: undefined });
</script>

<template>
  <span class="stackbar" :style="{ height: `${height}px` }" role="img" :aria-label="title">
    <span v-for="s in segments.filter((x) => x.share > 0)" :key="s.key" :style="{ flexBasis: `${s.share * 100}%`, background: s.color }" :title="s.title" />
  </span>
</template>

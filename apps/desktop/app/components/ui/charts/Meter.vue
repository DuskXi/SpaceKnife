<!-- A bar meter: `share` (0-1) of the track. -->
<script setup lang="ts">
import { clampShare, type Tone, TONE } from "@/lib/charts";

withDefaults(defineProps<{ share: number; tone?: Tone; title?: string }>(), { tone: "accent", title: undefined });
</script>

<template>
  <span class="meter" :title="title" role="img" :aria-label="title">
    <span :style="{ width: `${clampShare(share) * 100}%`, background: TONE[tone] }" />
  </span>
</template>

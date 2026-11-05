<!-- One icon of the design's set (design/build/icons.ts), drawn in the current text colour. -->
<script setup lang="ts">
import { icons, iconStrokeWidth, iconViewBox } from "@design/icons";

import type { IconName } from "@/lib/icons";

withDefaults(defineProps<{ name: IconName; size?: number; strokeWidth?: number }>(), { size: 16, strokeWidth: undefined });
</script>

<template>
  <!-- the icon set's own markup -->
  <svg
    :width="size"
    :height="size"
    :viewBox="iconViewBox"
    fill="none"
    stroke="currentColor"
    :stroke-width="strokeWidth ?? iconStrokeWidth"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
    v-html="icons[name]"
  />
</template>

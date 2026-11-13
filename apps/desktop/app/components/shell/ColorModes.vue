<!-- The toolbar's colour modes 「类别｜扁平｜时间｜分支」 (UI-MAIN-20). Hovering or focusing one shows its
     legend (ColorLegendTip): after a short pause the first time, at once while moving between them.
     A right-click opens the settings where that mode's colours are set (lib/colorLegend.ts). -->
<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { legendSettings } from "@/lib/colorLegend";
import { t } from "@/lib/i18n";
import { colorLabel } from "@/lib/toolbar";
import type { ViewState } from "@/protocol/generated";
import { openSettingsTab, setViewSettings } from "@/state/actions";
import { COLOR_MODES, type ColorModeName } from "@/state/visuals";

import ColorLegendTip from "./ColorLegendTip.vue";

defineProps<{ viewId: string; st: ViewState }>();

/** before the first legend shows; and how long one stays after the pointer left, so that moving on
 * to the next mode changes it instead of closing and opening it again */
const SHOW_DELAY = 350;
const HIDE_DELAY = 120;
const legendMode = ref<ColorModeName | null>(null);
let timer: ReturnType<typeof setTimeout> | undefined;

function show(m: ColorModeName, now = false): void {
  clearTimeout(timer);
  if (now || legendMode.value) legendMode.value = m;
  else timer = setTimeout(() => (legendMode.value = m), SHOW_DELAY);
}

function hide(now = false): void {
  clearTimeout(timer);
  if (now || !legendMode.value) legendMode.value = null;
  else timer = setTimeout(() => (legendMode.value = null), HIDE_DELAY);
}

function colorSettings(m: ColorModeName): void {
  hide(true);
  openSettingsTab(legendSettings(m));
}

onBeforeUnmount(() => clearTimeout(timer));
</script>

<template>
  <div class="legend-anchor">
    <div class="segmented" role="radiogroup" :aria-label="t('配色')">
      <span class="seg-icon"><Icon name="palette" :size="14" /></span>
      <button
        v-for="m in COLOR_MODES"
        :key="m"
        role="radio"
        :aria-checked="st.color_mode === m"
        :aria-describedby="legendMode === m ? 'sk-color-legend' : undefined"
        :class="{ on: st.color_mode === m }"
        @click="setViewSettings(viewId, { color_mode: m })"
        @contextmenu.prevent="colorSettings(m)"
        @mouseenter="show(m)"
        @mouseleave="hide()"
        @focus="show(m, true)"
        @blur="hide()"
        @keydown.esc="hide(true)"
      >
        {{ colorLabel(m) }}
      </button>
    </div>
    <ColorLegendTip v-if="legendMode" id="sk-color-legend" :mode="legendMode" :view-id="viewId" />
  </div>
</template>

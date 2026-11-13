<!-- Legend of a colour mode (UI-MAIN-20): under the toolbar's 「类别｜扁平｜时间｜分支」 while one of
     them is hovered or has the focus, for the mode hovered (lib/colorLegend.ts). -->
<script setup lang="ts">
import { computed } from "vue";

import Kbd from "@/components/ui/Kbd.vue";
import { colorLegend } from "@/lib/colorLegend";
import { plural } from "@/lib/format";
import { t } from "@/lib/i18n";
import { pageName } from "@/lib/settingsPages";
import { useApp } from "@/state/store";
import type { ColorModeName } from "@/state/visuals";

const props = defineProps<{ mode: ColorModeName; viewId: string }>();

const vocab = useApp((s) => s.vocab);
// only 「分支」 reads the picture (its branches); the others don't follow every frame
const frame = useApp((s) => (props.mode === "branch" ? (s.views[props.viewId]?.frame ?? null) : null));
const theme = useApp((s) => s.theme);
const legend = computed(() => (void theme.value, colorLegend(props.mode, vocab.value, frame.value)));
</script>

<template>
  <div class="legend-tip" role="tooltip">
    <div class="lt-title">{{ legend.title }}</div>
    <div class="lt-lead muted">{{ legend.lead }}</div>
    <div v-if="legend.rows.length" :class="['lt-rows', { two: legend.rows.length > 7 }]">
      <div v-for="(r, i) in legend.rows" :key="i" :class="['lt-row', { wide: r.wide }]">
        <span class="swatch" :style="{ background: r.color }" />
        <span class="lt-label">{{ r.label }}</span>
      </div>
    </div>
    <div v-if="legend.more" class="lt-note muted">{{ plural(legend.more, t("还有 {n} 个分支", { n: legend.more }), "1 more branch") }}</div>
    <div v-for="(n, i) in legend.notes" :key="`n${i}`" class="lt-note muted">{{ n }}</div>
    <div class="lt-hint">
      <span>{{ t("右键打开「{page}」设置", { page: pageName(legend.settings) }) }}</span>
      <span>·</span>
      <Kbd>C</Kbd>
      <span>{{ t("切换配色") }}</span>
    </div>
  </div>
</template>

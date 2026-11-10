<!-- 对比选择器 (UI-W6-03…06): 「对比」 · base ▾ › target ▾ · swap. The two ends of a comparison, in
     the toolbar. In a narrow window it shrinks (the snapshot names get an ellipsis) down to 260 px;
     `compact` leaves out the word 「对比」. -->
<script setup lang="ts">
import CompareEndMenu from "@/components/shell/CompareEndMenu.vue";
import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { swapCompare } from "@/state/compare";
import type { Tab } from "@/state/store";

withDefaults(defineProps<{ tab: Tab; compact?: boolean }>(), { compact: false });
</script>

<template>
  <div class="cmp-picker" role="group" :aria-label="t('快照对比')" :data-fit-min="260">
    <span class="cmp-label" :title="compact ? t('对比') : undefined">
      <Icon name="diff" :size="14" />
      <template v-if="!compact">{{ t("对比") }}</template>
    </span>
    <CompareEndMenu :tab="tab" which="base" />
    <span class="cmp-arrow" aria-hidden="true">
      <Icon name="chevronRight" :size="12" />
    </span>
    <CompareEndMenu :tab="tab" which="target" />
    <span class="cmp-vsep" />
    <button
      class="icon-btn small cmp-swap"
      :aria-label="t('交换基准与对比目标')"
      :title="t('交换基准与对比目标')"
      :disabled="tab.compare!.phase !== 'ready' && !tab.compare!.shownId"
      @click="swapCompare(tab.id)"
    >
      <Icon name="diff" :size="14" />
    </button>
  </div>
</template>

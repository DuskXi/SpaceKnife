<!-- 「退出对比」 (ExitCompare in shell/Compare.tsx); `compact` (a narrow window): the icon alone, the
     text stays its name and tooltip. -->
<script setup lang="ts">
import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { exitCompare } from "@/state/compare";
import type { Tab } from "@/state/store";

withDefaults(defineProps<{ tab: Tab; compact?: boolean }>(), { compact: false });
</script>

<template>
  <button :class="['outline-btn', { 'icon-only': compact }]" :aria-label="t('退出对比')" :title="t('退出对比（Esc）')" @click="exitCompare(tab.id)">
    <Icon name="close" :size="14" />
    <template v-if="!compact">{{ t("退出对比") }}</template>
  </button>
</template>

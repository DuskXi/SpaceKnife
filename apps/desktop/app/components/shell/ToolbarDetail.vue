<!-- Detail level stepper (Ctrl+- / Ctrl+=). The flame graph and the polar flame graph may have room for
     fewer levels than asked for: the stepper then says how many are drawn. `compact` (a narrow window)
     leaves out the word 「层级」; the group's name and the buttons' tooltips still say it. -->
<script setup lang="ts">
import { computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import type { ViewState } from "@/protocol/generated";
import { setViewSettings } from "@/state/actions";
import { useApp } from "@/state/store";
import { levelsThatFit } from "@/state/visuals";

const props = defineProps<{ viewId: string; st: ViewState; compact: boolean }>();
const info = useApp((s) => s.views[props.viewId]?.info);
const fitText = computed(() => {
  const fit = levelsThatFit(info.value, props.st.detail);
  return fit == null ? null : t("画得下 {n} 层", { n: fit });
});
</script>

<template>
  <div class="stepper" role="group" :aria-label="t('细节层级')">
    <button :aria-label="t('降低细节层级（Ctrl+-）')" :title="t('降低细节层级（Ctrl+-）')" :disabled="st.detail <= 1" @click="setViewSettings(viewId, { detail: st.detail - 1 })">
      <Icon name="minus" :size="14" />
    </button>
    <div class="stepper-mid" :title="fitText ?? undefined">
      <Icon name="layers" :size="14" />
      <span v-if="!compact" class="muted">{{ t("层级") }}</span>
      <span class="num">{{ st.detail }}</span>
      <span v-if="fitText" class="muted">{{ fitText }}</span>
    </div>
    <button :aria-label="t('提高细节层级（Ctrl+=）')" :title="t('提高细节层级（Ctrl+=）')" :disabled="st.detail >= 15" @click="setViewSettings(viewId, { detail: st.detail + 1 })">
      <Icon name="plus" :size="14" />
    </button>
  </div>
</template>

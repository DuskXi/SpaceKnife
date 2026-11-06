<!-- Docked log panel (W8, UI-W8-20…33): events with level, source and message; locate / copy the
     path of a line (LogList.vue); follows the newest line while scrolled to the bottom; a scan
     statistics page (LogStats.vue). Closed, an empty dock keeps the grid's row. -->
<script setup lang="ts">
import { computed, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { fmtInt } from "@/lib/format";
import { t } from "@/lib/i18n";
import { updateSettings } from "@/state/actions";
import { set, useApp } from "@/state/store";

import LogList from "./LogList.vue";
import LogStats from "./LogStats.vue";

const open = useApp((s) => s.logOpen);
const tab = useApp((s) => s.logTab);
const logs = useApp((s) => s.logs);
const auto = useApp((s) => s.logAuto);
const onlyWarnings = ref(false);
watch(open, (now) => !now && (onlyWarnings.value = false)); // a fresh panel each time it opens, as before
const warnings = computed(() => logs.value.filter((l) => l.level !== "INFO").length);
const shown = computed(() => (onlyWarnings.value ? logs.value.filter((l) => l.level !== "INFO") : logs.value));
</script>

<template>
  <section v-if="open" class="logdock open" :aria-label="t('日志面板')">
    <div class="log-head">
      <div class="segtabs small" role="tablist" :aria-label="t('停靠面板')">
        <button role="tab" :aria-selected="tab === 'log'" :class="{ on: tab === 'log' }" @click="set({ logTab: 'log' })">
          <Icon name="log" :size="14" />
          {{ t("日志") }}
        </button>
        <button role="tab" :aria-selected="tab === 'stats'" :class="{ on: tab === 'stats' }" @click="set({ logTab: 'stats' })">
          {{ t("扫描统计") }}
        </button>
      </div>
      <span class="log-summary">
        {{ t("本次会话 · {n} 条 ·", { n: fmtInt(logs.length) }) }}
        <button
          :class="['link', { on: onlyWarnings }]"
          :aria-pressed="onlyWarnings"
          :title="onlyWarnings ? t('显示全部事件') : t('只看警告和错误')"
          @click="onlyWarnings = !onlyWarnings"
        >
          {{ t("{n} 条警告", { n: fmtInt(warnings) }) }}
        </button>
      </span>
      <span class="flex" />
      <label class="switch-row">
        <span>{{ t("弹出事件时自动打开") }}</span>
        <button role="switch" :aria-checked="auto" :class="['switch', { on: auto }]" @click="updateSettings({ 'general.log_auto_open': !auto })">
          <span />
        </button>
      </label>
      <span class="vsep" />
      <button class="ghost small" @click="set({ logs: [] })">{{ t("清空") }}</button>
      <button class="icon-btn small" :aria-label="t('收起日志面板')" :title="t('收起日志面板（Ctrl+J）')" @click="set({ logOpen: false, logDismissed: true })">
        <Icon name="chevronDown" />
      </button>
    </div>
    <LogList v-if="tab === 'log'" :logs="shown" />
    <LogStats v-else />
  </section>
  <div v-else class="logdock" />
</template>

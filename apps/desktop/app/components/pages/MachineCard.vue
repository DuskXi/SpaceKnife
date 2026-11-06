<!-- The start page's card for 「本机磁盘」: the disks, their capacity by media and what is free, the
     fullest volume, the fastest link and how many findings there are; a click opens the page. It reads
     machine.disks with the start page (at most once a minute) and never pops up or animates. -->
<script setup lang="ts">
import { computed, watch } from "vue";

import StackBar from "@/components/ui/charts/StackBar.vue";
import Icon from "@/components/ui/Icon.vue";
import { fmtShare, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { openMachineTab, refreshMachine } from "@/state/actions";
import { byMedia, fastest, findings, fullest, linkText, totals } from "@/state/machine";
import { useApp } from "@/state/store";

const conn = useApp((s) => s.conn);
const data = useApp((s) => s.machine);
watch(conn, (c) => c === "ready" && void refreshMachine(), { immediate: true });

const view = computed(() => {
  const d = data.value;
  if (!d) return null;
  const sum = totals(d);
  const media = byMedia(d);
  const found = findings(d);
  return {
    sum,
    media,
    fast: fastest(d),
    full: fullest(d),
    found,
    warn: found.filter((f) => f.level === "warn").length,
    segments: media.map((m) => ({ key: m.tier, share: m.bytes / sum.capacity, color: m.color, title: `${m.label} · ${fmtSize(m.bytes, true)}` })),
  };
});
</script>

<template>
  <button v-if="!view" class="mcard" :aria-label="t('本机磁盘')" @click="openMachineTab()">
    <span class="mcard-head">
      <span class="mcard-icon"><Icon name="diskGauge" :size="16" /></span>
      <span class="mcard-title">{{ t("本机磁盘") }}</span>
      <Icon name="chevronRight" :size="14" />
    </span>
    <span class="muted small">{{ t("正在读取磁盘…") }}</span>
  </button>
  <button v-else class="mcard" :title="t('每块盘的类型、接口与速率，以及盘上的卷')" @click="openMachineTab()">
    <span class="mcard-head">
      <span class="mcard-icon"><Icon name="diskGauge" :size="16" /></span>
      <span class="mcard-title">{{ t("本机磁盘") }}</span>
      <span v-if="view.found.length > 0" :class="['mcard-find', { warn: view.warn }]">
        <Icon :name="view.warn ? 'warning' : 'info'" :size="12" />
        {{ t("{n} 条发现", { n: view.found.length }) }}
      </span>
      <Icon name="chevronRight" :size="14" />
    </span>
    <span class="mcard-kpis">
      <span class="mcard-kpi">
        <b>{{ view.sum.disks }}</b>
        <span>{{ t("块盘") }}</span>
      </span>
      <span class="mcard-kpi">
        <b>{{ fmtSize(view.sum.capacity, true) }}</b>
        <span>{{ t("总容量") }}</span>
      </span>
      <span class="mcard-kpi">
        <b>{{ fmtSize(view.sum.free, true) }}</b>
        <span>{{ t("可用") }}</span>
      </span>
    </span>
    <StackBar v-if="view.sum.capacity > 0" :height="6" :title="t('容量构成')" :segments="view.segments" />
    <span class="mcard-legend">
      <span v-for="m in view.media" :key="m.tier">
        <span class="chip-swatch" :style="{ background: m.color }" />
        {{ m.label }}
      </span>
    </span>
    <span class="mcard-lines">
      <span v-if="view.full" :class="view.full.share >= 0.9 ? 'warn' : ''">
        {{ t("最满") }} <b>{{ view.full.v.volume.mount_point }}</b> {{ fmtShare(view.full.v.volume.used, view.full.v.volume.total) }}
      </span>
      <span v-if="view.fast">
        {{ t("最快") }} <b>{{ linkText(view.fast.disk.link) }}</b>
      </span>
    </span>
  </button>
</template>

<!-- The progress band above the picture while a scan runs (UI-W2-10…21; ScanBand in
     shell/ScanProgress.tsx): percentage, figures, current path, progress bar, pause / continue, stop. -->
<script setup lang="ts">
import { computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { fmtInt, fmtSize, middlePath } from "@/lib/format";
import { t } from "@/lib/i18n";
import { fmtRate, fmtRemaining, phaseText, scanPercent, targetName } from "@/lib/scanProgress";
import type { ScanStatus } from "@/protocol/generated";
import { call, pauseScan, resumeScan } from "@/state/actions";
import { toast } from "@/state/store";

const props = defineProps<{ scan: ScanStatus; title: string }>();

const mft = computed(() => props.scan.method === "mft");
// an MFT scan's share comes from how much of the $MFT was read (then receiving and building)
const pct = computed(() => (props.scan.progress != null ? scanPercent(props.scan.progress) : null));
const used = computed(() => {
  const roots = props.scan.roots;
  return roots.every((r) => r.whole_volume && r.volume) ? roots.reduce((a, r) => a + (r.volume ? r.volume.total - r.volume.free : 0), 0) : null;
});
const name = computed(() => targetName(props.scan, props.title));
const figures = computed(() =>
  used.value != null ? t("已扫描 {scanned} / 已用 {used}", { scanned: fmtSize(props.scan.bytes_alloc), used: fmtSize(used.value) }) : t("已扫描 {scanned}", { scanned: fmtSize(props.scan.bytes_alloc) }),
);
const label = computed(() => (mft.value ? (phaseText(props.scan.phase) ?? t("MFT 快速扫描")) : pct.value != null ? t("{pct}%，{figures}", { pct: pct.value, figures: figures.value }) : figures.value));
const remaining = computed(() => (props.scan.remaining_s != null ? t("剩余{time}", { time: fmtRemaining(props.scan.remaining_s) }) : t("正在估算…")));

const stop = () => void call("scan.cancel", { scan_id: props.scan.scan_id }).catch((e) => toast(String(e)));
</script>

<template>
  <div :class="['scanband', { paused: scan.paused }]" role="group" :aria-label="t('扫描进度')">
    <div class="sb-pct mono" aria-hidden="true">{{ pct != null ? `${pct}%` : "…" }}</div>
    <div class="sb-mid">
      <div class="sb-line">
        <span class="strong">{{ scan.state === "queued" ? t("等待扫描 {name}", { name }) : t("正在扫描 {name}", { name }) }}</span>
        <template v-if="mft">
          <span class="sb-mft"><Icon name="bolt" :size="13" /> {{ t("MFT 快速扫描") }}</span>
          <span class="dot" />
          <span>{{ phaseText(scan.phase) ?? t("正在读取…") }}</span>
          <template v-if="scan.phase === 'mft'">
            <span class="dot" />
            <span>{{ remaining }}</span>
          </template>
          <span v-if="scan.phase !== 'build'" class="muted">{{ t("读完后一次建好目录树") }}</span>
        </template>
        <template v-else>
          <span>{{ figures }}</span>
          <span class="dot" />
          <span class="mono">{{ t("{n} 项", { n: fmtInt(scan.items) }) }}</span>
          <span v-if="scan.expected_items != null && used == null" class="muted">{{ t("（上次 {n} 项）", { n: fmtInt(scan.expected_items) }) }}</span>
          <span class="dot" />
          <span v-if="scan.paused">{{ t("磁盘读取已暂停，可随时继续") }}</span>
          <template v-else>
            <span>{{ t("每秒 {rate} 项", { rate: fmtRate(scan.rate) }) }}</span>
            <span class="dot" />
            <span>{{ remaining }}</span>
          </template>
        </template>
        <span class="flex" />
        <span v-if="scan.current_path && !scan.paused" class="sb-current" :title="scan.current_path">
          {{ t("当前") }} <span class="mono">{{ middlePath(scan.current_path, 48) }}</span>
        </span>
      </div>
      <div
        :class="['sb-bar', { indeterminate: pct == null }]"
        role="progressbar"
        :aria-label="label"
        :aria-valuemin="0"
        :aria-valuemax="100"
        :aria-valuenow="pct ?? undefined"
        :aria-valuetext="scan.paused ? t('{label}，已暂停', { label }) : label"
      >
        <span :style="pct != null ? { width: `${pct}%` } : undefined" />
      </div>
    </div>
    <div class="sb-actions">
      <button v-if="scan.paused" class="primary" @click="resumeScan(scan.scan_id)">
        <Icon name="play" :size="14" />
        {{ t("继续") }}
      </button>
      <button v-else class="ghost" :disabled="mft || scan.state !== 'scanning'" :title="mft ? t('MFT 快速扫描几秒内完成，无需暂停') : undefined" @click="pauseScan(scan.scan_id)">
        <Icon name="pause" :size="14" />
        {{ t("暂停") }}
      </button>
      <button class="ghost" @click="stop">
        <Icon name="stop" :size="14" />
        {{ t("停止") }}
      </button>
    </div>
  </div>
</template>

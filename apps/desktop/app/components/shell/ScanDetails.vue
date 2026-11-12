<!-- 「扫描详情」 side panel while a scan runs (UI-W2-23…50; ScanDetails, Method, Load and LoadCurve in
     shell/ScanProgress.tsx): scan method and the MFT switch, the disk-load limit with its 60-second
     curve, concurrency. -->
<script setup lang="ts">
import { computed, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { fmtInt, fmtKeys, fmtSize } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { activeTip, curveSummary, fmtMs, isRunning, levels, loadCurve } from "@/lib/scanProgress";
import type { ScanStatus } from "@/protocol/generated";
import { setScanLoad, useMft } from "@/state/actions";
import { type LoadLevel, set, setting, useApp } from "@/state/store";

const props = defineProps<{ scan: ScanStatus }>();

// ------------------------------------------------------------------ method (UI-W2-23…26)

const platform = window.sk.platform;
const mft = computed(() => props.scan.method === "mft");
const waiting = computed(() => props.scan.phase === "helper");
const canSwitch = computed(() => props.scan.mft_available && !mft.value && isRunning(props.scan));
// a whole volume reads its $MFT in about a second per million entries, plus starting the helper
const estimate = computed(() => Math.max(5, Math.round(((props.scan.expected_items ?? 2_000_000) / 1_000_000) * 1.2 + 3)));
const reason = computed(() => (platform === "win32" ? (props.scan.mft_available ? t("未获得管理员权限") : t("目标不是整个本地 NTFS 卷")) : ""));

// ------------------------------------------------------------------ load (UI-W2-27…45)

const io = computed(() => props.scan.io);
const customDefault = useApp((s) => setting(s, "scan.custom_limit", 70));
const custom = ref(io.value?.level === "custom" ? Math.round(io.value.limit) : customDefault.value);
// two sources, compared one by one: every progress event brings a new `io`, and a getter returning
// a new array would put the slider back under the pointer while it is dragged
watch(
  [() => io.value?.level, () => io.value?.limit] as const,
  ([level, limit]) => {
    if (level === "custom" && limit != null) custom.value = Math.round(limit);
  },
);
const options = computed(() => levels());

function pick(level: LoadLevel): void {
  const s = io.value;
  if (!s || (level === s.level && level !== "custom")) return;
  void setScanLoad(props.scan.scan_id, level, level === "custom" ? custom.value : null, s.auto ? null : s.concurrency);
}

function onLevelKey(e: KeyboardEvent): void {
  const list = options.value;
  const i = list.findIndex((l) => l.id === io.value?.level);
  const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
  if (!step) return;
  e.preventDefault();
  pick(list[(i + step + list.length) % list.length]!.id);
}

const sendCustom = () => {
  const s = io.value;
  if (s) void setScanLoad(props.scan.scan_id, "custom", custom.value, s.auto ? null : s.concurrency);
};
const setConcurrency = (n: number | null) => {
  const s = io.value;
  if (s) void setScanLoad(props.scan.scan_id, s.level, s.level === "custom" ? custom.value : null, n);
};

const curve = computed(() => (io.value ? loadCurve(io.value, props.scan.started_at) : null));
const curveLabel = computed(() => (io.value && curve.value ? curveSummary(io.value, curve.value.start, props.scan.paused) : ""));
const tip = computed(() => activeTip());

const showLog = () => set({ logOpen: true, logTab: "log", logDismissed: false });
</script>

<template>
  <aside class="inspector scandetails" :aria-label="t('扫描详情')">
    <div class="res-head">
      <h2>{{ t("扫描详情") }}</h2>
      <span class="flex" />
      <button class="icon-btn small" :aria-label="t('收起侧栏')" :title="t('收起侧栏')" @click="set({ inspectorHidden: true })">
        <Icon name="sidebar" />
      </button>
    </div>
    <section class="sd-section">
      <h3>{{ t("扫描方式") }}</h3>
      <div class="sd-method">
        <span :class="['sd-method-icon', { fast: mft }]">
          <Icon :name="mft ? 'bolt' : 'folder'" :size="16" />
        </span>
        <div>
          <div class="strong">{{ mft ? t("MFT 快速扫描（只读提权助手）") : reason ? t("硬扫描（{reason}）", { reason }) : t("并行硬扫描") }}</div>
          <div class="muted small">{{ mft ? t("直接读取 NTFS 主文件表，整盘几秒完成；主程序不提权") : t("逐目录遍历，耗时随文件数量增长") }}</div>
        </div>
      </div>
      <button v-if="canSwitch" class="primary sd-elevate" :disabled="waiting" @click="useMft(scan.scan_id)">
        <Icon name="shield" :size="15" />
        <span class="sd-elevate-text">
          <span>{{ waiting ? t("正在等待管理员授权…") : t("以管理员身份启用 MFT 快速扫描") }}</span>
          <span class="small">{{ t("约 {n} 秒完成 · 仅启动只读扫描助手", { n: estimate }) }}</span>
        </span>
      </button>
      <div v-if="scan.fallback" class="sd-note muted small" role="status"><Icon name="warning" :size="13" /> {{ scan.fallback }}</div>
    </section>
    <section v-if="io && curve" class="sd-section">
      <div class="row-between">
        <h3>{{ t("磁盘负载上限") }}</h3>
        <span class="small" :title="t('SpaceKnife 最近 1 秒在这块磁盘上造成的读取占用（受上限控制）')">
          <template v-if="io.measured">
            {{ t("当前") }} <span class="strong mono">{{ Math.round(io.self_pct) }}%</span>
            <span class="muted"> · {{ t("整盘 {n}%", { n: Math.round(io.total_pct) }) }}</span>
            <span class="muted" :title="tip"> · {{ t("活动时间 {n}%", { n: Math.round(io.active_pct) }) }}</span>
          </template>
          <span v-else class="muted">{{ t("无法测量此磁盘") }}</span>
        </span>
      </div>
      <div class="segmented small sd-levels" role="radiogroup" :aria-label="t('磁盘负载上限')" @keydown="onLevelKey">
        <button
          v-for="l in options"
          :key="l.id"
          role="radio"
          :aria-checked="io.level === l.id"
          :tabindex="io.level === l.id ? 0 : -1"
          :class="io.level === l.id ? 'on' : ''"
          @click="pick(l.id)"
        >
          {{ l.id === "custom" && io.level === "custom" ? t("自定义 {n}%", { n: Math.round(io.limit) }) : l.label }}
        </button>
      </div>
      <div v-if="io.level === 'custom'" class="set-slider">
        <input v-model.number="custom" type="range" :aria-label="t('自定义上限')" :min="5" :max="100" :step="5" @pointerup="sendCustom" @keyup="sendCustom" />
        <span class="set-value mono">
          {{ custom }}
          <span class="muted"> %</span>
        </span>
      </div>
      <div v-if="io.yielding" class="sd-note small warn-text" role="status"><Icon name="warning" :size="13" /> {{ t("其他程序正在大量读写这块磁盘，扫描已让路（最低进度）") }}</div>
      <figure class="sd-curve">
        <figcaption class="row-between small">
          <span class="muted">{{ t("磁盘读取占用 · 近 60 秒") }}</span>
          <span class="sd-legend muted">
            <!-- 「占用」 here is the disk load, not the size on disk the same word means elsewhere -->
            <span class="lg-line" /> {{ byLang("占用", "Load") }} <span class="lg-dash" /> {{ t("上限") }} <span class="lg-dot" :title="tip" />
            <span :title="tip">{{ t("活动时间") }}</span>
          </span>
        </figcaption>
        <svg :viewBox="`0 0 ${curve.W} ${curve.H}`" preserveAspectRatio="none" role="img" :aria-label="curveLabel">
          <line v-for="g in [25, 50, 75]" :key="g" class="grid" :x1="0" :x2="curve.W" :y1="curve.y(g)" :y2="curve.y(g)" />
          <path v-if="curve.total" class="total" :d="curve.total" />
          <path v-if="curve.active" class="active" :d="curve.active" />
          <path v-if="curve.own" class="own" :d="curve.own" />
          <line v-if="curve.limited" class="limit" :x1="0" :x2="curve.W" :y1="curve.y(io.limit)" :y2="curve.y(io.limit)" />
          <g v-if="curve.showStart && curve.start != null" class="start">
            <line :x1="curve.x(curve.start)" :x2="curve.x(curve.start)" :y1="curve.PAD_T" :y2="curve.H - curve.PAD_B" />
            <text :x="Math.min(curve.W - 44, curve.x(curve.start) + 3)" :y="curve.H - 3">{{ t("扫描开始") }}</text>
          </g>
          <circle v-if="curve.last" :class="['now', { paused: scan.paused }]" :cx="curve.x(curve.last.t)" :cy="curve.y(curve.last.self_pct)" :r="3" />
        </svg>
      </figure>
      <div v-if="io.measured && io.read_bps != null" class="small sd-rates" :title="t('整块磁盘在最近一次采样里的读取速度和平均每次读取的耗时（包括其他程序）')">
        {{ t("整盘读取") }} <span class="strong mono">{{ fmtSize(io.read_bps) }}/s</span>
        <span v-if="io.read_latency_ms != null" class="muted"> · {{ t("平均延迟 {ms} 毫秒", { ms: fmtMs(io.read_latency_ms) }) }}</span>
      </div>
      <div v-if="!mft" class="row-between sd-concurrency">
        <span class="small">
          {{ t("并发") }} <span class="strong mono">{{ io.concurrency }}</span>
          <span class="muted"> · {{ t("进行中 {n}", { n: io.inflight }) }}</span>
        </span>
        <span class="sd-auto">
          <span v-if="!io.auto" class="stepper" :aria-label="t('手动并发数')">
            <button :aria-label="t('减少并发')" :disabled="io.concurrency <= 1" @click="setConcurrency(io.concurrency - 1)">
              <Icon name="minus" :size="13" />
            </button>
            <span class="stepper-mid">
              <span class="num">{{ io.concurrency }}</span>
            </span>
            <button :aria-label="t('增加并发')" :disabled="io.concurrency >= 64" @click="setConcurrency(io.concurrency + 1)">
              <Icon name="plus" :size="13" />
            </button>
          </span>
          <span class="small">{{ t("自动调节") }}</span>
          <button role="switch" :aria-checked="io.auto" :aria-label="t('自动调节并发')" :class="['switch', { on: io.auto }]" @click="setConcurrency(io.auto ? io.concurrency : null)">
            <span />
          </button>
        </span>
      </div>
      <p v-if="mft" class="muted small">{{ t("MFT 读取是一次顺序读取，按档位放慢读取节奏，不分并发。") }}</p>
    </section>
    <section v-if="scan.errors > 0" class="sd-section">
      <div class="sd-problem">
        <span class="warn"><Icon name="warning" :size="16" /></span>
        <span class="flex">{{ t("{n} 个文件夹无法访问", { n: fmtInt(scan.errors) }) }}</span>
        <button class="ghost tiny" @click="showLog">
          <Icon name="log" :size="13" />
          {{ t("查看日志") }}
        </button>
      </div>
      <div v-if="scan.denied[0]" class="muted small">
        {{ t("已计入未知空间，例如") }}
        <div class="mono sd-path" :title="scan.denied.slice(0, 5).join('\n')">{{ scan.denied[0] }}</div>
      </div>
    </section>
    <span class="flex" />
    <div class="sd-tip">
      <Icon name="info" :size="16" />
      <div>
        <div class="strong">{{ t("扫描中可以随时放大和导航") }}</div>
        <div class="muted small sd-keys">
          <span><Kbd>{{ fmtKeys("Enter") }}</Kbd> {{ t("放大进入") }}</span>
          <span><Kbd>{{ fmtKeys("Backspace") }}</Kbd> {{ t("后退") }}</span>
          <span><Kbd>{{ fmtKeys("Ctrl+Home") }}</Kbd> {{ t("根") }}</span>
        </div>
      </div>
    </div>
  </aside>
</template>

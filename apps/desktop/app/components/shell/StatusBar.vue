<!-- Status bar (UI-MAIN-48…53): scan result, live monitoring, disk load, free space. -->
<script setup lang="ts">
import { computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { fmtDuration, fmtInt, fmtSize } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { activeTip, levelName, levelText, scanPercent } from "@/lib/scanProgress";
import { LEVEL_CAP, monitorText } from "@/lib/statusBar";
import { continueScan, updateSettings } from "@/state/actions";
import { activeTab, type LoadLevel, set, setting, startFormOf, toast, toggleLog, useApp } from "@/state/store";

const tab = useApp((s) => activeTab(s));
const scan = useApp((s) => (tab.value.scanId ? s.scans[tab.value.scanId] : undefined));
const monitorOn = useApp((s) => setting(s, "scan.monitor", true));
const live = computed(() => monitorText(scan.value, monitorOn.value));
const conn = useApp((s) => s.conn);
const volumes = useApp((s) => s.volumes);
const warnings = useApp((s) => s.logs.filter((l) => l.level === "WARNING" || l.level === "ERROR").length);
// the start page shows its chosen target and load level (UI-W1-13, UI-W1-14)
const form = useApp((s) => (tab.value.kind === "start" ? startFormOf(s, tab.value.id) : null));
const vol = computed(() => {
  const f = form.value;
  const formVolume = f ? volumes.value.find((v) => v.mount_point === (f.path.trim() ? f.check?.items.find((i) => i.volume)?.volume : f.picked)) : undefined;
  return scan.value?.roots[0]?.volume ?? formVolume ?? volumes.value.find((v) => v.is_system) ?? volumes.value[0];
});
const io = computed(() => {
  const s = scan.value;
  return s && (s.state === "scanning" || s.state === "queued") ? s.io : null;
});
const load = computed<LoadLevel>(() => (form.value ? form.value.load : "balanced"));
// 「均衡 ≤70%」, 「安静 ≤35%」; 极速, 空闲 and 自定义 without a percentage
const levelLabel = computed(() => (load.value === "balanced" || load.value === "quiet" ? `${levelName(load.value)} ≤${LEVEL_CAP[load.value]}%` : levelName(load.value)));
const cap = computed(() => {
  const x = io.value;
  return x ? (x.level === "max" || x.level === "idle" ? 0 : x.limit) : LEVEL_CAP[load.value];
});
const used = computed(() => (vol.value ? vol.value.total - vol.value.free : 0));
const running = computed(() => scan.value?.state === "scanning" || scan.value?.state === "queued");

const ioTitle = computed(() => {
  const x = io.value;
  return x
    ? t("SpaceKnife 当前占用这块磁盘 {self}%，整盘 {total}%，活动时间 {active}%\n{tip}", {
        self: Math.round(x.self_pct),
        total: Math.round(x.total_pct),
        active: Math.round(x.active_pct),
        tip: activeTip(),
      })
    : t("扫描时按这个上限限制磁盘读取");
});

function toggleMonitor(): void {
  const on = monitorOn.value;
  void updateSettings({ "scan.monitor": !on }).then((ok) => ok && toast(on ? t("实时监控已关闭") : t("实时监控已开启")));
}
</script>

<template>
  <footer class="statusbar">
    <button class="status-item" :title="warnings ? t('{n} 条警告', { n: warnings }) : t('扫描统计与日志')" @click="toggleLog()">
      <span v-if="conn !== 'ready'" class="danger">
        <Icon name="warning" :size="14" /> {{ conn === "failed" ? t("后端不可用") : t("后端连接中断 · 正在重连") }}
      </span>
      <span v-else-if="!scan">{{ t("尚未扫描 · 选择位置后开始") }}</span>
      <template v-else-if="running">
        <Icon :name="scan.paused ? 'pause' : 'refresh'" :size="14" />
        <span class="strong">{{ scan.paused ? t("已暂停") : t("扫描中…") }}{{ scan.progress != null ? `${scan.paused ? " ·" : ""} ${scanPercent(scan.progress)}%` : "" }}</span>
        <span>·</span>
        <span>{{ t("{n} 项", { n: fmtInt(scan.items) }) }}</span>
      </template>
      <template v-else>
        <span class="accent">
          <Icon name="bolt" :size="14" />
        </span>
        <!-- 「快照」 here is the one snapshot the tab shows (elsewhere the same word names the snapshots, plural) -->
        <span class="strong">{{ scan.source === "snapshot" ? byLang("快照", "Snapshot") : scan.method === "mft" ? t("MFT 快速扫描") : t("硬扫描") }}</span>
        <span>·</span>
        <span>{{ t("{n} 项", { n: fmtInt(scan.items) }) }}</span>
        <span>·</span>
        <span>{{ fmtDuration(scan.elapsed_s) }}</span>
      </template>
      <span v-if="warnings > 0" class="warn-count">{{ warnings }}</span>
    </button>
    <button
      v-if="scan && scan.source === 'scan' && scan.state === 'cancelled' && scan.pending_dirs > 0"
      class="status-item"
      :title="t('还有 {n} 个文件夹没有读取', { n: fmtInt(scan.pending_dirs) })"
      @click="continueScan(scan.scan_id)"
    >
      <Icon name="play" :size="14" />
      <span>{{ t("继续扫描") }}</span>
    </button>
    <span class="status-sep" />
    <button class="status-item" :title="live.title" @click="toggleMonitor">
      <span :class="live.tone">
        <Icon name="pulse" :size="14" />
      </span>
      <span>{{ live.text }}</span>
    </button>
    <span class="status-sep" />
    <button class="status-item" :title="ioTitle" @click="io && set({ inspectorHidden: false })">
      <Icon name="gauge" :size="14" />
      <span>{{ form ? t("磁盘负载上限") : t("磁盘负载") }}</span>
      <template v-if="io">
        <span class="strong">{{ levelText(io) }}</span>
        <span>· {{ t("当前 {value}", { value: io.measured ? `${Math.round(io.self_pct)}%` : "—" }) }}</span>
        <span v-if="io.measured" class="muted">· {{ t("活动时间 {n}%", { n: Math.round(io.active_pct) }) }}</span>
      </template>
      <span v-else class="strong">{{ levelLabel }}</span>
      <span v-if="cap > 0 && cap < 100" class="meter">
        <span class="meter-track" />
        <span v-if="io && io.measured" class="meter-fill" :style="{ width: `${Math.min(100, io.self_pct)}%` }" />
        <span class="meter-mark" :style="{ left: `${cap}%` }" />
      </span>
    </button>
    <span class="flex" />
    <button
      v-if="vol"
      class="status-item"
      :title="t('{fs} · {size}\n已用 {used} / {total}', { fs: vol.fs_type, size: fmtSize(vol.total, true), used: fmtSize(used), total: fmtSize(vol.total, true) })"
    >
      <span class="capacity">
        <span :style="{ width: `${vol.total ? (used / vol.total) * 100 : 0}%` }" />
      </span>
      <span>{{ t("可用") }}</span>
      <span class="strong">{{ fmtSize(vol.free) }}</span>
      <span>/ {{ fmtSize(vol.total, true) }}</span>
    </button>
  </footer>
</template>

<!-- 「本机磁盘」 (machine.disks): a dashboard of this computer's disks, scanned or not. On top, the
     headline numbers (disks, capacity and room, volumes, the fastest link, findings), the capacity by
     media and the findings; then a card per disk (MachineDisk.vue): how full it is, its interface and
     link, its volumes in disk order and what SpaceKnife knows of each from an open scan or its
     snapshots (classes, largest folders, growth). It opens only when asked (the start page's card,
     the palette) and reads the disks when shown and on 「刷新」. -->
<script setup lang="ts">
import { computed, watch } from "vue";

import MachineDisk from "@/components/pages/MachineDisk.vue";
import MachineVolume from "@/components/pages/MachineVolume.vue";
import Ring from "@/components/ui/charts/Ring.vue";
import StackBar from "@/components/ui/charts/StackBar.vue";
import Icon from "@/components/ui/Icon.vue";
import { classHexNow } from "@/lib/classes";
import { fmtDateTime, fmtInt, fmtShare, fmtSize } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { FULL, rowName, seconds } from "@/lib/machinePage";
import { refreshMachine } from "@/state/actions";
import { bandwidthText, byMedia, classGaps, classRows, diskName, elsewhere, fastest, findings, linkText, orderedDisks, summary, totals } from "@/state/machine";
import { useApp } from "@/state/store";

const conn = useApp((s) => s.conn);
const data = useApp((s) => s.machine);
const loading = useApp((s) => s.machineLoading);
const vocab = useApp((s) => s.vocab); // class names and colours follow the vocabulary
watch(conn, (c) => c === "ready" && void refreshMachine(), { immediate: true });

const sum = computed(() => (data.value ? summary(data.value) : null));
const other = computed(() => (data.value ? elsewhere(data.value) : []));
const disks = computed(() => (data.value ? orderedDisks(data.value) : []));

// ------------------------------------------------------------------ the summary

const kpis = computed(() => {
  const d = data.value;
  if (!d) return null;
  const tt = totals(d);
  const fast = fastest(d);
  const room = tt.used + tt.free;
  const used = room ? tt.used / room : 0;
  return { tt, fast, room, used, counts: summary(d).counts };
});

const media = computed(() => {
  const m = data.value ? byMedia(data.value) : [];
  const total = m.reduce((s, x) => s + x.bytes, 0);
  return { rows: m, total, segments: m.map((x) => ({ key: x.tier, share: x.bytes / total, color: x.color, title: `${x.label} · ${fmtSize(x.bytes, true)}` })) };
});

/** What fills the volumes: the file classes of every volume whose complete scan is known (open,
 * or kept from the last one), added up, in the class colors of the per-volume bars. */
const classes = computed(() => {
  void vocab.value;
  const d = data.value;
  const total = d?.classes ?? null;
  const rows = d ? classRows(total) : [];
  const gaps = d ? classGaps(d) : [];
  const color = (id: string | null) => (id ? classHexNow(id) : "var(--sk-visual-branch-neutral)");
  const sumBytes = total ? total.classes.reduce((s, c) => s + c.bytes, 0) : 0;
  return {
    total,
    rows: rows.map((r) => ({ ...r, name: rowName(r), color: color(r.classId) })),
    gaps,
    sumBytes,
  };
});
const list = (xs: string[]) => xs.join(byLang("、", ", "));

const found = computed(() => (data.value ? findings(data.value) : []));
const warnCount = computed(() => found.value.filter((f) => f.level === "warn").length);
</script>

<template>
  <div class="machine">
    <div class="mc-inner">
      <header class="mc-head">
        <div class="mc-head-text">
          <h1>{{ t("本机磁盘") }}</h1>
          <p class="muted">
            {{ sum && data?.disks.length ? t("{counts}，共 {size}", { counts: sum.counts, size: fmtSize(sum.total, true) }) : t("这台电脑的物理磁盘、接口速率，以及每块盘上的卷") }}
          </p>
        </div>
        <div class="mc-head-actions">
          <span v-if="data" class="muted small">{{ t("读取于 {time}", { time: fmtDateTime(seconds(data.collected_at)) }) }}</span>
          <button class="ghost small" :disabled="loading || conn !== 'ready'" @click="refreshMachine(true)">
            <Icon name="refresh" :size="14" />
            {{ loading ? t("正在读取…") : t("刷新") }}
          </button>
        </div>
      </header>
      <div v-if="!data" class="muted">{{ t("正在读取磁盘…") }}</div>
      <template v-else>
        <section v-if="kpis" class="mc-kpis" :aria-label="t('汇总')">
          <div class="mc-kpi">
            <span class="mc-kpi-visual"><Icon name="drive" :size="18" /></span>
            <span class="mc-kpi-text">
              <span class="mc-kpi-label">{{ t("物理磁盘") }}</span>
              <span class="mc-kpi-value">{{ fmtInt(kpis.tt.disks) }}</span>
              <span class="mc-kpi-sub" :title="kpis.counts || '—'">{{ kpis.counts || "—" }}</span>
            </span>
          </div>
          <div class="mc-kpi">
            <span class="mc-kpi-visual">
              <Ring :share="kpis.used" :size="40" :stroke="5" :tone="kpis.used >= FULL ? 'warn' : 'accent'" :title="t('已用 {pct}', { pct: fmtShare(kpis.tt.used, kpis.room || 1) })">
                {{ Math.round(kpis.used * 100) }}
              </Ring>
            </span>
            <span class="mc-kpi-text">
              <span class="mc-kpi-label">{{ t("容量") }}</span>
              <span class="mc-kpi-value">{{ fmtSize(kpis.tt.capacity, true) }}</span>
              <span class="mc-kpi-sub" :title="t('已用 {used} · 可用 {free}', { used: fmtSize(kpis.tt.used, true), free: fmtSize(kpis.tt.free, true) })">
                {{ t("已用 {used} · 可用 {free}", { used: fmtSize(kpis.tt.used, true), free: fmtSize(kpis.tt.free, true) }) }}
              </span>
            </span>
          </div>
          <div class="mc-kpi">
            <span class="mc-kpi-visual"><Icon name="layers" :size="18" /></span>
            <span class="mc-kpi-text">
              <span class="mc-kpi-label">{{ t("卷") }}</span>
              <span class="mc-kpi-value">{{ fmtInt(kpis.tt.volumes) }}</span>
              <span class="mc-kpi-sub" :title="t('{n} 个有扫描或快照', { n: kpis.tt.known })">{{ t("{n} 个有扫描或快照", { n: kpis.tt.known }) }}</span>
            </span>
          </div>
          <div class="mc-kpi">
            <span class="mc-kpi-visual"><Icon name="gauge" :size="18" /></span>
            <span class="mc-kpi-text">
              <span class="mc-kpi-label">{{ t("最快链路") }}</span>
              <span class="mc-kpi-value">{{ kpis.fast ? (linkText(kpis.fast.disk.link) ?? "—") : "—" }}</span>
              <span class="mc-kpi-sub" :title="kpis.fast ? `${bandwidthText(kpis.fast.mbps)} · ${diskName(kpis.fast.disk)}` : t('系统没有报告链路')">
                {{ kpis.fast ? `${bandwidthText(kpis.fast.mbps)} · ${diskName(kpis.fast.disk)}` : t("系统没有报告链路") }}
              </span>
            </span>
          </div>
        </section>
        <section class="mc-panel mc-classes-total" :aria-label="t('文件类别')">
          <h2>{{ t("文件类别") }}</h2>
          <template v-if="classes.total && classes.rows.length">
            <p class="muted small">
              {{ t("{n} 个卷，共 {size}：{list}", { n: classes.total.volumes.length, size: fmtSize(classes.total.bytes, true), list: list(classes.total.volumes) })
              }}{{ classes.gaps.length > 0 ? ` · ${t("还没有完整扫描过：{list}", { list: list(classes.gaps) })}` : "" }}
            </p>
            <StackBar
              :height="14"
              :title="t('文件类别')"
              :segments="classes.rows.map((r) => ({ key: r.key, share: r.share, color: r.color, title: `${r.name} · ${fmtSize(r.bytes, true)}` }))"
            />
            <ul class="mc-class-legend">
              <li v-for="r in classes.rows" :key="r.key" :title="`${r.name} · ${fmtSize(r.bytes, true)} · ${fmtShare(r.bytes, classes.sumBytes)}`">
                <span class="chip-swatch" :style="{ background: r.color }" />
                <span class="mc-class-name">{{ r.name }}</span>
                <span class="mono">{{ fmtSize(r.bytes, true) }}</span>
                <span class="mono muted">{{ `${Math.round(r.share * 1000) / 10}%` }}</span>
              </li>
            </ul>
          </template>
          <p v-else class="muted small">{{ t("扫描一个整卷之后，这里显示所有卷里都是哪些文件；结果会一直保留，直到下一次扫描") }}</p>
        </section>
        <div class="mc-panels">
          <section class="mc-panel" :aria-label="t('容量构成')">
            <h2>{{ t("容量构成") }}</h2>
            <p class="muted small">{{ t("按存储类型，越亮越快") }}</p>
            <template v-if="media.total > 0">
              <StackBar :height="12" :title="t('容量构成')" :segments="media.segments" />
              <table class="mc-legend">
                <tbody>
                  <tr v-for="m in media.rows" :key="m.tier">
                    <td>
                      <span class="chip-swatch" :style="{ background: m.color }" />
                      {{ m.label }}
                    </td>
                    <td class="muted">{{ t("{n} 块", { n: m.disks }) }}</td>
                    <td class="mono">{{ fmtSize(m.bytes, true) }}</td>
                    <td class="mono muted">{{ fmtShare(m.bytes, media.total) }}</td>
                  </tr>
                </tbody>
              </table>
            </template>
            <p v-else class="muted small">{{ t("系统没有报告磁盘容量") }}</p>
          </section>
          <section class="mc-panel" :aria-label="t('发现')">
            <h2>
              {{ t("发现") }} <span v-if="found.length > 0" class="mono muted">{{ found.length }}</span>
            </h2>
            <p v-if="warnCount > 0" class="muted small">{{ t("{n} 条需要留意", { n: warnCount }) }}</p>
            <ul v-if="found.length" class="mc-findings">
              <li v-for="f in found" :key="f.text" :class="f.level">
                <Icon :name="f.level === 'warn' ? 'warning' : 'info'" :size="14" />
                <span>{{ f.text }}</span>
              </li>
            </ul>
            <p v-else class="mc-allgood">
              <Icon name="check" :size="14" />
              {{ t("一切正常：没有快满的卷，每条链路都跑在磁盘支持的速度上") }}
            </p>
          </section>
        </div>
        <h2 class="mc-h2">{{ t("磁盘") }}</h2>
        <div class="mc-grid mc-disks">
          <MachineDisk v-for="d in disks" :key="d.id" :disk="d" :data="data" />
        </div>
        <div v-if="!data.disks.length" class="mc-empty muted">{{ t("这个系统没有告诉 SpaceKnife 有哪些物理磁盘；下面是它的卷") }}</div>
        <section v-if="other.length > 0" class="mc-other" :aria-label="t('其他位置')">
          <h2 class="mc-h2">{{ t("其他位置") }}</h2>
          <p class="muted small">{{ t("网络位置、虚拟卷，以及找不到所在磁盘的卷") }}</p>
          <div class="mc-grid">
            <div v-for="v in other" :key="v.volume.mount_point" class="mc-disk mc-place">
              <MachineVolume :v="v" />
            </div>
          </div>
        </section>
      </template>
    </div>
  </div>
</template>

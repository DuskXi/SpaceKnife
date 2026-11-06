<!-- A disk's card on 「本机磁盘」: how full it is, its interface and link (the rate it runs at against
     what the disk supports), read / written, temperature and life, why its own record is missing,
     its volumes in disk order (MachineVolume.vue) and the details. -->
<script setup lang="ts">
import { computed } from "vue";

import MachineVolume from "@/components/pages/MachineVolume.vue";
import Meter from "@/components/ui/charts/Meter.vue";
import Ring from "@/components/ui/charts/Ring.vue";
import Icon from "@/components/ui/Icon.vue";
import { fmtDateTime, fmtInt, fmtShare, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { FULL, seconds } from "@/lib/machinePage";
import { MEDIA_ZH } from "@/lib/volumes";
import type { MachineDisks, PhysicalDisk } from "@/protocol/generated";
import { readDiskRecords } from "@/state/actions";
import {
  diskLabel,
  diskMap,
  diskName,
  diskUsed,
  healthNoteText,
  interfaceName,
  LIFE_LOW,
  lifeLeft,
  linkBandwidth,
  linkNotes,
  linkShare,
  linkText,
  powerOnText,
  recordGap,
  showMap,
  volumesOf,
} from "@/state/machine";
import { useApp } from "@/state/store";

const props = defineProps<{ disk: PhysicalDisk; data: MachineDisks }>();

const conn = useApp((s) => s.conn);
const reading = useApp((s) => s.machineReading);
const readNote = useApp((s) => s.machineReadNote);

const vols = computed(() => volumesOf(props.data, props.disk));
const map = computed(() => diskMap(props.disk, props.data.volumes));
const name = computed(() => diskName(props.disk));
const empty = computed(() => props.disk.size == null); // a card reader's empty slot: its name and its drive letter only
const used = computed(() => (!empty.value ? diskUsed(props.disk, props.data) : null));
const fill = computed(() => (used.value != null && props.disk.size ? used.value / props.disk.size : null));
const meta = computed(() =>
  [
    MEDIA_ZH[props.disk.media] ?? props.disk.media,
    props.disk.size ? fmtSize(props.disk.size, true) : t("没有插入介质"),
    used.value != null ? t("已用 {size}", { size: fmtSize(used.value, true) }) : null,
    diskLabel(props.disk),
  ]
    .filter(Boolean)
    .join(" · "),
);

/** Interface, current link and how it compares with the most the disk supports: one line (the
 * meter and the link's text give way first; their full text is in the tooltips). */
const link = computed(() => {
  const d = props.disk;
  const text = linkText(d.link);
  const bandwidth = linkBandwidth(d.link);
  const share = linkShare(d.link);
  const pct = share == null ? "" : `${Math.round(share * 100)}%`;
  const shareText = share == null ? "" : share >= 0.999 ? t("满速") : t("最高速率的 {pct}", { pct });
  return { text, bandwidth, share, pct, shareText, title: t("当前链路速率：{link}", { link: [text, bandwidth].filter(Boolean).join(" ") }) };
});

/** Read and written: over the disk's life (its own record) and since the system started. Always
 * both rows: what isn't known is "—", with the reason on hover. */
const io = computed(() => {
  const h = props.disk.health;
  const io = props.disk.io;
  return {
    h,
    io,
    lifetime: h?.read_at ? t("磁盘自己记录的整个寿命里的读写量，以管理员权限读取于 {when}", { when: fmtDateTime(seconds(h.read_at)) }) : t("磁盘自己记录的整个寿命里的读写量"),
    lifetimeGap: recordGap(props.disk),
    since: io?.since ? t("系统 {when} 启动以来的读写量", { when: fmtDateTime(seconds(io.since)) }) : t("系统启动以来的读写量"),
    sinceGap: t("系统没有给出这块盘开机以来的读写计数"),
  };
});
const size = (b: number | null | undefined) => (b == null ? "—" : fmtSize(b, true));

/** Temperature, power-on time and (NVMe) the write endurance left: always the three, "—" when the
 * disk doesn't say, with the reason on hover. The life bar is full on a new drive and shrinks with
 * wear; only a little left (or past its rated endurance) is a warning. */
const health = computed(() => {
  const d = props.disk;
  const h = d.health;
  const gap = recordGap(d);
  const temp = h?.temperature_c ?? null;
  const hours = h?.power_on_hours ?? null;
  const usedPct = h?.percentage_used ?? null;
  const left = usedPct == null ? null : lifeLeft(usedPct);
  const low = left != null && (left <= LIFE_LOW || (usedPct ?? 0) > 100);
  const lifeTitle =
    usedPct == null
      ? d.interface === "nvme"
        ? gap
        : t("只有 NVMe 固态硬盘报告写入寿命的估计")
      : usedPct > 100
        ? t("已超过额定的写入寿命：厂商估计已用 {used}。盘通常还能继续用，只是写入量超过了厂商的保证", { used: `${usedPct}%` })
        : t("剩余的写入寿命：厂商估计已用 {used}，还剩 {left}", { used: `${usedPct}%`, left: `${left}%` });
  return { gap, temp, hours, left, low, lifeTitle };
});

/** Why the disk's own record is missing (two lines at most, the rest on hover), the button that
 * reads it with administrator rights (Linux), and why that read didn't happen. */
const record = computed(() => {
  const note = healthNoteText(props.disk.health_note);
  const failed = readNote.value?.disk === props.disk.id ? readNote.value.text : "";
  const admin = props.disk.health_note === "needs_admin";
  return { note, failed, admin, shown: !!note || admin || !!failed };
});
</script>

<template>
  <section :class="['mc-disk', { 'no-medium': empty }]" :aria-label="name">
    <div class="mc-disk-head">
      <Ring v-if="fill != null" :share="fill" :size="44" :stroke="5" :tone="fill >= FULL ? 'warn' : 'accent'" :title="t('已用 {pct}', { pct: fmtShare(used ?? 0, disk.size ?? 1) })">
        {{ Math.round(fill * 100) }}
      </Ring>
      <span v-else class="mc-icon"><Icon name="drive" :size="18" /></span>
      <div class="mc-title">
        <div class="mc-name" :title="name">{{ name }}</div>
        <div class="mc-meta">{{ meta }}</div>
      </div>
      <div class="mc-tags">
        <span v-if="disk.system" class="mc-tag accent">{{ t("系统盘") }}</span>
        <span v-if="disk.removable" class="mc-tag">{{ t("可移动") }}</span>
      </div>
    </div>
    <!-- fixed slots, one row each in the grid (subgrid): every card in a row lines up section by
         section; a slot with nothing in it takes no room -->
    <div class="mc-slot">
      <div v-if="!empty" class="mc-line">
        <span class="mc-chip fixed" :title="t('接口')">
          <Icon name="bolt" :size="12" />
          {{ interfaceName(disk.interface) }}
        </span>
        <span v-if="link.text" class="mc-chip squeeze" :title="link.title">
          <Icon name="gauge" :size="12" />
          <span class="mc-chip-text">
            {{ link.text }}<span v-if="link.bandwidth" class="mc-chip-sub"> {{ link.bandwidth }}</span>
          </span>
        </span>
        <span v-if="link.share != null" class="mc-linkmeter" :title="t('链路速率占这块盘支持的最高速率的比例：{share}', { share: link.shareText })">
          <Meter :share="link.share" :tone="link.share < 1 ? 'muted' : 'accent'" />
          <span class="small muted">{{ link.share >= 0.999 ? link.shareText : link.pct }}</span>
        </span>
      </div>
    </div>
    <div class="mc-slot">
      <template v-if="!empty">
        <div v-for="n in linkNotes(disk)" :key="n.text" :class="['mc-note', { warn: n.warn }]">
          <Icon :name="n.warn ? 'warning' : 'info'" :size="13" />
          <span class="mc-clamp" :title="n.text">{{ n.text }}</span>
        </div>
      </template>
    </div>
    <div class="mc-slot">
      <table v-if="!empty" class="mc-io">
        <thead>
          <tr>
            <th />
            <th>{{ t("读取") }}</th>
            <th>{{ t("写入") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr :title="io.h?.read_bytes != null || io.h?.written_bytes != null ? io.lifetime : io.lifetimeGap">
            <th>{{ t("累计") }}</th>
            <td :class="io.h?.read_bytes == null ? 'none' : undefined" :title="io.h?.read_bytes == null ? io.lifetimeGap : undefined">{{ size(io.h?.read_bytes) }}</td>
            <td :class="io.h?.written_bytes == null ? 'none' : undefined" :title="io.h?.written_bytes == null ? io.lifetimeGap : undefined">{{ size(io.h?.written_bytes) }}</td>
          </tr>
          <tr :title="io.io ? io.since : io.sinceGap">
            <th>{{ t("开机以来") }}</th>
            <td :class="io.io ? undefined : 'none'">{{ size(io.io?.read_bytes) }}</td>
            <td :class="io.io ? undefined : 'none'">{{ size(io.io?.written_bytes) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
    <div class="mc-slot">
      <div v-if="!empty" class="mc-line">
        <span :class="['mc-chip fixed', { none: health.temp == null, warn: health.temp != null && health.temp >= 70 }]" :title="health.temp == null ? health.gap : t('温度')">
          <Icon name="thermometer" :size="12" />
          {{ health.temp == null ? "— °C" : `${health.temp} °C` }}
        </span>
        <span :class="['mc-chip fixed', { none: health.hours == null }]" :title="health.hours == null ? health.gap : t('{n} 小时', { n: fmtInt(health.hours) })">
          <Icon name="clock" :size="12" />
          {{ t("通电 {time}", { time: health.hours == null ? "—" : powerOnText(health.hours) }) }}
        </span>
        <span :class="['mc-linkmeter', { none: health.left == null, warn: health.left != null && health.low }]" :title="health.lifeTitle">
          <Meter :share="health.left == null ? 0 : health.left / 100" :tone="health.low ? 'warn' : 'muted'" />
          <span class="small muted">{{ t("剩余寿命 {pct}", { pct: health.left == null ? "—" : `${health.left}%` }) }}</span>
        </span>
      </div>
    </div>
    <div class="mc-slot">
      <template v-if="!empty && record.shown">
        <div v-if="record.note" class="mc-note">
          <Icon name="info" :size="13" />
          <span class="mc-clamp" :title="record.note">{{ record.note }}</span>
        </div>
        <div v-if="record.admin" class="mc-read">
          <button
            class="ghost small"
            :disabled="reading !== null || conn !== 'ready'"
            :title="t('会弹出系统的管理员授权。只读取磁盘自己的记录，不改动任何东西；一次授权读出所有需要的盘')"
            @click="readDiskRecords(disk.id)"
          >
            <Icon name="shield" :size="13" />
            {{ reading === disk.id ? t("正在等待授权…") : t("用管理员权限读取…") }}
          </button>
        </div>
        <div v-if="record.failed" class="mc-note warn" role="status">
          <Icon name="warning" :size="13" />
          <span class="mc-clamp" :title="record.failed">{{ record.failed }}</span>
        </div>
      </template>
    </div>
    <div class="mc-slot">
      <div v-if="showMap(map)" class="mc-map" aria-hidden="true">
        <span v-for="s in map" :key="s.mount" class="mc-seg" :style="{ width: `${s.share * 100}%` }" :title="s.mount">
          <span :style="{ width: `${s.used * 100}%` }" />
        </span>
      </div>
      <div v-if="vols.length > 0" class="mc-vols">
        <MachineVolume v-for="v in vols" :key="v.volume.mount_point" :v="v" />
      </div>
    </div>
    <details class="mc-details">
      <summary>{{ t("详细信息") }}</summary>
      <dl>
        <dt>{{ t("型号") }}</dt>
        <dd>{{ disk.model || "—" }}</dd>
        <template v-if="disk.vendor">
          <dt>{{ t("厂商") }}</dt>
          <dd>{{ disk.vendor }}</dd>
        </template>
        <dt>{{ t("序列号") }}</dt>
        <dd class="mono">{{ disk.serial || "—" }}</dd>
        <dt>{{ t("固件") }}</dt>
        <dd class="mono">{{ disk.firmware || "—" }}</dd>
        <dt>TRIM</dt>
        <dd>{{ disk.trim == null ? "—" : disk.trim ? t("支持") : t("不支持") }}</dd>
        <template v-if="disk.health">
          <dt>{{ t("健康记录") }}</dt>
          <dd>{{ disk.health.source === "nvme" ? t("NVMe SMART / 健康日志") : t("ATA 设备统计日志") }}</dd>
          <template v-if="disk.health.power_on_hours != null">
            <dt>{{ t("通电时间") }}</dt>
            <dd>{{ t("{n} 小时", { n: fmtInt(disk.health.power_on_hours) }) }}</dd>
          </template>
          <template v-if="disk.health.power_cycles != null">
            <dt>{{ t("通电次数") }}</dt>
            <dd>{{ fmtInt(disk.health.power_cycles) }}</dd>
          </template>
          <template v-if="disk.health.unsafe_shutdowns != null">
            <dt>{{ t("不安全关机") }}</dt>
            <dd>{{ fmtInt(disk.health.unsafe_shutdowns) }}</dd>
          </template>
          <template v-if="disk.health.media_errors != null">
            <dt>{{ t("介质错误") }}</dt>
            <dd>{{ fmtInt(disk.health.media_errors) }}</dd>
          </template>
          <template v-if="disk.health.available_spare != null">
            <dt>{{ t("备用块") }}</dt>
            <dd>{{ t("{pct}（低于 {low} 时报警）", { pct: `${disk.health.available_spare}%`, low: `${disk.health.spare_threshold ?? 0}%` }) }}</dd>
          </template>
        </template>
        <dt>{{ t("系统名称") }}</dt>
        <dd class="mono">{{ disk.id }}</dd>
      </dl>
    </details>
  </section>
</template>

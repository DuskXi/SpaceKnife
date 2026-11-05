<!-- A volume on 「本机磁盘」: how full it is, what SpaceKnife knows of it (an open scan or its
     snapshots: classes, largest folders, growth) and the way to it (view, open the snapshot, scan). -->
<script setup lang="ts">
import { computed } from "vue";

import Meter from "@/components/ui/charts/Meter.vue";
import Sparkline from "@/components/ui/charts/Sparkline.vue";
import StackBar from "@/components/ui/charts/StackBar.vue";
import Icon from "@/components/ui/Icon.vue";
import { classHexNow } from "@/lib/classes";
import { fmtAgo, fmtDelta, fmtInt, fmtShare, fmtSize, fmtSizeAs } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { className, FULL, scanSummary, seconds } from "@/lib/machinePage";
import type { VolumeOverview } from "@/protocol/generated";
import { activate, newStartTab, openSnapshot } from "@/state/actions";
import { growth } from "@/state/machine";
import { get, patchStartForm, useApp } from "@/state/store";

const props = defineProps<{ v: VolumeOverview }>();

const tabs = useApp((s) => s.tabs);
const vocab = useApp((s) => s.vocab); // class names and colours follow the vocabulary

const vol = computed(() => props.v.volume);
const share = computed(() => (vol.value.total ? Math.min(1, vol.value.used / vol.value.total) : 0));
const g = computed(() => growth(props.v.history));
const classes = computed(() => {
  void vocab.value;
  const s = props.v.scan;
  if (!s) return [];
  return s.classes.map((c) => ({ ...c, name: className(c.class_id), color: classHexNow(c.class_id) }));
});
const segments = computed(() => {
  const s = props.v.scan;
  return s ? classes.value.map((c) => ({ key: c.class_id, share: c.bytes / s.bytes_alloc, color: c.color, title: `${c.name} · ${fmtSize(c.bytes, true)}` })) : [];
});
const history = computed(() => {
  const h = props.v.history;
  if (!h.length) return null;
  return {
    values: [...h].reverse().map((x) => x.bytes_alloc),
    title: t("最近 {n} 次快照：{from} → {to}", { n: h.length, from: fmtSize(h[h.length - 1]!.bytes_alloc, true), to: fmtSize(h[0]!.bytes_alloc, true) }),
  };
});

// the way to the volume: its open tab, its snapshot, or a scan
const openTab = computed(() => {
  const id = props.v.scan?.scan_id;
  return id ? tabs.value.find((x) => x.scanId === id) : undefined;
});

/** A start tab with this volume picked: the scan options stay the user's to check. */
function scanVolume(mount: string): void {
  newStartTab();
  patchStartForm(get().active, { picked: mount, path: "", check: null });
}
</script>

<template>
  <div class="mc-vol">
    <div class="mc-vol-top">
      <div class="mc-vol-name">
        <span class="mono">{{ vol.mount_point }}</span>
        <span v-if="vol.label" class="mc-vol-label">{{ vol.label }}</span>
        <span class="muted small">{{ [vol.fs_type, vol.remote_path].filter(Boolean).join(" · ") }}</span>
      </div>
      <div class="mc-vol-act">
        <button v-if="openTab" class="ghost small" @click="activate(openTab.id)">{{ t("查看") }}</button>
        <button v-else-if="v.scan?.snapshot && !v.scan.scan_id" class="ghost small" @click="openSnapshot(v.scan.snapshot)">
          <Icon name="snapshot" :size="13" />
          {{ t("打开快照") }}
        </button>
        <button v-else-if="!vol.reason" class="ghost small" @click="scanVolume(vol.mount_point)">{{ t("扫描…") }}</button>
      </div>
    </div>
    <div class="mc-vol-use">
      <Meter :share="share" :tone="share >= FULL ? 'warn' : 'muted'" :title="vol.total ? t('已用 {pct}', { pct: fmtShare(vol.used, vol.total) }) : undefined" />
      <span class="mono small">
        {{ vol.total ? `${fmtSizeAs(vol.used, vol.total)} / ${fmtSize(vol.total, true)}` : "—"
        }}<span v-if="vol.total > 0" :class="share >= FULL ? 'mc-full' : 'muted'"> · {{ fmtShare(vol.used, vol.total) }}</span>
      </span>
    </div>
    <div class="mc-vol-scan">
      <div class="mc-scan-text">
        <span v-if="!v.scan" class="muted">{{ vol.reason || t("还没有扫描过") }}</span>
        <template v-else>
          <span>{{ scanSummary(v.scan) }}</span>
          <template v-if="v.scan.classes.length > 0 && v.scan.bytes_alloc > 0">
            <StackBar :height="6" :title="t('文件类别')" :segments="segments" />
            <span class="mc-classes">
              <span v-for="c in classes" :key="c.class_id" class="mc-class" :title="`${c.name} · ${t('{n} 个文件', { n: fmtInt(c.files) })}`">
                <span class="chip-swatch" :style="{ background: c.color }" />
                {{ c.name }} {{ fmtSize(c.bytes, true) }}
              </span>
            </span>
          </template>
          <span v-if="v.scan.folders.length > 0" class="muted small">
            {{ t("最大的文件夹：{list}", { list: v.scan.folders.map((f) => `${f.name} ${fmtSize(f.bytes, true)}`).join(byLang("、", ", ")) }) }}
          </span>
        </template>
        <span v-if="g && history" class="mc-growth">
          <Sparkline :values="history.values" :title="history.title" />
          <span class="small muted">{{ t("比 {when} 的快照 {delta}", { when: fmtAgo(seconds(g.since)), delta: fmtDelta(g.delta) }) }}</span>
        </span>
      </div>
    </div>
  </div>
</template>

<!-- The start page's side (UI-W1-38…46): the machine card, the recent scans with their snapshots
     (open, compare with now), and opening a snapshot file. -->
<script setup lang="ts">
import { computed, ref } from "vue";

import Icon from "@/components/ui/Icon.vue";
import MachineCard from "@/components/pages/MachineCard.vue";
import { fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { fmtWhen, recentRecords, type RecentRecord } from "@/lib/volumes";
import { openSnapshot, refreshLists } from "@/state/actions";
import { compareWithNow } from "@/state/compare";
import { type Tab, useApp } from "@/state/store";

const props = defineProps<{ tab: Tab }>();

const snapshots = useApp((s) => s.snapshots);
const volumes = useApp((s) => s.volumes);
const snapDir = useApp((s) => s.settings?.snapshots ?? "");
const all = ref(false);
const menu = ref(false);
// the names are in the interface language: t() is reactive, so the list follows a language change
const records = computed(() => recentRecords(snapshots.value, volumes.value, all.value));
const shown = computed(() => (all.value ? records.value : records.value.slice(0, 4)));

const sub = (r: RecentRecord) =>
  r.volume
    ? t("{size} · {method} · {files} 个文件", { size: fmtSize(r.snap.bytes_alloc), method: r.snap.method === "mft" ? t("MFT 快扫") : t("硬扫描"), files: fmtInt(r.snap.files) })
    : `${r.snap.roots.join("; ")} · ${fmtSize(r.snap.bytes_alloc)}`;

function refresh(): void {
  menu.value = false;
  void refreshLists();
}
function openFolder(): void {
  menu.value = false;
  void window.sk.shell.openPath(snapDir.value);
}
async function openFile(): Promise<void> {
  const path = await window.sk.dialog.openSnapshot();
  if (path) await openSnapshot(path, props.tab.id);
}
</script>

<template>
  <aside class="start-side" :aria-label="t('最近')">
    <MachineCard />
    <div class="start-title">
      <h2>
        {{ t("最近") }} <span class="mono muted">{{ records.length }}</span>
      </h2>
      <div class="menu-anchor">
        <button class="icon-btn" :aria-label="t('最近记录选项')" aria-haspopup="menu" :aria-expanded="menu" @click="menu = !menu">
          <Icon name="more" />
        </button>
        <div v-if="menu" class="menu" role="menu" @mouseleave="menu = false">
          <button role="menuitem" class="menu-item" @click="refresh">
            <span class="menu-check" />
            {{ t("刷新列表") }}
          </button>
          <button role="menuitem" class="menu-item" :disabled="!snapDir" @click="openFolder">
            <span class="menu-check" />
            {{ t("打开快照文件夹") }}
          </button>
        </div>
      </div>
    </div>
    <div class="recent-card">
      <p v-if="records.length === 0" class="muted recent-empty">{{ t("还没有扫描记录。每次扫描完成后会自动保存快照，之后可以秒开。") }}</p>
      <div :class="all ? 'recent-list all' : 'recent-list'">
        <div v-for="r in shown" :key="r.key" class="recent">
          <span class="recent-icon"><Icon :name="r.volume ? 'drive' : 'folder'" :size="16" /></span>
          <div class="recent-body">
            <div class="recent-line">
              <span class="recent-name">{{ r.name }}</span>
              <span class="muted small mono">{{ fmtWhen(r.snap.created_at) }}</span>
            </div>
            <div class="recent-line muted small">
              <span class="recent-sub">{{ sub(r) }}</span>
              <span class="recent-badge">
                <Icon name="snapshot" :size="12" />
                {{ t("有快照") }}
              </span>
            </div>
            <div class="recent-actions">
              <button class="ghost tiny" @click="openSnapshot(r.snap.path, tab.id)">{{ t("打开") }}</button>
              <button class="link small" :title="t('与这个位置现在的状态对比（需要时先扫描）')" @click="compareWithNow(r.snap, tab.id)">
                <Icon name="diff" :size="13" /> {{ t("与现在对比") }}
              </button>
            </div>
          </div>
        </div>
      </div>
      <button v-if="records.length > 4 || all" class="recent-all" @click="all = !all">
        {{ all ? t("只看最近 4 条") : t("查看全部扫描记录") }} <Icon name="chevronRight" :size="12" />
      </button>
    </div>
    <div class="drop-zone">
      <span class="drop-icon"><Icon name="upload" :size="18" /></span>
      <div class="strong">{{ t("把快照文件拖到这里") }}</div>
      <div class="muted small">{{ t("打开后可直接浏览，或与现在的磁盘对比") }}</div>
      <button class="ghost small" @click="openFile">
        <Icon name="snapshot" :size="14" />
        {{ t("打开快照文件…") }}
      </button>
    </div>
  </aside>
</template>

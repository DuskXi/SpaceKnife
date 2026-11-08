<!-- 扫描统计 (UI-W8-33, W8 待定 4): the scan of the active tab, live while it runs (LogPanel.vue). -->
<script setup lang="ts">
import { computed } from "vue";

import Kbd from "@/components/ui/Kbd.vue";
import { fmtDateTime, fmtDuration, fmtInt, fmtSize } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { activeTab, set, useApp } from "@/state/store";

const tab = useApp((s) => activeTab(s));
const scan = useApp((s) => (tab.value.scanId ? s.scans[tab.value.scanId] : undefined));

/** [label, value]; a null value is the 「无法访问」 link, "pending" the M4 note */
const rows = computed(() => {
  const s = scan.value;
  if (!s) return null;
  const running = s.state === "scanning" || s.state === "queued";
  const state = { queued: t("排队中"), scanning: t("扫描中"), done: t("已完成"), cancelled: t("已停止（结果保留）"), failed: t("失败") }[s.state];
  const left: [string, string][] = [
    // 「快照」: the one snapshot this tab shows (as a log source and elsewhere: the snapshots, plural)
    [t("扫描方式"), s.source === "snapshot" ? byLang("快照", "Snapshot") : s.method === "mft" ? t("MFT 快速扫描") : t("硬扫描")],
    [t("状态"), s.paused && running ? t("已暂停") : state],
    [t("开始"), s.started_at ? fmtDateTime(Date.parse(s.started_at) / 1000) : "—"],
    [t("结束"), s.finished_at ? fmtDateTime(Date.parse(s.finished_at) / 1000) : running ? t("进行中") : "—"],
    [t("耗时"), fmtDuration(s.elapsed_s)],
    [t("文件 / 文件夹"), `${fmtInt(s.files)} / ${fmtInt(s.dirs)}`],
  ];
  const right: [string, string][] = [
    [t("已扫描合计"), fmtSize(s.bytes_alloc)],
    [t("未知空间"), s.unknown != null ? fmtSize(s.unknown) : t("—（不是整卷扫描）")],
    [
      t("平均吞吐"),
      s.elapsed_s > 0 ? t("{rate} 项/秒 · {size}/秒", { rate: fmtInt(Math.round(s.items / s.elapsed_s)), size: fmtSize(s.bytes_alloc / s.elapsed_s) }) : "—",
    ],
  ];
  return { left, right, errors: s.errors };
});
</script>

<template>
  <div v-if="!rows" class="log-empty">{{ t("当前标签没有扫描") }}</div>
  <div v-else class="log-stats">
    <dl>
      <div v-for="[k, v] in rows.left" :key="k">
        <dt>{{ k }}</dt>
        <dd>{{ v }}</dd>
      </div>
    </dl>
    <dl>
      <div v-for="[k, v] in rows.right.slice(0, 2)" :key="k">
        <dt>{{ k }}</dt>
        <dd>{{ v }}</dd>
      </div>
      <div>
        <dt>{{ t("无法访问") }}</dt>
        <dd>
          <button v-if="rows.errors > 0" class="link" @click="set({ logTab: 'log' })">{{ t("{n} 个文件夹", { n: fmtInt(rows.errors) }) }}</button>
          <template v-else>0</template>
        </dd>
      </div>
      <div v-for="[k, v] in rows.right.slice(2)" :key="k">
        <dt>{{ k }}</dt>
        <dd>{{ v }}</dd>
      </div>
      <div>
        <dt>{{ t("磁盘负载历史") }}</dt>
        <dd>
          <span class="muted">{{ t("M4 接入") }}</span>
        </dd>
      </div>
    </dl>
    <div class="log-stats-hint muted small"><Kbd>Ctrl+J</Kbd> {{ t("显示 / 隐藏日志面板") }}</div>
  </div>
</template>

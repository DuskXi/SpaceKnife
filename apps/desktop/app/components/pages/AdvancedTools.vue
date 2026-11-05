<!-- 「设置 › 高级」 tools (UI-W7-36): settings export / import (also SpaceSniffer's
     SpaceSnifferConfig.xml, previewed before anything changes), restore everything, the scan helper,
     the snapshots and the cache. 「标签」's clear-all is ClearTags.vue. -->
<script setup lang="ts">
import { shallowRef } from "vue";

import ConfirmButton from "@/components/pages/ConfirmButton.vue";
import ImportPreview from "@/components/pages/ImportPreview.vue";
import SetRow from "@/components/pages/SetRow.vue";
import Icon from "@/components/ui/Icon.vue";
import { fail, useMaintenance } from "@/composables/useMaintenance";
import { fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import type { SettingsImportResult } from "@/protocol/generated";
import { call, resetSettings } from "@/state/actions";
import { toast } from "@/state/store";

const pad = (n: number) => String(n).padStart(2, "0");
const [info, refresh] = useMaintenance();
const preview = shallowRef<{ path: string; plan: SettingsImportResult } | null>(null);

async function exportSettings(): Promise<void> {
  const d = new Date();
  const path = await window.sk.dialog.saveFile({
    title: t("导出设置"),
    defaultName: t("SpaceKnife-设置-{date}.json", { date: `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}` }),
    filters: [{ name: t("SpaceKnife 设置"), extensions: ["json"] }],
  });
  if (!path) return;
  try {
    await call("settings.export", { path });
    toast(t("设置已导出"));
  } catch (err) {
    fail(err);
  }
}

async function importFrom(spacesniffer: boolean): Promise<void> {
  const path = await window.sk.dialog.openFile({
    title: spacesniffer ? t("选择 SpaceSnifferConfig.xml") : t("导入设置"),
    filters: spacesniffer ? [{ name: t("SpaceSniffer 配置"), extensions: ["xml"] }] : [{ name: t("SpaceKnife 设置"), extensions: ["json"] }],
  });
  if (!path) return;
  try {
    preview.value = { path, plan: await call("settings.import", { path, apply: false }) };
  } catch (err) {
    fail(err);
  }
}

async function apply(): Promise<void> {
  if (!preview.value) return;
  try {
    const r = await call("settings.import", { path: preview.value.path, apply: true });
    preview.value = null;
    const n = fmtInt(r.changes.length);
    toast(r.classes ? t("已导入 {n} 项设置和文件类别", { n }) : t("已导入 {n} 项设置", { n }));
  } catch (err) {
    fail(err);
  }
}

function resetAll(): void {
  void resetSettings(null);
  void call("classes.reset", {}).catch(fail);
  toast(t("已全部恢复默认"));
}

const helperText = () =>
  info.value
    ? info.value.helper_running
      ? info.value.helper_elevated
        ? t("运行中（管理员权限）：MFT 快速扫描与 USN 日志由它读取")
        : t("运行中：MFT 快速扫描与 USN 日志由它读取")
      : t("未运行")
    : "…";
const openSnapshots = () => info.value && void window.sk.shell.openPath(info.value.snapshots_dir);
</script>

<template>
  <section class="set-group" :aria-label="t('设置文件')">
    <div class="set-group-head">
      <h2>{{ t("导入与导出") }}</h2>
    </div>
    <div class="set-card">
      <SetRow :name="t('导出设置')" :desc="t('所有改过的设置和自定义的文件类别，存成一个文件')">
        <button class="ghost small" @click="exportSettings"><Icon name="upload" :size="14" /> {{ t("导出…") }}</button>
      </SetRow>
      <SetRow :name="t('导入设置')" :desc="t('先列出会改变的项，确认后才生效')">
        <button class="ghost small" @click="importFrom(false)">{{ t("导入…") }}</button>
      </SetRow>
      <SetRow :name="t('从 SpaceSniffer 导入')" :desc="t('读取 SpaceSnifferConfig.xml：几何、动画、行为、外观和文件类别')">
        <button class="ghost small" @click="importFrom(true)">{{ t("选择配置文件…") }}</button>
      </SetRow>
      <SetRow :name="t('全部恢复默认')" :desc="t('所有设置和文件类别回到初始值')">
        <ConfirmButton :label="t('全部恢复默认…')" :ask="t('所有设置都会恢复默认，确定？')" @yes="resetAll" />
      </SetRow>
    </div>
  </section>
  <section class="set-group" :aria-label="t('扫描助手与缓存')">
    <div class="set-group-head">
      <h2>{{ t("扫描助手与缓存") }}</h2>
    </div>
    <div class="set-card">
      <SetRow :name="t('扫描助手')" :desc="helperText()">
        <button class="ghost small" :disabled="!info?.helper_running" @click="refresh(call('system.helper_stop', {}))">{{ t("停止扫描助手") }}</button>
      </SetRow>
      <SetRow
        :name="t('快照')"
        :desc="info ? t('{n} 个快照，共 {size} · {dir}', { n: fmtInt(info.snapshot_count), size: fmtSize(info.snapshots_bytes), dir: info.snapshots_dir }) : '…'"
      >
        <button class="ghost small" :disabled="!info" @click="openSnapshots"><Icon name="reveal" :size="14" /> {{ t("打开") }}</button>
      </SetRow>
      <SetRow :name="t('缓存')" :desc="info ? `${fmtSize(info.cache_bytes)} · ${info.cache_dir}` : '…'">
        <ConfirmButton :label="t('清除缓存…')" :ask="t('正在使用的缓存会保留，确定清除？')" :disabled="!info?.cache_bytes" @yes="refresh(call('system.cache_clear', {}))" />
      </SetRow>
    </div>
  </section>
  <ImportPreview v-if="preview" :preview="preview" @apply="apply" @cancel="preview = null" />
</template>

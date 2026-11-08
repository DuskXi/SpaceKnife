<!-- 「高级」's files and diagnostics: the settings file, the log folder, and the diagnostics copied
     for a bug report. -->
<script setup lang="ts">
import SetRow from "@/components/pages/SetRow.vue";
import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { call } from "@/state/actions";
import { toast, useApp } from "@/state/store";

const state = useApp((s) => s.settings);
const version = useApp((s) => s.serverVersion);

const openPath = (path: string) => void window.sk.shell.openPath(path);

async function copyDiagnostics(): Promise<void> {
  const st = state.value;
  if (!st) return;
  const info = await call("system.info", {}).catch(() => null);
  const lines = [
    t("SpaceKnife 前端 {frontend} · 后端 {backend}", { frontend: window.sk.version, backend: version.value }),
    t("平台 {platform} · {agent}", { platform: window.sk.platform, agent: navigator.userAgent }),
    info ? t("后端 {info}", { info: JSON.stringify(info) }) : t("后端信息不可用"),
    t("设置文件 {path}", { path: st.path }),
    t("日志目录 {path}", { path: st.logs }),
  ];
  window.sk.shell.copyText(lines.join("\n"));
  toast(t("诊断信息已复制"));
}
</script>

<template>
  <section v-if="state" class="set-group" :aria-label="t('文件与诊断')">
    <div class="set-group-head">
      <h2>{{ t("文件与诊断") }}</h2>
    </div>
    <div class="set-card">
      <div v-for="f in [{ name: t('设置文件'), path: state.path }, { name: t('日志文件夹'), path: state.logs }]" :key="f.path" class="set-row">
        <div class="set-text">
          <div class="set-name">{{ f.name }}</div>
          <div class="set-desc mono" :title="f.path">{{ f.path }}</div>
        </div>
        <div class="set-control">
          <button class="ghost small" @click="openPath(f.path)">
            <Icon name="reveal" :size="14" />
            {{ t("打开") }}
          </button>
        </div>
      </div>
      <SetRow :name="t('复制诊断信息')" :desc="t('版本、平台与能力清单，用于反馈问题')">
        <button class="ghost small" @click="copyDiagnostics">
          <Icon name="copy" :size="14" />
          {{ t("复制") }}
        </button>
      </SetRow>
    </div>
  </section>
</template>

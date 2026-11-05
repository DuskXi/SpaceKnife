<!-- The top of 「关于与更新」: version, install, what the last update check found and when the next one
     comes; the update card itself when there is a newer release. -->
<script setup lang="ts">
import { computed } from "vue";

import UpdateCard from "@/components/shell/UpdateCard.vue";
import Icon from "@/components/ui/Icon.vue";
import { fmtAgo, fmtDateTime } from "@/lib/format";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import { seconds } from "@/lib/machinePage";
import { checkUpdates } from "@/state/actions";
import { useApp } from "@/state/store";
import { hasUpdate, installKindName, seriesName } from "@/state/updates";

const st = useApp((s) => s.update);
const conn = useApp((s) => s.conn);
const version = window.sk.version;
const kind = computed(() => st.value?.install_kind ?? "unknown");
const checked = computed(() => seconds(st.value?.checked_at));
const next = computed(() => seconds(st.value?.next_check_at));
const newer = computed(() => {
  const s = st.value;
  return hasUpdate(s) ? s : null;
});
const status = computed(() => {
  const s = st.value;
  if (!s) return t("这个后端不支持更新检查");
  if (s.checking) return t("正在检查更新…");
  if (s.error) return t("上次检查失败：{error}", { error: s.error });
  if (hasUpdate(s))
    return s.muted ? t("有新版本 {version}（已跳过 {series} 的提示）", { version: s.latest.version, series: seriesName(s.series ?? "") }) : t("有新版本 {version}", { version: s.latest.version });
  return checked.value ? t("已是最新版本") : t("还没有检查过更新");
});
const statusClass = computed(() => (st.value?.error ? " warn" : newer.value ? " new" : checked.value ? "" : " idle"));
const statusIcon = computed<IconName>(() => (st.value?.error ? "warning" : newer.value ? "up" : checked.value ? "check" : "info"));
const openReleases = () => window.sk.shell.openExternal("https://github.com/DuskXi/SpaceKnife/releases");
</script>

<template>
  <section class="about-head" :aria-label="t('版本')">
    <span class="about-logo"><Icon name="logo" :size="28" /></span>
    <div class="about-title">
      <h2>SpaceKnife {{ version }}</h2>
      <div class="about-sub">
        <span>{{ installKindName(kind) }}</span>
        <span v-if="st" class="uc-dot" aria-hidden="true" />
        <span v-if="st">{{ st.channel === "stable" ? t("只要正式版") : t("包括预览版") }}</span>
      </div>
      <div :class="`about-status${statusClass}`" aria-live="polite">
        <Icon :name="statusIcon" :size="14" />
        {{ status }}
      </div>
      <div class="about-times muted small">
        <span v-if="checked > 0" :title="fmtDateTime(checked)">{{ t("上次检查：{when}", { when: fmtAgo(checked) }) }}</span>
        <span v-if="next > 0">{{ t("下次自动检查：{when}", { when: fmtDateTime(next) }) }}</span>
        <span v-if="st && !st.enabled">{{ t("自动检查已关闭") }}</span>
      </div>
    </div>
    <div class="about-actions">
      <button class="ghost" :disabled="!st || st.checking || conn !== 'ready'" @click="checkUpdates()">
        <Icon name="refresh" :size="14" />
        {{ t("检查更新") }}
      </button>
      <button class="ghost" @click="openReleases">
        <Icon name="reveal" :size="14" />
        {{ t("全部发布") }}
      </button>
    </div>
  </section>
  <div v-if="newer" class="about-update">
    <UpdateCard :status="newer" placement="inline" />
  </div>
</template>

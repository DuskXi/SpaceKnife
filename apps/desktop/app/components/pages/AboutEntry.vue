<!-- 「常规」's way to 「关于与更新」: the version and whether a newer one is known, one click away. -->
<script setup lang="ts">
import { computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { settingsGo } from "@/state/actions";
import { useApp } from "@/state/store";
import { hasUpdate } from "@/state/updates";

const st = useApp((s) => s.update);
const version = window.sk.version;
const desc = computed(() => {
  const s = st.value;
  return hasUpdate(s) ? t("有新版本 {version}", { version: s.latest.version }) : t("版本信息、更新设置与发布时间线");
});
</script>

<template>
  <section class="set-group" :aria-label="t('关于')">
    <div class="set-group-head">
      <h2>{{ t("关于") }}</h2>
    </div>
    <div class="set-card">
      <div class="set-row">
        <div class="set-text">
          <div class="set-name">SpaceKnife {{ version }}</div>
          <div class="set-desc">{{ desc }}</div>
        </div>
        <div class="set-control">
          <button class="ghost" @click="settingsGo('about')">
            {{ t("关于与更新") }}
            <Icon name="chevronRight" :size="14" />
          </button>
        </div>
      </div>
    </div>
  </section>
</template>

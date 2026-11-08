<!-- Settings tab (W7, UI-W7-01…37). Every row is rendered from the backend's metadata
     (settings.get): name, description, control, options, range, when it applies and whether it can
     be changed here. Changes save immediately; the backend validates and broadcasts them. The pages
     and the search are lib/settingsPages.ts; the toolbar's search box is SettingsSearch.vue. -->
<script setup lang="ts">
import { computed } from "vue";

import AboutComponents from "@/components/pages/AboutComponents.vue";
import AboutEntry from "@/components/pages/AboutEntry.vue";
import AboutHeader from "@/components/pages/AboutHeader.vue";
import AboutTimeline from "@/components/pages/AboutTimeline.vue";
import AdvancedTools from "@/components/pages/AdvancedTools.vue";
import ClassesEditor from "@/components/pages/ClassesEditor.vue";
import ClearTags from "@/components/pages/ClearTags.vue";
import SettingsFiles from "@/components/pages/SettingsFiles.vue";
import SettingsFooter from "@/components/pages/SettingsFooter.vue";
import SettingsGroup from "@/components/pages/SettingsGroup.vue";
import SettingsKeys from "@/components/pages/SettingsKeys.vue";
import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { t } from "@/lib/i18n";
import { GROUPS, groupBy, pageOf, PAGES, searchSettings } from "@/lib/settingsPages";
import type { SettingMeta } from "@/protocol/generated";
import { settingsGo } from "@/state/actions";
import { useApp } from "@/state/store";

const EMPTY_META: SettingMeta[] = [];

const state = useApp((s) => s.settings);
const page = useApp((s) => s.settingsPage);
const query = useApp((s) => s.settingsQuery);
const conn = useApp((s) => s.conn);
const meta = computed(() => state.value?.meta ?? EMPTY_META);
// the search also matches the page names, in the interface language (t() is reactive)
const hits = computed(() => (query.value.trim() ? searchSettings(meta.value, query.value.trim()) : null));
const counts = computed(() => {
  const m = new Map<string, number>();
  for (const h of hits.value ?? []) m.set(h.category, (m.get(h.category) ?? 0) + 1);
  return m;
});
const navPage = (id: string) => PAGES.find((x) => x.id === id)!;

// the page shown
const p = computed(() => pageOf(page.value));
const rows = computed(() => meta.value.filter((m) => m.category === p.value.id));
const groups = computed(() => groupBy(rows.value));

// the search results, by page then group
const byPage = computed(() => (hits.value ? PAGES.map((x) => [x, hits.value!.filter((h) => h.category === x.id)] as const).filter(([, h]) => h.length) : []));
// the key cap sits inside the sentence: split the translated text around {key}
const resultsLine = computed(() => t("{n} 项设置 · 按 {key} 清空", { n: hits.value?.length ?? 0 }).split("{key}"));

const openFile = () => state.value && void window.sk.shell.openPath(state.value.path);
</script>

<template>
  <div class="settings">
    <nav class="settings-nav" :aria-label="t('设置分类')">
      <div class="settings-nav-list">
        <div v-for="(group, gi) in GROUPS" :key="gi" class="settings-nav-group">
          <button
            v-for="id in group"
            :key="id"
            :class="['settings-nav-item', { on: !hits && page === id, faded: hits && !(counts.get(id) ?? 0) }]"
            :aria-current="!hits && page === id ? 'page' : undefined"
            @click="settingsGo(id)"
          >
            <Icon :name="navPage(id).icon" />
            <span class="flex">{{ navPage(id).name }}</span>
            <span v-if="hits && (counts.get(id) ?? 0) > 0" class="settings-hit-count mono">{{ counts.get(id) }}</span>
          </button>
        </div>
      </div>
      <button class="settings-nav-file" :disabled="!state" @click="openFile">
        <Icon name="reveal" :size="14" />
        {{ t("打开设置文件") }}
      </button>
    </nav>
    <div class="settings-main">
      <div class="settings-scroll">
        <div v-if="conn !== 'ready'" class="settings-banner warn">{{ t("与后端的连接已断开，设置暂不可修改") }}</div>
        <div v-if="state?.error" class="settings-banner warn">{{ state.error }}</div>
        <div v-if="!state" class="muted">{{ t("正在读取设置…") }}</div>
        <template v-else-if="hits">
          <div v-if="!hits.length" class="settings-empty muted">{{ t("没有找到与「{query}」相关的设置", { query: query.trim() }) }}</div>
          <template v-else>
            <header class="settings-head">
              <h1>{{ t("搜索结果") }}</h1>
              <p class="muted">
                {{ resultsLine[0] }}<Kbd>Esc</Kbd>{{ resultsLine[1] }}
              </p>
            </header>
            <div v-for="[pg, found] in byPage" :key="pg.id" class="settings-result-page">
              <SettingsGroup v-for="[group, items] in groupBy(found)" :key="group" :title="`${pg.name} › ${group}`" note="" :rows="items" :query="query.trim()" />
            </div>
          </template>
        </template>
        <template v-else>
          <header class="settings-head">
            <h1>{{ p.name }}</h1>
            <p class="muted">{{ p.note }}</p>
          </header>
          <AboutHeader v-if="p.id === 'about'" />
          <ClassesEditor v-if="p.id === 'classes'" />
          <SettingsKeys v-if="p.id === 'keys'" :rows="rows" />
          <div v-if="p.id !== 'keys'" class="settings-columns">
            <SettingsGroup v-for="[group, items] in groups" :key="group" :title="group" :note="items[0]?.group_note ?? ''" :rows="items" />
            <ClearTags v-if="p.id === 'tags'" />
            <AdvancedTools v-if="p.id === 'advanced'" />
            <SettingsFiles v-if="p.id === 'advanced'" />
            <AboutComponents v-if="p.id === 'about'" />
            <AboutEntry v-if="p.id === 'general'" />
          </div>
          <AboutTimeline v-if="p.id === 'about'" />
        </template>
      </div>
      <SettingsFooter :page="page" />
    </div>
  </div>
</template>

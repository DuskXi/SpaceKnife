<!-- Toolbar search box of the settings tab (UI-W7-05). -->
<script setup lang="ts">
import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { set, useApp } from "@/state/store";

const query = useApp((s) => s.settingsQuery);

function onKey(e: KeyboardEvent): void {
  e.stopPropagation();
  if (e.key === "Escape") {
    if (query.value) set({ settingsQuery: "" });
    else (e.target as HTMLInputElement).blur();
  }
}
</script>

<template>
  <div class="filter settings-search">
    <span class="filter-icon"><Icon name="search" /></span>
    <input
      id="sk-settings-search"
      class="filter-input plain"
      :placeholder="t('搜索设置')"
      :aria-label="t('搜索设置')"
      :value="query"
      spellcheck="false"
      @input="set({ settingsQuery: ($event.target as HTMLInputElement).value })"
      @keydown="onKey"
    />
    <kbd class="kbd filter-kbd">/</kbd>
  </div>
</template>

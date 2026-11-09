<!-- Toolbar search box of the help tab: titles, descriptions and headings. Enter opens the best hit. -->
<script setup lang="ts">
import Icon from "@/components/ui/Icon.vue";
import { docLang, search } from "@/help/docs";
import { t } from "@/lib/i18n";
import { helpGo } from "@/state/help";
import { get, set, useApp } from "@/state/store";

const query = useApp((s) => s.helpQuery);

function onKey(e: KeyboardEvent): void {
  e.stopPropagation();
  if (e.key === "Escape") {
    if (query.value) set({ helpQuery: "" });
    else (e.target as HTMLInputElement).blur();
  } else if (e.key === "Enter") {
    const q = query.value.trim();
    const best = q ? search(docLang(get().lang), q)[0] : undefined;
    if (!best) return;
    // a word only a heading has: that heading
    const inTitle = q
      .toLowerCase()
      .split(/\s+/)
      .every((w) => best.page.title.toLowerCase().includes(w));
    helpGo(best.page.id, inTitle ? null : (best.headings[0]?.id ?? null));
  }
}
</script>

<template>
  <div class="filter settings-search">
    <span class="filter-icon"><Icon name="search" /></span>
    <input
      id="sk-help-search"
      class="filter-input plain"
      :placeholder="t('搜索帮助')"
      :aria-label="t('搜索帮助')"
      :value="query"
      spellcheck="false"
      @input="set({ helpQuery: ($event.target as HTMLInputElement).value })"
      @keydown="onKey"
    />
    <kbd class="kbd filter-kbd">/</kbd>
  </div>
</template>

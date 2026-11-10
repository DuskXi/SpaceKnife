<!-- The recent releases, newest first, each with its notes in the interface language (the newest one
     open). The timeline comes from the backend's cache (update.releases): it is there offline too and
     fills in after the first check. -->
<script setup lang="ts">
import { shallowRef, watch, computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { render } from "@/help/markdown";
import { getLang, t } from "@/lib/i18n";
import type { UpdateInfo } from "@/protocol/generated";
import { call, checkUpdates } from "@/state/actions";
import { openHelpTab } from "@/state/help";
import { useApp } from "@/state/store";
import { isPreview, notesFor, releaseDate } from "@/state/updates";

const checkedAt = useApp((s) => s.update?.checked_at ?? null);
const current = useApp((s) => s.update?.current || window.sk.version);
const conn = useApp((s) => s.conn);
const releases = shallowRef<UpdateInfo[] | null>(null);
const open = shallowRef<Set<string>>(new Set());

watch(
  [checkedAt, conn],
  ([, c], _, onCleanup) => {
    if (c !== "ready") return;
    let alive = true;
    call("update.releases", {})
      .then((r) => {
        if (!alive) return;
        releases.value = r.releases;
        if (!open.value.size && r.releases[0]) open.value = new Set([r.releases[0].version]);
      })
      .catch(() => alive && (releases.value = []));
    onCleanup(() => (alive = false));
  },
  { immediate: true },
);
const here = computed(() => releases.value?.findIndex((r) => r.version === current.value) ?? -1);

function toggle(version: string): void {
  const next = new Set(open.value);
  if (!next.delete(version)) next.add(version);
  open.value = next;
}

// the notes of the open releases in the interface language (getLang() is reactive)
const notes = computed(() => {
  const lang = getLang();
  const out = new Map<string, string>();
  for (const r of releases.value ?? []) {
    if (open.value.has(r.version)) out.set(r.version, render(notesFor(r.notes, lang), lang === "zh-CN" ? "zh" : "en", "index").html);
  }
  return out;
});
const notesHtml = (r: UpdateInfo): string => notes.value.get(r.version) ?? "";

function onNotesClick(e: MouseEvent): void {
  const a = (e.target as HTMLElement).closest("a");
  if (!a) return;
  e.preventDefault();
  const doc = a.getAttribute("data-doc");
  if (doc) openHelpTab(doc, a.getAttribute("data-anchor"));
  else if (a.hasAttribute("data-external")) window.sk.shell.openExternal(a.getAttribute("href") ?? "");
}
function onAuxClick(e: MouseEvent): void {
  if ((e.target as HTMLElement).closest("a")) e.preventDefault();
}
const openUrl = (url: string) => window.sk.shell.openExternal(url);
</script>

<template>
  <section class="about-timeline" :aria-label="t('发布时间线')">
    <div class="set-group-head">
      <h2>{{ t("发布时间线") }}</h2>
      <span v-if="releases && releases.length > 0" class="muted small">{{ t("最近 {n} 个版本", { n: releases.length }) }}</span>
    </div>
    <div v-if="releases === null" class="muted small">{{ t("正在读取…") }}</div>
    <div v-else-if="releases.length === 0" class="set-card about-empty">
      <span class="muted">{{ t("还没有发布记录：检查一次更新后，这里会列出最近的版本和它们的更新内容") }}</span>
      <button class="ghost" :disabled="conn !== 'ready'" @click="checkUpdates()">{{ t("检查更新") }}</button>
    </div>
    <ol v-else class="tl">
      <li
        v-for="(r, i) in releases"
        :key="r.tag"
        :class="['tl-item', { current: r.version === current, newer: here > 0 && i < here }]"
      >
        <span class="tl-dot" aria-hidden="true" />
        <button class="tl-head" :aria-expanded="open.has(r.version)" :aria-controls="`tl-notes-${r.tag}`" @click="toggle(r.version)">
          <span class="tl-version">{{ r.version }}</span>
          <span v-if="r.version === current" class="tl-tag current">{{ t("当前") }}</span>
          <span v-if="here > 0 && i < here" class="tl-tag new">{{ t("新") }}</span>
          <span v-if="isPreview(r)" class="tl-tag">{{ t("预览版") }}</span>
          <span class="flex" />
          <span class="tl-date mono">{{ releaseDate(r.published_at) }}</span>
          <Icon :name="open.has(r.version) ? 'chevronDown' : 'chevronRight'" :size="14" />
        </button>
        <div v-if="open.has(r.version)" :id="`tl-notes-${r.tag}`" class="tl-notes">
          <!-- the release notes rendered by help/markdown.ts (our markup) -->
          <div v-if="notesHtml(r)" class="help-body" @click="onNotesClick" @auxclick="onAuxClick" v-html="notesHtml(r)" />
          <div v-else class="muted small">{{ t("这个版本没有发布说明") }}</div>
          <button class="link small" @click="openUrl(r.url)">
            {{ t("在 GitHub 上查看") }}
            <Icon name="reveal" :size="12" />
          </button>
        </div>
      </li>
    </ol>
  </section>
</template>

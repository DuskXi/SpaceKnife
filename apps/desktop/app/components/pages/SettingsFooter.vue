<!-- The settings tab's footer (UI-W7-22…24): whether changes are saved, restore the page's defaults
     (with an undo for a few seconds) and 「完成」. -->
<script setup lang="ts">
import { computed, onScopeDispose, shallowRef } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { pageName, type SettingValue } from "@/lib/settingsPages";
import { closeSettings, resetSettings, updateSettings } from "@/state/actions";
import { type SettingsPageId, useApp } from "@/state/store";

const props = defineProps<{ page: SettingsPageId }>();

const state = useApp((s) => s.settings);
const save = useApp((s) => s.settingsSave);
const query = useApp((s) => s.settingsQuery);
const undo = shallowRef<Record<string, SettingValue> | null>(null);
let timer: ReturnType<typeof setTimeout> | null = null;
onScopeDispose(() => void (timer && clearTimeout(timer)));

const changed = computed(() => {
  const st = state.value;
  if (!st) return [];
  return st.meta.filter((m) => m.category === props.page && m.enabled && st.values[m.key] !== st.defaults[m.key]).map((m) => m.key);
});

async function restore(): Promise<void> {
  const st = state.value;
  if (!st) return;
  const before = Object.fromEntries(changed.value.map((k) => [k, st.values[k]!]));
  await resetSettings(props.page);
  undo.value = before;
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => (undo.value = null), 5000);
}

function revert(): void {
  if (undo.value) void updateSettings(undo.value);
  undo.value = null;
}
</script>

<template>
  <footer class="settings-foot">
    <span v-if="save.state === 'saving'" class="muted">{{ t("正在保存…") }}</span>
    <span v-else-if="save.state === 'error'" class="warn settings-save">
      <Icon name="warning" :size="14" />
      {{ t("保存失败：{message}", { message: save.message }) }}
    </span>
    <span v-else class="muted settings-save">
      <Icon name="check" :size="14" />
      {{ t("更改已自动保存") }}
    </span>
    <span v-if="undo" class="settings-undo">
      {{ t("已恢复「{page}」的默认值", { page: pageName(page) }) }} ·
      <button class="link" @click="revert">{{ t("撤销") }}</button>
    </span>
    <span class="flex" />
    <button
      class="ghost"
      :disabled="!!query.trim() || changed.length === 0"
      :title="changed.length ? t('把「{page}」的 {n} 项恢复为默认值', { page: pageName(page), n: changed.length }) : t('当前页已经全部是默认值')"
      @click="restore"
    >
      {{ t("恢复默认") }}
    </button>
    <button class="primary small" @click="closeSettings()">{{ t("完成") }}</button>
  </footer>
</template>

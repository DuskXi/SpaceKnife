<!-- 「快捷键」: the page's own settings, then every command with its keys (the palette's list),
     searchable by name or key. -->
<script setup lang="ts">
import { computed, onMounted, ref, shallowRef } from "vue";

import SettingsGroup from "@/components/pages/SettingsGroup.vue";
import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { fmtKeys } from "@/lib/format";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import { groupBy } from "@/lib/settingsPages";
import type { PaletteCommand, SettingMeta } from "@/protocol/generated";
import { call } from "@/state/actions";

const props = defineProps<{ rows: SettingMeta[] }>();

const commands = shallowRef<PaletteCommand[] | null>(null);
const q = ref("");
onMounted(() => void call("palette.commands", {}).then((r) => (commands.value = r.commands)));
const groups = computed(() => groupBy(props.rows));
const shown = computed(() => (commands.value ?? []).filter((c) => !q.value || c.name.includes(q.value) || c.keys.some((k) => k.toLowerCase().includes(q.value.toLowerCase()))));
</script>

<template>
  <div class="settings-columns single">
    <SettingsGroup v-for="[group, items] in groups" :key="group" :title="group" note="" :rows="items" />
    <section class="set-group" :aria-label="t('键位表')">
      <div class="set-group-head">
        <h2>{{ t("键位表") }}</h2>
        <input class="set-text-input small" :placeholder="t('搜索命令或按键')" :value="q" @input="q = ($event.target as HTMLInputElement).value" @keydown.stop />
      </div>
      <div class="set-card keys-table">
        <div v-if="!commands" class="set-row muted">{{ t("正在读取…") }}</div>
        <div v-for="c in shown" :key="c.id" class="key-row">
          <Icon :name="(c.icon as IconName) || 'command'" :size="14" />
          <span class="flex">{{ c.name }}</span>
          <template v-if="c.keys.length">
            <Kbd v-for="k in c.keys" :key="k">{{ fmtKeys(k) }}</Kbd>
          </template>
          <span v-else class="muted small">{{ t("未绑定") }}</span>
        </div>
      </div>
    </section>
  </div>
</template>

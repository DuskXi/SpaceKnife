<!-- A setting's row: its name (the search's match marked), why it is off, its description, when a
     change applies, and its control. Changes save at once. -->
<script setup lang="ts">
import { computed } from "vue";

import FolderList from "@/components/pages/FolderList.vue";
import SettingControl from "@/components/pages/SettingControl.vue";
import { t } from "@/lib/i18n";
import { dependsOk, markOnce, type SettingValue } from "@/lib/settingsPages";
import type { SettingMeta } from "@/protocol/generated";
import { updateSettings } from "@/state/actions";
import { useApp } from "@/state/store";

const props = defineProps<{ meta: SettingMeta; query?: string }>();

const value = useApp((s) => s.settings?.values[props.meta.key]);
const values = useApp((s) => s.settings?.values);
const conn = useApp((s) => s.conn);
const scanning = useApp((s) => Object.values(s.scans).some((x) => x.state === "scanning" || x.state === "queued"));
const disabled = computed(() => !props.meta.enabled || !dependsOk(props.meta.depends, values.value) || conn.value !== "ready");
const id = computed(() => `set-${props.meta.key}`);
const name = computed(() => markOnce(props.meta.name, props.query));
const save = (v: SettingValue) => void updateSettings({ [props.meta.key]: v });
</script>

<template>
  <div :id="id" :class="['set-row', { disabled, wide: meta.control === 'folders' }]">
    <div class="set-text">
      <div class="set-name">
        {{ name[0] }}<mark v-if="name[1]">{{ name[1] }}</mark>{{ name[2] }}
        <span v-if="!meta.enabled && meta.reason" class="set-badge">{{ meta.reason }}</span>
      </div>
      <div :id="`${id}-desc`" class="set-desc">{{ meta.description }}</div>
      <div v-if="meta.applies === 'next_scan' && scanning && meta.enabled" class="set-note">{{ t("将在下一次扫描生效") }}</div>
      <div v-if="meta.applies === 'new_views' && meta.enabled" class="set-note subtle">{{ t("只影响之后新开的视图") }}</div>
    </div>
    <div class="set-control">
      <FolderList v-if="meta.control === 'folders'" :value="String(value ?? '')" :label="meta.name" :disabled="disabled" :described-by="`${id}-desc`" @save="save" />
      <SettingControl v-else :meta="meta" :value="value" :disabled="disabled" :described-by="`${id}-desc`" @save="save" />
    </div>
  </div>
</template>

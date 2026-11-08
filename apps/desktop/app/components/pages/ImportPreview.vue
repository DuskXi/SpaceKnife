<!-- What a settings import would change (settings.import with apply: false), confirmed before anything
     changes: the settings with their values before and after, the file classes, what is not imported. -->
<script setup lang="ts">
import { computed } from "vue";

import { fmtInt } from "@/lib/format";
import { t } from "@/lib/i18n";
import type { SettingsImportResult } from "@/protocol/generated";

const props = defineProps<{ preview: { path: string; plan: SettingsImportResult } }>();
const emit = defineEmits<{ apply: []; cancel: [] }>();

const plan = computed(() => props.preview.plan);
const nothing = computed(() => !plan.value.changes.length && !plan.value.classes);
const show = (v: unknown): string => (typeof v === "boolean" ? (v ? t("开") : t("关")) : String(v));
</script>

<template>
  <div class="modal-back" role="presentation" @mousedown="emit('cancel')">
    <div class="modal import-modal" role="dialog" aria-modal="true" aria-labelledby="import-title" @mousedown.stop>
      <h3 id="import-title">{{ plan.source === "spacesniffer" ? t("从 SpaceSniffer 导入") : t("导入设置") }}</h3>
      <p class="muted small mono">{{ preview.path }}</p>
      <div class="import-body">
        <p v-if="nothing">{{ t("没有需要改变的设置。") }}</p>
        <template v-if="plan.changes.length > 0">
          <div class="muted small">{{ t("将改变 {n} 项设置", { n: fmtInt(plan.changes.length) }) }}</div>
          <table class="import-table">
            <tbody>
              <tr v-for="c in plan.changes" :key="c.key">
                <td>{{ c.name }}</td>
                <td class="mono muted">{{ show(c.before) }}</td>
                <td class="muted">→</td>
                <td class="mono">{{ show(c.after) }}</td>
              </tr>
            </tbody>
          </table>
        </template>
        <template v-if="plan.classes">
          <div class="muted small">{{ t("文件类别将变为") }}</div>
          <ul class="import-classes">
            <li v-for="c in plan.classes" :key="c.id">
              <strong>{{ c.name }}</strong> <span class="muted small mono">{{ c.extensions.join(";") || t("未归类的扩展名") }}</span>
            </li>
          </ul>
        </template>
        <template v-if="plan.skipped.length > 0">
          <div class="muted small">{{ t("不导入") }}</div>
          <ul class="import-skipped muted small">
            <li v-for="s in plan.skipped" :key="s">{{ s }}</li>
          </ul>
        </template>
      </div>
      <div class="modal-buttons">
        <button class="ghost" @click="emit('cancel')">{{ t("取消") }}</button>
        <button class="primary" :disabled="nothing" @click="emit('apply')">{{ t("导入") }}</button>
      </div>
    </div>
  </div>
</template>

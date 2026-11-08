<!-- Breadcrumb with path editing (UI-MAIN-14…16): Ctrl+L (the shell sends sk:edit-path) or a click on
     the last segment edits the path; Enter goes there, Esc stops. -->
<script setup lang="ts">
import { computed, nextTick, onScopeDispose, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { type Crumb, crumbLabel, CRUMBS_MIN, fullPath } from "@/lib/toolbar";
import { call, viewCall } from "@/state/actions";
import { activeTab, useApp } from "@/state/store";

const props = defineProps<{ viewId: string | null }>();
const EMPTY: Crumb[] = [];
const view = useApp((s) => (props.viewId ? s.views[props.viewId] : undefined));
const tab = useApp((s) => activeTab(s));
const scan = useApp((s) => (tab.value.scanId ? s.scans[tab.value.scanId] : undefined));
const editing = ref(false);
const error = ref("");
const input = ref<HTMLInputElement | null>(null);
const crumbs = computed(() => view.value?.info?.breadcrumb ?? EMPTY);
const atRoot = computed(() => crumbs.value.length <= 1);
const whole = computed(() => scan.value?.roots.length === 1 && scan.value.roots[0]!.whole_volume);

const onEdit = () => (editing.value = true);
window.addEventListener("sk:edit-path", onEdit);
onScopeDispose(() => window.removeEventListener("sk:edit-path", onEdit));
watch(editing, async (now) => {
  if (!now) return;
  await nextTick();
  if (input.value) {
    input.value.value = fullPath(crumbs.value, tab.value.title);
    input.value.select();
  }
});

async function go(): Promise<void> {
  const path = input.value?.value.trim() ?? "";
  const scanId = tab.value.scanId;
  if (!path || !scanId) return;
  try {
    const found = await call("tree.find", { scan_id: scanId, path });
    if (!found.node) {
      error.value = t("路径不存在，或不在本次扫描范围内");
      return;
    }
    await viewCall(props.viewId, "view.zoom", { key: found.node.id });
    editing.value = false;
    error.value = "";
  } catch (e) {
    error.value = String(e);
  }
}

function onKey(e: KeyboardEvent): void {
  e.stopPropagation();
  if (e.key === "Enter") void go();
  if (e.key === "Escape") {
    editing.value = false;
    error.value = "";
  }
}
const onBlur = () => setTimeout(() => (editing.value = false), 150);

async function jumpUp(steps: number): Promise<void> {
  for (let s = 0; s < steps; s++) await viewCall(props.viewId, "view.up");
}
const label = (c: Crumb, i: number) => crumbLabel(c, i, !!whole.value, tab.value.title);
function onCrumb(i: number): void {
  if (i === crumbs.value.length - 1) editing.value = true;
  else void jumpUp(crumbs.value.length - 1 - i);
}
</script>

<template>
  <div v-if="editing" class="crumb-edit">
    <input ref="input" spellcheck="false" :aria-label="t('路径')" :aria-invalid="!!error" @keydown="onKey" @blur="onBlur" />
    <span v-if="error" class="crumb-error">{{ error }}</span>
  </div>
  <nav v-else class="crumbs" :aria-label="t('当前位置')" :data-fit-min="CRUMBS_MIN">
    <span v-for="(c, i) in crumbs" :key="`${c.key}-${i}`" class="crumb-seg">
      <span v-if="i > 0" class="crumb-arrow"><Icon name="chevronRight" :size="12" /></span>
      <button
        :class="i === crumbs.length - 1 ? 'crumb-cur' : 'crumb'"
        :aria-label="i === crumbs.length - 1 ? t('{name}，编辑路径（Ctrl+L）', { name: label(c, i) }) : label(c, i)"
        @click="onCrumb(i)"
      >
        <Icon v-if="i === 0" name="drive" />
        {{ label(c, i) }}
      </button>
    </span>
    <span v-if="atRoot && whole" class="crumb-note">{{ t("整个卷") }}</span>
  </nav>
</template>

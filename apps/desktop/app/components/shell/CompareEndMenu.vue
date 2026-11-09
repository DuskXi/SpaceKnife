<!-- One end of a comparison in the toolbar's picker (EndMenu in shell/Compare.tsx): 「现在」, the
     snapshots, or a snapshot file; picking the other end swaps them. -->
<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { endLabel, stamp } from "@/lib/compare";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { NOW, setCompareEnds, swapCompare } from "@/state/compare";
import type { Tab } from "@/state/store";

const props = defineProps<{ tab: Tab; which: "base" | "target" }>();

const open = ref(false);
const box = ref<HTMLDivElement | null>(null);
const c = computed(() => props.tab.compare!);
const spec = computed(() => (props.which === "base" ? c.value.base : c.value.target));
const other = computed(() => (props.which === "base" ? c.value.target : c.value.base));
const label = computed(() => endLabel(spec.value, c.value.sources));

const down = (e: MouseEvent) => {
  if (box.value && !box.value.contains(e.target as Node)) open.value = false;
};
watch(open, (now) => {
  if (now) window.addEventListener("mousedown", down, true);
  else window.removeEventListener("mousedown", down, true);
});
onScopeDispose(() => window.removeEventListener("mousedown", down, true));

const options = computed(() => {
  const list: { spec: string; label: string; sub: string }[] = [];
  const s = c.value.sources;
  if (s?.now) list.push({ spec: NOW, label: t("现在"), sub: fmtSize(s.now.bytes_alloc) });
  for (const snap of s?.snapshots ?? []) list.push({ spec: snap.path, label: t("{time} 快照", { time: stamp(snap.created_at) }), sub: fmtSize(snap.bytes_alloc) });
  return list;
});

function pick(to: string): void {
  open.value = false;
  if (to === spec.value) return;
  if (to === other.value) return void swapCompare(props.tab.id); // picking the other end swaps them
  void (props.which === "base" ? setCompareEnds(props.tab.id, to, c.value.target) : setCompareEnds(props.tab.id, c.value.base, to));
}

async function openFile(): Promise<void> {
  open.value = false;
  const path = await window.sk.dialog.openSnapshot();
  if (path) pick(path);
}

const onEscape = (e: KeyboardEvent) => {
  if (e.key === "Escape") open.value = false;
};
</script>

<template>
  <div ref="box" :class="['cmp-end', { now: spec === NOW }]">
    <button
      class="cmp-end-btn"
      aria-haspopup="listbox"
      :aria-expanded="open"
      :aria-label="which === 'base' ? t('基准：{label}', { label }) : t('对比目标：{label}', { label })"
      @click="open = !open"
      @keydown="onEscape"
    >
      <span class="cmp-end-text">{{ label }}</span>
      <Icon name="chevronDown" :size="12" />
    </button>
    <div v-if="open" class="cmp-menu" role="listbox" :aria-label="which === 'base' ? t('基准') : t('对比目标')" @keydown="onEscape">
      <button v-for="o in options" :key="o.spec" role="option" :aria-selected="o.spec === spec" :class="['cmp-option', { on: o.spec === spec }]" @click="pick(o.spec)">
        <span class="cmp-check"><Icon v-if="o.spec === spec" name="check" :size="14" /></span>
        <span class="flex">{{ o.label }}</span>
        <span class="muted mono small">{{ o.sub }}</span>
      </button>
      <div class="cmp-menu-sep" />
      <button class="cmp-option" @click="openFile">
        <span class="cmp-check" />
        <span class="flex">{{ t("打开快照文件…") }}</span>
      </button>
    </div>
  </div>
</template>

<!-- 「导出报告…」 (W4 palette, docs/export-templates.md): a SpaceSniffer template or CSV / JSON / HTML,
     the zoom root or the whole scan, filtered or not, written by the backend to a file the user picks. -->
<script setup lang="ts">
import { computed, ref, shallowRef, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import type { ExportTarget } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { focusedViewId, set, toast, useApp } from "@/state/store";

const pad = (n: number) => String(n).padStart(2, "0");

function defaultName(target: ExportTarget, title: string): string {
  if (target.file_name) return target.file_name;
  const d = new Date();
  const stamp = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}`;
  const base = title.replace(/[\\/:*?"<>|]+/g, "_").trim() || "SpaceKnife";
  return `${base}-${stamp}.${target.extension}`;
}

const open = useApp((s) => s.exportOpen);
const viewId = useApp((s) => focusedViewId(s));
const filtering = useApp((s) => (viewId.value ? !!s.views[viewId.value]?.filter?.active : false));
const zoomed = useApp((s) => (viewId.value ? (s.views[viewId.value]?.info?.breadcrumb.length ?? 1) > 1 : false));
const title = useApp((s) => s.tabs.find((tab) => tab.id === s.active)?.title ?? "");
const targets = shallowRef<ExportTarget[] | null>(null);
const pick = ref("Grouped by folder");
const scope = ref<"zoom" | "all">("zoom");
const useFilter = ref(true);
const busy = ref(false);

watch(
  [open, targets],
  () => {
    if (!open.value || targets.value) return;
    void call("export.targets", {})
      .then((r) => (targets.value = r.targets))
      .catch(() => (targets.value = []));
  },
  { immediate: true },
);
watch(
  open,
  (now, _, onCleanup) => {
    if (!now) return;
    const key = (e: KeyboardEvent) => e.key === "Escape" && set({ exportOpen: false });
    window.addEventListener("keydown", key);
    onCleanup(() => window.removeEventListener("keydown", key));
  },
  { immediate: true },
);

const close = () => set({ exportOpen: false });
const target = computed(() => targets.value?.find((x) => x.name === pick.value) ?? targets.value?.[0]);
const group = (kind: ExportTarget["kind"]) => (targets.value ?? []).filter((x) => x.kind === kind);

async function run(): Promise<void> {
  const tg = target.value;
  const view = viewId.value;
  if (!tg || !view) return;
  const path = await window.sk.dialog.saveFile({
    title: t("导出到"),
    defaultName: defaultName(tg, title.value),
    filters: [{ name: tg.title, extensions: [tg.extension] }],
  });
  if (!path) return;
  busy.value = true;
  try {
    const r = await call("export.run", { view_id: view, target: tg.name, source: null, path, scope: zoomed.value ? scope.value : "all", use_filter: filtering.value && useFilter.value });
    close();
    toast(t("已导出 {size}：{name}", { size: fmtSize(r.bytes), name: r.path.split(/[\\/]/).pop() ?? r.path }));
    window.sk.shell.showItemInFolder(r.path);
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div v-if="open" class="modal-back" role="presentation" @mousedown="close">
    <div class="modal export-modal" role="dialog" aria-modal="true" aria-labelledby="export-title" @mousedown.stop>
      <h3 id="export-title">{{ t("导出报告") }}</h3>
      <p v-if="!viewId" class="muted">{{ t("先打开一次扫描") }}</p>
      <p v-if="targets === null" class="muted">{{ t("正在读取模板…") }}</p>
      <div v-if="targets" class="export-groups" role="radiogroup" :aria-label="t('模板与格式')">
        <div v-for="kind in ['template', 'format'] as const" :key="kind" class="export-group">
          <div class="muted small">{{ kind === "template" ? t("模板（与 SpaceSniffer 相同的模板语言）") : t("数据格式") }}</div>
          <label v-for="x in group(kind)" :key="x.name" :class="['export-option', { on: x.name === target?.name }]">
            <input type="radio" name="export-target" :checked="x.name === target?.name" @change="pick = x.name" />
            <span class="flex">
              <span class="export-name">
                {{ x.title }}
                <span v-if="kind === 'template'" class="muted small mono"> {{ x.name }}</span>
              </span>
              <span v-if="x.description" class="muted small export-desc">{{ x.description }}</span>
            </span>
            <span class="muted small mono">.{{ x.extension }}</span>
          </label>
        </div>
      </div>
      <div class="export-scope">
        <div v-if="zoomed" class="segmented small" role="radiogroup" :aria-label="t('范围')">
          <button role="radio" :aria-checked="scope === 'zoom'" :class="{ on: scope === 'zoom' }" @click="scope = 'zoom'">{{ t("当前位置") }}</button>
          <button role="radio" :aria-checked="scope === 'all'" :class="{ on: scope === 'all' }" @click="scope = 'all'">{{ t("整个扫描") }}</button>
        </div>
        <label v-if="filtering" class="export-check">
          <input v-model="useFilter" type="checkbox" />
          {{ t("只导出过滤匹配的项") }}
        </label>
      </div>
      <div class="modal-buttons">
        <button class="ghost" @click="close">{{ t("取消") }}</button>
        <button class="primary" :disabled="!target || !viewId || busy" @click="run()">
          <Icon name="upload" :size="14" /> {{ t("导出…") }}
        </button>
      </div>
    </div>
  </div>
</template>

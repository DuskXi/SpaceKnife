<!-- 「设置 › 文件类别」 (UI-W7-33): add, rename, recolour, reorder and delete file classes. Every change
     is sent at once (classes.update); the backend validates, saves, recolours the treemaps and re-runs
     :class: filters, and the parser vocabulary follows (classes.changed). A colour is the theme on
     screen's (W7 待定 22): each theme keeps its own, 「同时应用到另一主题」 sets both. Below, the folders
     the user classed by hand (UI-W7-38, added from the map's menu 「归类」), each one removable.

     Reordering drags a row with pointer events (Tauri's drag-drop handler swallows the page's HTML5
     drag and drop on Windows): the row is put before the one it is let go over; Esc cancels. -->
<script setup lang="ts">
import { computed, onMounted, onScopeDispose, ref, shallowRef } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { colorOf, themeColor, userColor } from "@/lib/classes";
import { addRow, type ClassRow, duplicates, folderNote, moveRow, rowAt, rowChanged, splitExts, toRow } from "@/lib/classesEditor";
import { byLang, t } from "@/lib/i18n";
import { themeName } from "@/lib/theme";
import type { ClassesState } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { get, set, toast, useApp } from "@/state/store";

const theme = useApp((s) => s.theme); // the colours shown and changed are this theme's
const state = shallowRef<ClassesState | null>(null);
// 「同时应用到另一主题」: a colour picked (or reset) goes to both themes
const both = ref(false);
const rows = shallowRef<ClassRow[]>([]);
const confirm = ref<string | null>(null); // row key, or "reset"

onMounted(() => {
  void call("classes.get", {})
    .then((st) => {
      state.value = st;
      rows.value = st.classes.map(toRow);
    })
    .catch(() => undefined);
});

// extensions already taken by an earlier class: that class colours them (the notes follow the language)
const dups = computed(() => duplicates(rows.value));

async function commit(next: ClassRow[]): Promise<void> {
  rows.value = next;
  try {
    const st = await call("classes.update", {
      classes: next.map((r) => ({ id: r.id, name: r.name.trim(), extensions: r.id === "other" ? [] : [r.exts], color: r.color, color_light: r.color_light })),
    });
    state.value = st;
    // new classes got their ids: keep the rows' keys, take the ids
    rows.value = rows.value.map((r, i) => ({ ...r, id: st.classes[i]?.id ?? r.id }));
    const vocab = get().vocab;
    set({ vocab: vocab ? { ...vocab, classes: st.classes } : vocab });
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
    if (state.value) rows.value = state.value.classes.map(toRow);
  }
}
const patch = (key: string, p: Partial<ClassRow>) => (rows.value = rows.value.map((r) => (r.key === key ? { ...r, ...p } : r)));
function changed(key: string): void {
  const r = rows.value.find((x) => x.key === key);
  if (!r || !rowChanged(r, state.value?.classes.find((c) => c.id === r.id))) return;
  void commit(rows.value.map((x) => (x.key === key ? { ...x, exts: splitExts(x.exts).join("; ") } : x)));
}
/** The colour fields a pick or a reset changes: the theme on screen's, or both. */
const recolor = (hex: string): Partial<ClassRow> => (both.value ? { color: hex, color_light: hex } : { [themeName() === "light" ? "color_light" : "color"]: hex });

const view = computed(() => {
  void theme.value;
  return rows.value.map((r) => {
    const id = r.id ?? "other";
    const c = { id, color: r.color, color_light: r.color_light };
    return {
      r,
      other: r.id === "other",
      hex: colorOf(c),
      // a built-in class's own colour in this theme can go back to the theme's
      resettable: !!r.id && !!themeColor(id) && !!userColor(c, theme.value),
      dups: dups.value.get(r.key),
      note: folderNote(r.id),
    };
  });
});

const blurOnEnter = (e: KeyboardEvent) => e.key === "Enter" && (e.target as HTMLInputElement).blur();
const value = (e: Event) => (e.target as HTMLInputElement).value;

function remove(key: string): void {
  confirm.value = null;
  void commit(rows.value.filter((x) => x.key !== key));
}
async function resetAll(): Promise<void> {
  confirm.value = null;
  const st = await call("classes.reset", {});
  state.value = st;
  rows.value = st.classes.map(toRow);
}
async function unclass(path: string): Promise<void> {
  try {
    state.value = await call("classes.folder", { path, class_id: null });
  } catch (err) {
    toast(err instanceof RpcError ? err.message : String(err));
  }
}
const folderClass = (classId: string | null) => (classId ? state.value?.classes.find((c) => c.id === classId) : null);

// ------------------------------------------------------------------ reordering (pointer events)

const list = ref<HTMLDivElement | null>(null);
let drag: { key: string; y: number; pointer: number } | null = null;
const dragging = ref<string | null>(null);
const over = ref<string | null>(null);

function boxes(): { key: string; top: number; bottom: number }[] {
  return [...(list.value?.querySelectorAll<HTMLElement>("[data-class-key]") ?? [])].map((el) => {
    const b = el.getBoundingClientRect();
    return { key: el.dataset.classKey!, top: b.top, bottom: b.bottom };
  });
}

function onPointerDown(r: ClassRow, e: PointerEvent): void {
  if (e.button !== 0 || r.id === "other") return;
  // the fields and buttons keep the pointer: a drag starts on the grip or the row's free space
  if ((e.target as HTMLElement).closest("input, button, label")) return;
  drag = { key: r.key, y: e.clientY, pointer: e.pointerId };
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
}
function onPointerMove(e: PointerEvent): void {
  const d = drag;
  if (!d || d.pointer !== e.pointerId) return;
  if (!dragging.value) {
    if (Math.abs(e.clientY - d.y) < 4) return; // a click, not a drag
    dragging.value = d.key;
    window.addEventListener("keydown", onEscape, true);
  }
  e.preventDefault();
  over.value = rowAt(boxes(), e.clientY);
}
function onPointerUp(e: PointerEvent): void {
  const d = drag;
  if (!d || d.pointer !== e.pointerId) return;
  const to = dragging.value ? over.value : null;
  endDrag();
  if (to) {
    const next = moveRow(rows.value, d.key, to);
    if (next) void commit(next);
  }
}
function endDrag(): void {
  drag = null;
  dragging.value = null;
  over.value = null;
  window.removeEventListener("keydown", onEscape, true);
}
function onEscape(e: KeyboardEvent): void {
  if (e.key !== "Escape") return;
  e.preventDefault();
  e.stopPropagation();
  endDrag();
}
onScopeDispose(() => window.removeEventListener("keydown", onEscape, true));
</script>

<template>
  <section class="set-group" :aria-label="t('文件类别')">
    <div class="set-group-head">
      <h2>{{ t("类别") }}</h2>
      <span class="muted small">{{ t("拖动排序：同一扩展名在多个类别里时，排在前面的决定颜色") }}</span>
    </div>
    <div ref="list" :class="['set-card classes-card', { reordering: dragging }]" role="list">
      <div
        v-for="x in view"
        :key="x.r.key"
        role="listitem"
        :data-class-key="x.r.key"
        :class="['class-row', { dragging: dragging === x.r.key, 'drop-target': dragging && over === x.r.key && dragging !== x.r.key }]"
        @pointerdown="onPointerDown(x.r, $event)"
        @pointermove="onPointerMove"
        @pointerup="onPointerUp"
        @pointercancel="endDrag"
      >
        <span :class="['class-grip', { off: x.other }]" aria-hidden="true">⋮⋮</span>
        <label class="class-color" :title="t('改颜色')" :style="{ background: x.hex }">
          <input
            type="color"
            :value="x.hex.slice(0, 7)"
            :aria-label="t('{name} 的颜色', { name: x.r.name })"
            @input="patch(x.r.key, recolor(value($event).toUpperCase()))"
            @blur="changed(x.r.key)"
          />
        </label>
        <div class="class-fields">
          <input
            class="class-name"
            :value="x.r.name"
            :disabled="x.other"
            :aria-label="t('类别名')"
            @input="patch(x.r.key, { name: value($event) })"
            @blur="changed(x.r.key)"
            @keydown="blurOnEnter"
          />
          <input
            class="class-exts mono"
            :value="x.other ? '' : x.r.exts"
            :placeholder="x.other ? t('所有未归类的扩展名') : t('扩展名，用 ; 分隔，例如 pak; ucas')"
            :disabled="x.other"
            :aria-label="t('{name} 的扩展名', { name: x.r.name })"
            @input="patch(x.r.key, { exts: value($event) })"
            @blur="changed(x.r.key)"
            @keydown="blurOnEnter"
          />
          <div v-if="x.dups" class="warn-text small">{{ t("已在前面的类别里：{list}", { list: x.dups.join(byLang("、", ", ")) }) }}</div>
          <div v-if="x.note" class="muted small class-note">{{ x.note }}</div>
        </div>
        <span class="muted small mono class-filter">:class:{{ x.r.name.trim() }}</span>
        <!-- with the row's other action, in a slot every row has: the names stay where they were, and a
             row with the button does not shift -->
        <span class="class-reset">
          <button
            v-if="x.resettable"
            class="icon-btn tiny"
            :title="t('恢复默认颜色')"
            :aria-label="t('恢复 {name} 的默认颜色', { name: x.r.name })"
            @click="commit(rows.map((y) => (y.key === x.r.key ? { ...y, ...recolor('') } : y)))"
          >
            <Icon name="refresh" :size="12" />
          </button>
        </span>
        <template v-if="!x.other">
          <span v-if="confirm === x.r.key" class="class-confirm">
            <button class="danger-btn small" @click="remove(x.r.key)">{{ t("删除") }}</button>
            <button class="ghost small" @click="confirm = null">{{ t("取消") }}</button>
          </span>
          <button v-else class="icon-btn small" :aria-label="t('删除类别 {name}', { name: x.r.name })" :title="t('删除（其扩展名归入「其他」）')" @click="confirm = x.r.key">
            <Icon name="close" :size="14" />
          </button>
        </template>
      </div>
    </div>
    <div class="class-actions">
      <button class="ghost small" @click="commit(addRow(rows))"><Icon name="plus" :size="14" /> {{ t("添加类别") }}</button>
      <template v-if="state && (state.custom || !!state.folders.length)">
        <span v-if="confirm === 'reset'" class="class-confirm">
          <span class="small">
            {{
              state.folders.length
                ? t("恢复为内置的 {n} 个类别，并清除 {m} 个文件夹归类？", { n: state.defaults.length, m: state.folders.length })
                : t("恢复为内置的 {n} 个类别？", { n: state.defaults.length })
            }}
          </span>
          <button class="danger-btn small" @click="resetAll">{{ t("恢复") }}</button>
          <button class="ghost small" @click="confirm = null">{{ t("取消") }}</button>
        </span>
        <button v-else class="ghost small" @click="confirm = 'reset'">{{ t("恢复默认类别") }}</button>
      </template>
      <label class="export-check small class-both" :title="theme === 'light' ? t('改的颜色也用在深色主题里') : t('改的颜色也用在浅色主题里')">
        <input type="checkbox" :checked="both" @change="both = ($event.target as HTMLInputElement).checked" />
        {{ t("同时应用到另一主题") }}
      </label>
    </div>
    <div class="set-group-head">
      <h2>{{ t("按文件夹归类") }}</h2>
      <span class="muted small">{{ t("在图上右键文件夹，选「归类」添加；这些文件夹里的文件不论扩展名都算所选的类别") }}</span>
    </div>
    <div class="set-card" role="list">
      <div v-if="state && state.folders.length === 0" class="muted small class-folders-empty">{{ t("还没有。游戏、项目、模型缓存等文件夹会自动识别") }}</div>
      <div v-for="f in state?.folders ?? []" :key="f.path" role="listitem" class="class-folder">
        <span class="mono class-folder-path" :title="f.path">{{ f.path }}</span>
        <span class="class-folder-class">
          <span v-if="folderClass(f.class_id)" class="swatch" :style="{ background: colorOf(folderClass(f.class_id)!) }" />
          {{ folderClass(f.class_id)?.name ?? t("只按扩展名") }}
        </span>
        <button class="icon-btn small" :aria-label="t('取消 {path} 的归类', { path: f.path })" :title="t('改回自动识别')" @click="unclass(f.path)">
          <Icon name="close" :size="14" />
        </button>
      </div>
    </div>
  </section>
</template>

<style scoped>
/* the reordering drag (pointer events): the row carried, and the row it goes before */
.classes-card.reordering {
  cursor: grabbing;
  user-select: none;
}
.class-row.dragging {
  opacity: 0.55;
}
.class-row.drop-target {
  box-shadow: inset 0 2px 0 var(--sk-color-accent-default);
}
</style>

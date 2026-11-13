<!-- A `folders` setting (treemap.hidden_folders): the folders one per row with a button that takes it
     out, and a box that adds one by its full path (Enter) or, in the app, 「浏览…」. The value is
     the folders one per line; every change saves at once. -->
<script setup lang="ts">
import { computed, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { inBrowser } from "@/lib/host";
import { t } from "@/lib/i18n";
import { addFolders, folderLines, isFullPath, splitPath } from "@/lib/folderList";

const props = defineProps<{ value: string; label: string; disabled: boolean; describedBy: string }>();
const emit = defineEmits<{ save: [value: string] }>();

// what was saved last, until the stored value comes back: a second change right after the first
// builds on it (else it would undo the first)
const pending = ref<string | null>(null);
watch(
  () => props.value,
  () => (pending.value = null),
);
const current = computed(() => pending.value ?? props.value);
const folders = computed(() => folderLines(current.value));
const typed = ref("");
const error = ref("");
const win = () => window.sk.platform === "win32";

function save(value: string): void {
  pending.value = value;
  emit("save", value);
}

function add(paths: string[]): void {
  const { value, added } = addFolders(current.value, paths);
  if (added) save(value);
}

function addTyped(): void {
  const p = typed.value.trim();
  if (!p) return;
  if (!isFullPath(p, win())) {
    error.value = t("请写完整路径，例如 {example}", { example: win() ? "C:\\Users\\lenny\\Music" : "/home/lenny/Music" });
    return;
  }
  error.value = "";
  typed.value = "";
  add([p]);
}

function remove(p: string): void {
  save(folders.value.filter((f) => f !== p).join("\n"));
}

async function browse(): Promise<void> {
  const picked = await window.sk.dialog.openFolders(t("选择要隐藏的文件夹"));
  if (picked.length) add(picked);
}

function onKey(e: KeyboardEvent): void {
  e.stopPropagation();
  if (e.key === "Enter") addTyped();
}
</script>

<template>
  <div class="folder-list" :aria-describedby="describedBy">
    <ul v-if="folders.length" class="fl-items" :aria-label="label">
      <li v-for="f in folders" :key="f" class="fl-item">
        <Icon name="folder" :size="14" />
        <!-- the folder's name, then where it is (cut from its start: the part next to the name stays) -->
        <span class="fl-name" :title="f">{{ splitPath(f).name }}</span>
        <span class="fl-path mono" :title="f"
          ><bdi>{{ splitPath(f).dir }}</bdi></span
        >
        <button class="icon-btn small" :aria-label="t('移出列表：{path}', { path: f })" :title="t('移出列表')" :disabled="disabled" @click="remove(f)">
          <Icon name="close" :size="12" />
        </button>
      </li>
    </ul>
    <div v-else class="fl-empty muted">{{ t("还没有文件夹。扫描到列表里的文件夹时，工具栏会出现「隐藏文件夹」。") }}</div>
    <div class="fl-add">
      <span :class="['fl-input', { invalid: error }]">
        <input
          :aria-label="t('添加文件夹的完整路径')"
          :aria-invalid="!!error"
          :placeholder="t('输入文件夹的完整路径，回车添加')"
          :value="typed"
          :disabled="disabled"
          @input="typed = ($event.target as HTMLInputElement).value"
          @keydown="onKey"
        />
      </span>
      <button class="ghost" :disabled="disabled || !typed.trim()" @click="addTyped">{{ t("添加") }}</button>
      <button v-if="!inBrowser()" class="ghost" :disabled="disabled" @click="browse">{{ t("浏览…") }}</button>
    </div>
    <span v-if="error" class="set-error">{{ error }}</span>
  </div>
</template>

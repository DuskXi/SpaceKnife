<!-- The undo toast of a file operation (UI-W5-42…47): progress while it runs, then 5 seconds to undo
     (put trashed items back, or call off a permanent deletion that is still waiting). -->
<script setup lang="ts">
import { computed, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { fmtInt, fmtSize, plural } from "@/lib/format";
import { t } from "@/lib/i18n";
import { opCancel, opUndo } from "@/state/actions";
import { set, useApp } from "@/state/store";

const UNDO_MS = 5000;

const op = useApp((s) => s.op);
const since = ref(0);
const now = ref(0);

// the undo window starts when the operation is done (trash) or the moment it waits (delete)
const undoable = computed(() => !!op.value && op.value.can_undo && (op.value.phase === "done" || op.value.phase === "waiting"));
watch(
  [undoable, () => op.value?.task_id, () => op.value?.phase],
  (_, __, onCleanup) => {
    if (!undoable.value) return;
    const t0 = performance.now();
    since.value = t0;
    now.value = t0;
    const timer = setInterval(() => (now.value = performance.now()), 100);
    onCleanup(() => clearInterval(timer));
  },
  { immediate: true },
);
watch(
  [op, undoable, now, since],
  (_, __, onCleanup) => {
    const o = op.value;
    if (o && (o.phase === "done" || o.phase === "undone" || o.phase === "cancelled") && (!undoable.value || now.value - since.value > UNDO_MS)) {
      const timer = setTimeout(() => set({ op: null }), undoable.value ? 0 : 2500);
      onCleanup(() => clearTimeout(timer));
    }
  },
  { immediate: true },
);

const left = computed(() => Math.max(0, UNDO_MS - (now.value - since.value)));
const text = computed(() => {
  const o = op.value;
  if (!o) return "";
  const ok = o.results.filter((r) => r.ok).length;
  const failed = o.results.length - ok;
  const progress = { done: fmtInt(o.done), total: fmtInt(o.total) };
  if (o.phase === "waiting") return plural(o.total, t("{n} 项将被永久删除", { n: fmtInt(o.total) }), "1 item will be deleted permanently");
  if (o.phase === "running") return o.mode === "trash" ? t("正在移到回收站 {done} / {total}…", progress) : t("正在删除 {done} / {total}…", progress);
  if (o.phase === "undone") return t("已撤销");
  if (o.phase === "cancelled") return t("已停止");
  if (o.mode === "trash") {
    const parts = {
      n: fmtInt(ok),
      freed: o.freed ? t("（{size}）", { size: fmtSize(o.freed) }) : "",
      failed: failed ? t("，{n} 项未移动（见日志）", { n: fmtInt(failed) }) : "",
    };
    return plural(ok, t("已移到回收站 {n} 项{freed}{failed}", parts), "Moved 1 item to trash{freed}{failed}", parts);
  }
  const parts = {
    n: fmtInt(ok),
    freed: o.freed ? t("，释放 {size}", { size: fmtSize(o.freed) }) : "",
    failed: failed ? t("，{n} 项失败（见日志）", { n: fmtInt(failed) }) : "",
  };
  return plural(ok, t("已永久删除 {n} 项{freed}{failed}", parts), "Deleted 1 item permanently{freed}{failed}", parts);
});
</script>

<template>
  <div v-if="op" class="op-toast" role="status" aria-live="polite">
    <Icon :name="op.phase === 'running' || op.phase === 'waiting' ? 'clock' : 'check'" :size="16" />
    <span class="flex">{{ text }}</span>
    <button v-if="undoable && left > 0" class="ghost" @click="opUndo(op.task_id)">
      {{ t("撤销（{n} 秒）", { n: Math.ceil(left / 1000) }) }}
    </button>
    <button v-if="op.phase === 'running'" class="ghost" @click="opCancel(op.task_id)">{{ t("停止") }}</button>
    <button class="icon-btn small" :aria-label="t('关闭提示')" @click="set({ op: null })">
      <Icon name="close" :size="14" />
    </button>
    <span v-if="undoable" class="op-countdown" :style="{ width: `${(left / UNDO_MS) * 100}%` }" />
  </div>
</template>

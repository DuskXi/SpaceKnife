<!-- The title bar's update badge (updates.v1, state/updates.ts): there while a newer release is known,
     however far behind the app is; a running download fills it and shows its percentage. A click
     opens the update card under it; a click elsewhere or Esc closes it. -->
<script setup lang="ts">
import { computed, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { closeUpdate, toggleUpdateCard } from "@/state/actions";
import { useApp } from "@/state/store";
import { announced, downloading, downloadOf, downloadPct } from "@/state/updates";

import UpdateCard from "./UpdateCard.vue";

const status = useApp((s) => s.update);
const open = useApp((s) => s.updateOpen);
const anchor = ref<HTMLDivElement | null>(null);
const badge = ref<HTMLButtonElement | null>(null);
watch(
  open,
  (now, _, onCleanup) => {
    if (!now) return;
    const away = (e: MouseEvent) => {
      if (!anchor.value?.contains(e.target as Node)) closeUpdate();
    };
    window.addEventListener("mousedown", away);
    onCleanup(() => window.removeEventListener("mousedown", away));
  },
  { immediate: true },
);

const view = computed(() => {
  const st = status.value;
  if (!announced(st)) return null;
  const version = st.latest.version;
  const d = downloadOf(st);
  const busy = downloading(d);
  const pct = downloadPct(d);
  const ready = d.state === "done";
  const label = busy ? (pct === null ? t("下载中") : `${pct}%`) : ready ? t("可以更新") : t("更新 {version}", { version });
  const tip = busy
    ? t("正在下载 SpaceKnife {version}", { version })
    : ready
      ? t("SpaceKnife {version} 已下载并校验，点开后选择怎么更新", { version })
      : d.state === "failed"
        ? t("更新下载失败：{error}", { error: d.error ?? "" })
        : t("SpaceKnife {version} 可用（你在用 {current}）", { version, current: st.current });
  return { status: st, state: d.state, busy, pct, ready, label, tip };
});

function onKeyDown(e: KeyboardEvent): void {
  if (e.key !== "Escape" || !open.value) return;
  e.stopPropagation(); // not also the map's Escape (clear the selection)
  closeUpdate();
  badge.value?.focus();
}
</script>

<template>
  <div v-if="view" ref="anchor" class="update-anchor" @keydown="onKeyDown">
    <button
      ref="badge"
      :class="['update-badge', { on: open, busy: view.busy, ready: view.ready }]"
      :aria-label="view.tip"
      :title="view.tip"
      aria-haspopup="dialog"
      :aria-expanded="open"
      @click="toggleUpdateCard"
    >
      <span v-if="view.busy" class="update-fill" :style="{ width: `${view.pct ?? 0}%` }" />
      <Icon :name="view.state === 'done' ? 'check' : view.state === 'failed' ? 'warning' : 'up'" :size="14" />
      <span :class="view.busy ? 'mono' : undefined">{{ view.label }}</span>
    </button>
    <UpdateCard v-if="open" :status="view.status" placement="anchored" />
  </div>
</template>

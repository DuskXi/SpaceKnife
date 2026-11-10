<!-- The hover card of a change (UI-W6-20…22): both sizes, the change in percent, and the children
     that changed most (CompareHoverCard in shell/Compare.tsx). -->
<script setup lang="ts">
import { computed, ref, shallowRef, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { useHoverCard } from "@/composables/useHoverCard";
import { KIND_ICON, sideName } from "@/lib/compare";
import { fmtDelta, fmtInt, fmtSize } from "@/lib/format";
import { cardAvoid, hoverFacts, placeCompareCard } from "@/lib/hoverCard";
import { t } from "@/lib/i18n";
import type { CompareNodeResult } from "@/protocol/generated";
import { call } from "@/state/actions";
import { setting, useApp } from "@/state/store";
import { Kind } from "@/visual/frame";
import type { HoverInfo } from "@/visual/types";

const HOVER_DELAY = 250;

const props = defineProps<{ hover: HoverInfo | null; bounds: { w: number; h: number } }>();

const info = shallowRef<CompareNodeResult | null>(null);
const target = useApp((s) => {
  const tab = s.tabs.find((x) => x.compare?.viewId === props.hover?.viewId);
  return tab?.compare?.info?.target ?? null;
});
const keepOpen = useApp((s) => setting(s, "general.hover_keep_open", true));
const shown = useHoverCard(() => props.hover, keepOpen, HOVER_DELAY);
const card = ref<HTMLDivElement | null>(null);
const pos = ref({ left: 0, top: 0 });

// the change's details, once per target (not per pointer move); a new picture of a live
// comparison asks again and the card keeps what it shows until the answer comes
const heldKey = ref<number | null>(null);
const shownKey = computed(() => {
  const s = shown.value;
  if (!s) return null;
  const facts = hoverFacts(s.frame, s.target, null);
  return facts.kind !== Kind.FREE && facts.kind !== Kind.UNKNOWN ? facts.key : null;
});
const seq = computed(() => shown.value?.frame.info.seq ?? 0);
watch(
  [() => shown.value?.viewId ?? null, shownKey, seq] as const,
  ([viewId, key], _old, onCleanup) => {
    if (!viewId || key === null) {
      info.value = null;
      heldKey.value = null;
      return;
    }
    if (heldKey.value !== key) {
      info.value = null;
      heldKey.value = null;
    }
    let alive = true;
    onCleanup(() => (alive = false));
    void call("compare.node", { view_id: viewId, key, children: 4 })
      .then((r) => {
        if (!alive) return;
        info.value = r;
        heldKey.value = key;
      })
      .catch(() => undefined);
  },
  { immediate: true },
);

watch(
  [shown, info, () => props.bounds.w, () => props.bounds.h],
  () => {
    if (!shown.value || !card.value) return;
    pos.value = placeCompareCard(cardAvoid(shown.value), { w: card.value.offsetWidth, h: card.value.offsetHeight }, props.bounds);
  },
  { flush: "post" },
);

const view = computed(() => {
  const r = info.value;
  if (!r) return null;
  const it = r.item;
  const pct = it.base > 0 ? (it.delta / it.base) * 100 : null;
  const sep = it.path.includes("\\") ? "\\" : "/";
  const parts = it.path.split(sep);
  const path = parts.length > 3 ? `…${sep}${parts.slice(-2).join(sep)}` : it.path;
  return { it, pct, path, tname: target.value ? sideName(target.value) : t("目标") };
});
</script>

<template>
  <div v-if="shown && view && info" ref="card" class="hovercard cmp-card" :style="{ left: `${pos.left}px`, top: `${pos.top}px` }" role="tooltip">
    <div class="cmp-card-title">
      <Icon :name="KIND_ICON[view.it.kind]" :size="15" />
      <span class="flex cmp-card-name">{{ view.it.kind === "group" ? t("{n} 个小项", { n: fmtInt(view.it.members) }) : view.it.label }}</span>
      <strong class="mono">{{ fmtDelta(view.it.delta) }}</strong>
    </div>
    <div class="mono small muted cmp-card-path">{{ view.path }}</div>
    <div class="row-between small">
      <span>
        <span class="muted">{{ t("基准") }}</span> <span class="mono">{{ view.it.status === "new" ? "—" : fmtSize(view.it.base) }}</span> <span class="muted">›</span>
        <span class="muted">{{ view.tname }}</span> <span class="mono">{{ view.it.status === "deleted" ? "—" : fmtSize(view.it.target) }}</span>
      </span>
      <span v-if="view.pct != null && view.it.status !== 'deleted'" class="mono">{{ `${view.pct >= 0 ? "+" : "−"}${Math.abs(view.pct).toFixed(1)}%` }}</span>
    </div>
    <div v-if="info.children.length > 0" class="cmp-card-kids">
      <div v-for="k in info.children" :key="k.node" class="cmp-card-kid small">
        <Icon :name="KIND_ICON[k.kind]" :size="13" />
        <span class="flex cmp-card-name">{{ k.label }}</span>
        <span v-if="k.new_files > 0" class="muted">{{ t("新增 {n} 项", { n: fmtInt(k.new_files) }) }}</span>
        <span class="mono">{{ fmtDelta(k.delta) }}</span>
      </div>
    </div>
    <div class="muted small cmp-card-hint">{{ t("双击放大 · 右键更多") }}</div>
  </div>
</template>

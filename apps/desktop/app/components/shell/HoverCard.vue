<!-- Hover card (UI-MAIN-29…31): beside the hovered block, never covering it; flips to stay inside.
     Where a row's box can be most of the drawing (a large sector), the card follows the pointer. -->
<script setup lang="ts">
import { computed, ref, shallowRef, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { useHoverCard } from "@/composables/useHoverCard";
import { classColors } from "@/lib/classes";
import { fmtAgo, fmtDateTime, fmtInt, fmtPct, fmtSize, sizeParts } from "@/lib/format";
import { cardAvoid, hoverFacts, placeCard, splitPlaceholders } from "@/lib/hoverCard";
import { t } from "@/lib/i18n";
import type { TreeNodeResult } from "@/protocol/generated";
import { call } from "@/state/actions";
import { setting, useApp } from "@/state/store";
import { type ColorMode, fillOf, labelColors } from "@/treemap/paint";
import { Flag, Kind } from "@/visual/frame";
import { rowTitle } from "@/visual/rowTitle";
import { HIT_ROOT, type HoverInfo } from "@/visual/types";

const DELAY = 300;

const props = defineProps<{ hover: HoverInfo | null; bounds: { w: number; h: number } }>();

const vocab = useApp((s) => s.vocab);
const scanId = useApp((s) => (props.hover ? s.views[props.hover.viewId]?.state.scan_id : undefined));
const colorMode = useApp((s) => (props.hover ? s.views[props.hover.viewId]?.state.color_mode : "class") ?? "class");
const scanning = useApp((s) => {
  const st = scanId.value ? s.scans[scanId.value] : undefined;
  return st?.state === "scanning" || st?.state === "queued";
});
// W7 常规 · 悬停提示
const keepOpen = useApp((s) => setting(s, "general.hover_keep_open", true));
const showDates = useApp((s) => setting(s, "general.hover_dates", true));
const showAge = useApp((s) => setting(s, "general.hover_age", true));
const shown = useHoverCard(() => props.hover, keepOpen, DELAY);
const info = shallowRef<TreeNodeResult | null>(null);
const parentAlloc = ref(0);
const card = ref<HTMLDivElement | null>(null);
const pos = ref({ left: 0, top: 0 });

// the node's details, once per node (not per pointer move); a new picture asks again (its numbers
// may have changed) and the card keeps what it shows until the answer comes
const detailNode = computed(() => {
  const s = shown.value;
  if (!s) return null;
  const facts = hoverFacts(s.frame, s.target, null);
  return facts.kind < Kind.SMALL ? facts.node : null;
});
const seq = computed(() => shown.value?.frame.info.seq ?? 0);
watch(
  [detailNode, scanId, seq] as const,
  ([node, scan], _old, onCleanup) => {
    if (node === null || !scan) {
      info.value = null;
      return;
    }
    if (info.value && info.value.node.id !== node) info.value = null;
    let alive = true;
    onCleanup(() => (alive = false));
    void call("tree.node", { scan_id: scan, node })
      .then(async (r) => {
        if (!alive) return;
        info.value = r;
        if (r.node.parent > 0) {
          const p = await call("tree.node", { scan_id: scan, node: r.node.parent });
          if (alive) parentAlloc.value = p.node.alloc;
        }
      })
      .catch(() => {});
  },
  { immediate: true },
);

// right of the block, top-aligned; else left; else below / above (UI-MAIN-29)
watch(
  [shown, info, () => props.bounds.w, () => props.bounds.h],
  () => {
    if (!shown.value || !card.value) return;
    pos.value = placeCard(cardAvoid(shown.value), { w: card.value.offsetWidth, h: card.value.offsetHeight }, props.bounds);
  },
  { flush: "post" },
);

const view = computed(() => {
  const s = shown.value;
  if (!s) return null;
  const f = s.frame;
  const node = info.value?.node;
  const v = vocab.value;
  const facts = hoverFacts(f, s.target, v, node?.ext);
  const kind = facts.kind;
  const fill = fillOf(facts.columns, facts.row, colorMode.value as ColorMode, v?.classes.map((c) => c.id) ?? [], undefined, classColors(v));
  const [fg] = labelColors(fill);
  const bg = `rgb(${fill
    .slice(0, 3)
    .map((x) => Math.round(x * 255))
    .join(",")})`;
  const [num, unit] = sizeParts(facts.size);
  const cls = v?.classes[facts.cls];
  const dir = kind === Kind.FOLDER || kind === Kind.COLLAPSED || kind === Kind.PENDING;
  const named = rowTitle(f, s.target, { scanning: scanning.value, formatInt: (n) => fmtInt(n) }).line1;
  const title = (kind < Kind.SMALL ? node?.name : null) ?? named ?? "";
  const i = info.value;
  const folder = i ? i.path.slice(0, Math.max(0, i.path.length - i.node.name.length)).replace(/[\\/]+$/, "") : "";
  const parentName = folder.split(/[\\/]/).filter(Boolean).pop() ?? "";
  const flags = s.target === HIT_ROOT ? (f.info.root?.flags ?? 0) : (f.flags[s.target] ?? 0);
  return { node, size: facts.size, fg, bg, num, unit, cls, dir, title, folder, parentName, flags };
});

const share = computed(() => splitPlaceholders(t("占 {name} {share}")));
const times = computed(() => {
  const n = view.value?.node;
  if (!n) return [];
  return [
    ["ctime", t("创建"), n.ctime],
    ["mtime", t("修改"), n.mtime],
    ["atime", t("访问"), n.atime],
  ] as const;
});

/** In a recycle bin (「回收站特效」): its space comes back only when the bin is emptied. */
function trashText(bin: boolean, size: number): string {
  const mac = window.sk.platform === "darwin";
  return bin
    ? mac
      ? t("废纸篓：清空后可释放 {size}", { size: fmtSize(size) })
      : t("回收站：清空后可释放 {size}", { size: fmtSize(size) })
    : mac
      ? t("在废纸篓里：清空废纸篓后才释放空间")
      : t("在回收站里：清空回收站后才释放空间");
}
</script>

<template>
  <div v-if="shown && view" ref="card" class="hovercard" role="tooltip" :style="{ left: `${pos.left}px`, top: `${pos.top}px` }">
    <div class="hc-body">
      <div class="hc-head">
        <div class="hc-icon" :style="{ background: view.dir ? 'var(--sk-color-bg-raised)' : view.bg, color: view.dir ? 'var(--sk-color-text-primary)' : view.fg }">
          <Icon :name="view.dir ? 'folder' : 'file'" />
        </div>
        <div class="hc-name">
          <div class="hc-title">{{ view.title }}</div>
          <div v-if="view.folder" class="mono hc-path">{{ view.folder }}</div>
        </div>
      </div>
      <div class="row-between">
        <div class="big-num small">
          <span class="num">{{ view.num }}</span>
          <span class="unit">{{ view.unit }}</span>
        </div>
        <span v-if="parentAlloc > 0 && view.node" class="muted"
          ><template v-for="(part, i) in share" :key="i"
            ><template v-if="!part.slot">{{ part.text }}</template
            ><template v-else-if="part.text === 'name'">{{ view.parentName }}</template
            ><span v-else-if="part.text === 'share'" class="mono strong">{{ fmtPct(view.node.alloc, parentAlloc) }}</span></template
          ></span
        >
      </div>
      <div v-if="view.flags & Flag.TRASH" class="hc-trash">
        <Icon name="basket" :size="12" />
        <span>{{ trashText((view.flags & Flag.TRASH_BIN) !== 0, view.size) }}</span>
      </div>
      <div v-if="view.node" class="hc-class muted">
        <span v-if="view.dir">{{ t("{files} 个文件 · {dirs} 个文件夹", { files: fmtInt(view.node.files), dirs: fmtInt(view.node.dirs) }) }}</span>
        <template v-else>
          <span class="swatch" :style="{ background: view.bg }" />
          <span>{{ view.cls?.name ?? t("其他") }}{{ view.node.ext ? ` · .${view.node.ext}` : "" }}</span>
        </template>
      </div>
    </div>
    <div v-if="view.node && showDates" class="hc-times">
      <div v-for="[key, label, time] in times" :key="key" class="hc-time">
        <span class="muted">{{ label }}</span>
        <span v-if="showAge">{{ fmtAgo(time) }}</span>
        <span class="mono muted">{{ fmtDateTime(time) }}</span>
      </div>
    </div>
    <div class="hc-hint">
      <span>{{ t("双击放大") }}</span>
      <span>·</span>
      <span>{{ t("右键更多") }}</span>
      <span>·</span>
      <Kbd>Space</Kbd>
      <span>{{ t("预览") }}</span>
    </div>
  </div>
</template>

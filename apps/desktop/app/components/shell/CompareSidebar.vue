<!-- 「快照对比」 side panel (W6): net change, legend, growth / shrink lists, timeline (CompareSidebar,
     MoreMenu, Summary, Legend, ChangeList, ChangeRow and Timeline in shell/Compare.tsx). -->
<script setup lang="ts">
import { computed, onScopeDispose, ref, shallowRef, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { day, fmtSpan, KIND_ICON, LIST_LIMIT, sideName, stamp, timeline } from "@/lib/compare";
import { diffColor, diffTiers, growColors, maxChange, shrinkColors } from "@/lib/diff";
import { fmtDelta, fmtInt, fmtSize, plural, sizeParts } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import type { ChangeItem, CompareSummary } from "@/protocol/generated";
import { call, viewCall } from "@/state/actions";
import { runCommand } from "@/state/commands";
import { NOW, setCompareEnds, swapCompare } from "@/state/compare";
import { patchView, type Tab, toast, useApp } from "@/state/store";
import { componentFor } from "@/visual/registry";

const props = defineProps<{ tab: Tab }>();
const c = computed(() => props.tab.compare!);
const viewId = computed(() => c.value.viewId);
const seq = useApp((s) => (viewId.value ? (s.views[viewId.value]?.info?.seq ?? -1) : -1));
const frame = useApp((s) => (viewId.value ? (s.views[viewId.value]?.frame ?? null) : null));
const dir = useApp((s) => (viewId.value ? (s.views[viewId.value]?.state.direction ?? "all") : "all"));
const selected = useApp((s) => (viewId.value ? (s.views[viewId.value]?.state.selected ?? null) : null));
const sum = shallowRef<CompareSummary | null>(null);
const busy = computed(() => c.value.phase !== "ready");

// the numbers follow the zoom root and the filter (W6 待定 3): fetched again for every frame
watch(
  [viewId, seq, () => c.value.info?.revision] as const,
  ([id, n], _old, onCleanup) => {
    if (!id || n < 0) return;
    let alive = true;
    onCleanup(() => (alive = false));
    void call("compare.summary", { view_id: id, limit: LIST_LIMIT })
      .then((r) => {
        if (alive) sum.value = r;
      })
      .catch(() => undefined);
  },
  { immediate: true },
);

// the same scale as the map: the rows its visualization fills
const legend = computed(() => {
  const f = frame.value;
  const g = f ? componentFor(f.layout)?.geometry : undefined;
  return diffTiers(f?.delta ? maxChange(f.delta, f.kind, g ? (row) => g.filled(f, row) : undefined) : 0);
});
const maxAbs = computed(() => Math.max(1, ...(sum.value?.grow_items ?? []).map((i) => Math.abs(i.delta)), ...(sum.value?.shrink_items ?? []).map((i) => Math.abs(i.delta))));

const lists = computed(() => {
  const s = sum.value;
  if (!s) return [];
  const out: { key: string; title: string; right: string; items: ChangeItem[]; empty: string }[] = [];
  if (dir.value !== "shrink") out.push({ key: "grow", title: t("增长最多"), right: t("前 {n} 项", { n: Math.min(LIST_LIMIT, s.grow_items.length) }), items: s.grow_items, empty: t("没有增长的项") });
  if (dir.value !== "grow") out.push({ key: "shrink", title: t("减少"), right: s.shrink ? fmtDelta(-s.shrink) : "0", items: s.shrink_items, empty: t("没有减少的项") });
  return out;
});

// ------------------------------------------------------------------ more menu

const moreOpen = ref(false);
const moreBox = ref<HTMLDivElement | null>(null);
const down = (e: MouseEvent) => {
  if (moreBox.value && !moreBox.value.contains(e.target as Node)) moreOpen.value = false;
};
watch(moreOpen, (now) => {
  if (now) window.addEventListener("mousedown", down, true);
  else window.removeEventListener("mousedown", down, true);
});
onScopeDispose(() => window.removeEventListener("mousedown", down, true));

function copy(): void {
  moreOpen.value = false;
  const s = sum.value;
  if (!s) return;
  const lines = [
    t("快照对比：{base} → {target}（相隔 {span}）", { base: sideName(s.base), target: sideName(s.target), span: fmtSpan(s.span_s) }),
    t("净变化 {net}，增长 {grow}，减少 {shrink}", { net: fmtDelta(s.net), grow: fmtSize(s.grow), shrink: fmtSize(s.shrink) }),
    t("增长最多："),
    ...s.grow_items.map((i) => `  ${fmtDelta(i.delta)}  ${i.path}`),
    t("减少："),
    ...s.shrink_items.map((i) => `  ${fmtDelta(i.delta)}  ${i.path}`),
  ];
  window.sk.shell.copyText(lines.join("\n"));
  toast(t("已复制对比汇总"));
}

// ------------------------------------------------------------------ summary

const summary = computed(() => {
  const s = sum.value;
  if (!s) return null;
  const label = s.net > 0 ? t("净增长") : s.net < 0 ? t("净减少") : t("无净变化");
  const [num, unit] = s.net === 0 ? ["0", ""] : sizeParts(Math.abs(s.net));
  const days = s.days_to_full;
  // an estimate from a few minutes of change says nothing: at least a day between the ends (W6 待定 1)
  const showFill = s.target.kind === "now" && s.span_s >= 86400 && s.daily != null && s.daily > 0 && days != null && days <= 365;
  const fill = showFill
    ? {
        warn: days! <= 30,
        text: plural(Math.floor(days!), t("日均 {daily} · 约 {days} 天后可用空间耗尽", { daily: fmtDelta(s.daily!), days: fmtInt(Math.floor(days!)) }), "Daily average {daily} · free space runs out in about 1 day", {
          daily: fmtDelta(s.daily!),
        }),
      }
    : null;
  return { label, num, unit, total: s.grow + s.shrink, fill };
});

// ------------------------------------------------------------------ timeline

const line = computed(() => timeline(c.value.sources, c.value.base, c.value.target));

/** click = base, Shift+click = target (W6 待定 13) */
function clickPoint(spec: string, shift: boolean): void {
  const cc = c.value;
  const id = props.tab.id;
  if (shift) {
    if (spec !== cc.target) void (spec === cc.base ? swapCompare(id) : setCompareEnds(id, cc.base, spec));
  } else if (spec !== cc.base) void (spec === cc.target ? swapCompare(id) : setCompareEnds(id, spec, cc.target));
}

const pointName = (spec: string, at: string) => (spec === NOW ? t("现在") : spec === c.value.base ? t("基准快照 {time}", { time: stamp(at) }) : t("快照 {time}", { time: stamp(at) }));
</script>

<template>
  <aside :class="['inspector', 'cmp-side', { busy }]" :aria-label="t('快照对比')" :aria-busy="busy">
    <div class="cmp-head">
      <strong>{{ t("快照对比") }}</strong>
      <span v-if="sum && !sum.whole" class="cmp-scope muted small" :title="sum.scope">
        {{ t("范围：{name}", { name: sum.scope.split(/[\\/]/).filter(Boolean).pop() ?? "" }) }}
      </span>
      <span class="flex" />
      <div ref="moreBox" class="cmp-more">
        <button class="icon-btn" :aria-label="t('更多对比选项')" :title="t('更多对比选项')" :aria-expanded="moreOpen" @click="moreOpen = !moreOpen">
          <Icon name="more" />
        </button>
        <div v-if="moreOpen" class="cmp-menu right" role="menu" @keydown="(e: KeyboardEvent) => e.key === 'Escape' && (moreOpen = false)">
          <button role="menuitem" class="cmp-option" :disabled="!sum" @click="copy">
            <span class="cmp-check"><Icon name="copy" :size="14" /></span>
            {{ t("复制汇总") }}
          </button>
          <button role="menuitem" class="cmp-option" disabled :title="t('导出对比报告还没有接入（见 TODO）')">
            <span class="cmp-check"><Icon name="upload" :size="14" /></span>
            {{ t("导出对比报告…") }}
          </button>
        </div>
      </div>
    </div>
    <div v-if="busy" class="cmp-progress" role="status">
      <span class="spinner" aria-hidden="true" />
      <span>{{ c.message || t("正在对比…") }}</span>
      <span v-if="c.progress != null && c.phase === 'comparing'" class="mono muted">{{ Math.round(c.progress * 100) }}%</span>
    </div>
    <div v-if="sum && summary" class="cmp-body">
      <section class="cmp-sum" :aria-label="t('变化汇总')">
        <div class="row-between small">
          <span class="muted">{{ summary.label }}</span>
          <span class="muted">{{ t("相隔 {span}", { span: fmtSpan(sum.span_s) }) }}</span>
        </div>
        <div class="cmp-big">
          <span class="mono">{{ sum.net === 0 ? "0" : `${sum.net > 0 ? "+" : "−"}${summary.num}` }}</span>
          <span v-if="summary.unit" class="cmp-unit">{{ summary.unit }}</span>
        </div>
        <div class="cmp-ratio" role="img" :aria-label="t('增长 {grow}，减少 {shrink}', { grow: fmtSize(sum.grow), shrink: fmtSize(sum.shrink) })">
          <span v-if="summary.total > 0" :style="{ flexGrow: sum.grow, background: growColors()[3] }" />
          <span v-if="summary.total > 0 && sum.shrink > 0" :style="{ flexGrow: sum.shrink, background: shrinkColors()[2] }" />
        </div>
        <div class="row-between small">
          <span><span class="cmp-swatch" :style="{ background: growColors()[3] }" /> {{ t("增长") }} <span class="mono">{{ fmtSize(sum.grow) }}</span></span>
          <span><span class="cmp-swatch" :style="{ background: shrinkColors()[2] }" /> {{ t("减少") }} <span class="mono">{{ fmtSize(sum.shrink) }}</span></span>
        </div>
        <div v-if="summary.fill" :class="['cmp-fill', { warn: summary.fill.warn }]" :title="t('按两端之间的日均变化推算')">
          <Icon :name="summary.fill.warn ? 'warning' : 'info'" :size="14" />
          {{ summary.fill.text }}
        </div>
      </section>
      <section class="cmp-legend" :aria-label="t('图例')">
        <div class="cmp-grid">
          <span />
          <span v-for="h in legend.heads" :key="h" class="muted small mono">{{ h }}</span>
          <span class="small">{{ t("增长") }}</span>
          <span v-for="col in growColors()" :key="col" class="cmp-chip" :style="{ background: col }" />
          <span class="small">{{ t("减少") }}</span>
          <span v-for="(col, i) in shrinkColors()" :key="col" class="cmp-chip" :style="{ background: col, gridColumn: i === 2 ? 'span 2' : undefined }" />
        </div>
        <div class="cmp-legend-notes small">
          <span><span class="cmp-badge new">{{ t("新增") }}</span> {{ t("新出现的项") }}</span>
          <span class="muted">{{ t("面积 = 变化量") }}</span>
        </div>
      </section>
      <section v-for="l in lists" :key="l.key" class="cmp-list" :aria-label="l.title">
        <div class="row-between cmp-list-head">
          <strong class="small">{{ l.title }}</strong>
          <span class="muted small mono">{{ l.right }}</span>
        </div>
        <div v-if="l.items.length === 0" class="muted small cmp-empty">{{ l.empty }}</div>
        <div role="list">
          <button
            v-for="it in l.items"
            :key="`${it.node}-${it.status}`"
            role="listitem"
            :class="['cmp-row', { on: selected === it.node }]"
            :title="it.path"
            @click="viewCall(viewId!, 'view.reveal', { key: it.node })"
            @mouseenter="patchView(viewId!, { hoverKey: it.node })"
            @mouseleave="patchView(viewId!, { hoverKey: null })"
          >
            <Icon :name="KIND_ICON[it.kind]" :size="14" />
            <span class="cmp-row-main">
              <span class="cmp-row-name">
                <span class="cmp-row-label">{{ it.label }}</span>
                <span v-if="it.status === 'new'" class="cmp-badge new">{{ t("新增") }}</span>
                <span v-if="it.status === 'deleted'" class="cmp-badge deleted">{{ t("已删除") }}</span>
                <span v-if="it.reason && it.status !== 'new' && it.status !== 'deleted'" class="cmp-reason">{{ it.reason }}</span>
              </span>
              <span class="cmp-bar" :style="{ width: `${(Math.abs(it.delta) / maxAbs) * 100}%`, background: diffColor(it.delta, legend.tiers) }" />
            </span>
            <span class="mono cmp-row-delta">{{ fmtDelta(it.delta) }}</span>
          </button>
        </div>
      </section>
    </div>
    <div v-else class="cmp-body muted cmp-wait">{{ c.phase === "failed" ? c.message : t("正在准备对比数据…") }}</div>
    <section class="cmp-timeline" :aria-label="t('快照时间轴')">
      <div class="row-between">
        <!-- a list of them: plural in English, one form in Chinese -->
        <strong class="small">{{ byLang("快照", "Snapshots") }}</strong>
        <button class="text-btn small" :title="t('把当前扫描保存为快照（Ctrl+S）')" @click="runCommand('snapshot.save')">
          <Icon name="snapshot" :size="14" /> {{ t("保存快照") }}
        </button>
      </div>
      <div class="cmp-track">
        <span class="cmp-rail" />
        <span class="cmp-span" :style="{ left: `${line.lo}%`, width: `${line.hi - line.lo}%` }" />
        <button
          v-for="p in line.points"
          :key="p.spec"
          :class="['cmp-dot', { base: p.spec === c.base, target: p.spec === c.target }]"
          :style="{ left: `${line.pos(p.at)}%` }"
          :aria-label="pointName(p.spec, p.at)"
          :aria-pressed="p.spec === c.base || p.spec === c.target"
          :title="t('{name}（单击设为基准，Shift+单击设为对比目标）', { name: p.spec === NOW ? t('现在') : stamp(p.at) })"
          @click="clickPoint(p.spec, $event.shiftKey)"
        >
          <span v-if="line.labelled.has(p.spec)" :class="['cmp-dot-label', { strong: p.spec === c.base, accent: p.spec === NOW && p.spec === c.target }]"
            >{{ p.spec === NOW ? t("现在") : day(p.at) }}{{ p.spec === c.base ? ` ${t("基准")}` : "" }}</span
          >
        </button>
      </div>
    </section>
  </aside>
</template>

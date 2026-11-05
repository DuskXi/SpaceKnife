<!-- W3 「匹配结果」 sidebar (UI-W3-27…34, UI-W3-38; shell/Results.tsx): summary, the expression as
     condition capsules, the result list (rows grouped by visible block, served page by page by
     view.results), key hints. -->
<script setup lang="ts">
import { computed, nextTick, onMounted, onScopeDispose, ref, shallowRef, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { useThrottled } from "@/composables/useThrottled";
import { classHex, inkOn } from "@/lib/classes";
import { fmtInt, fmtShare, fmtSize, sizeParts } from "@/lib/format";
import { splitPlaceholders } from "@/lib/hoverCard";
import { t } from "@/lib/i18n";
import {
  capsules,
  describeRow,
  extClasses,
  type ListRange,
  PAGE,
  pagesFor,
  pagesReady,
  pendingLabel,
  rangeAt,
  renderedRows,
  ROW_H,
  rowAt as pagedRow,
  scrollToRow,
  type Sort,
  SORTS,
  sortLabel,
} from "@/lib/results";
import { isRunning } from "@/lib/scanProgress";
import type { FilterNormal, ResultRow, ViewResults } from "@/protocol/generated";
import { basketAdd, cancellable, clearFilter, viewCall } from "@/state/actions";
import { get, patchView, set, useApp } from "@/state/store";

const props = defineProps<{ viewId: string }>();

const filter = useApp((s) => s.views[props.viewId]?.filter ?? null);
const parse = useApp((s) => s.views[props.viewId]?.parse ?? null);
const scanned = useApp((s) => s.views[props.viewId]?.info?.scanned ?? 0);
const seq = useApp((s) => s.views[props.viewId]?.info?.seq ?? 0);
const rootLabel = useApp((s) => s.views[props.viewId]?.info?.breadcrumb[0]?.label ?? "");
const scanId = useApp((s) => s.views[props.viewId]?.state.scan_id ?? "");
const storedSort = useApp((s) => (s.views[props.viewId]?.state.results_sort ?? "size") as Sort);
const vocab = useApp((s) => s.vocab);
const sort = ref<Sort>(storedSort.value);
const range = ref<ListRange>({ first: 0, count: 20 });
const shown = shallowRef<{ meta: ViewResults; pages: Map<number, ResultRow[]> } | null>(null);
const active = ref(-1);
const list = ref<HTMLDivElement | null>(null);
let cache = { key: "", pages: new Map<number, ResultRow[]>(), inflight: new Map<number, () => void>() };
// a new frame invalidates every page; while the scan runs (four frames a second) refetch once a second
const running = useApp((s) => isRunning(s.scans[scanId.value]));
const tick = useThrottled(seq, () => (running.value ? 1000 : 0));
const extClass = computed(() => extClasses(vocab.value?.classes));

watch([() => props.viewId, storedSort], ([, s]) => (sort.value = s));
// leaving (filter cleared, tab switched) must not leave a hover outline behind on the treemap
watch(
  () => props.viewId,
  (id, _old, onCleanup) => onCleanup(() => patchView(id, { hoverKey: null })),
  { immediate: true },
);
// pages still on their way when the panel goes are not needed any more
onScopeDispose(() => cache.inflight.forEach((cancel) => cancel()));

// fetch the pages under the visible range; keep showing the previous rows until they arrive
watch(
  [() => props.viewId, tick, sort, () => range.value.first, () => range.value.count, () => filter.value?.active] as const,
  ([viewId, n, order, , , on]) => {
    if (!on) return;
    const key = `${viewId}:${n}:${order}`;
    if (cache.key !== key) {
      cache.inflight.forEach((cancel) => cancel()); // pages of an older frame or order
      cache = { key, pages: new Map(), inflight: new Map() };
    }
    const c = cache;
    const need = pagesFor(range.value);
    const publish = (meta: ViewResults) => {
      if (pagesReady(need, c.pages, meta.total)) shown.value = { meta, pages: new Map(c.pages) };
    };
    for (const p of need) {
      if (c.pages.has(p) || c.inflight.has(p)) continue;
      const job = cancellable("view.results", { view_id: viewId, sort: order, offset: p * PAGE, limit: PAGE });
      c.inflight.set(p, job.cancel);
      job.promise
        .then((res) => {
          if (cache.key !== key) return;
          c.inflight.delete(p);
          if (!res) return;
          c.pages.set(p, res.rows);
          publish(res);
        })
        .catch(() => c.inflight.delete(p));
    }
  },
  { immediate: true },
);

function onScroll(): void {
  const el = list.value;
  if (!el) return;
  const r = rangeAt(el.scrollTop, el.clientHeight);
  if (r.first !== range.value.first || r.count !== range.value.count) range.value = r;
}
let ro: ResizeObserver | null = null;
onMounted(() => {
  if (!list.value || typeof ResizeObserver === "undefined") return;
  ro = new ResizeObserver(onScroll);
  ro.observe(list.value);
});
onScopeDispose(() => ro?.disconnect());

const meta = computed(() => shown.value?.meta ?? null);
const total = computed(() => meta.value?.total ?? 0);
const rowAt = (i: number): ResultRow | undefined => pagedRow(shown.value?.pages, i);

function changeSort(s: Sort): void {
  sort.value = s;
  const v = get().views[props.viewId];
  if (v) patchView(props.viewId, { state: { ...v.state, results_sort: s } });
  active.value = -1;
  list.value?.scrollTo({ top: 0 });
}

const hover = (key: number | null) => patchView(props.viewId, { hoverKey: key });
const locate = (r: ResultRow) => r.kind !== "rest" && void viewCall(props.viewId, "view.reveal", { key: r.key });
// the unified context menu for a result: its block is located first, so the menu has it (UI-W5-01)
async function menuFor(r: ResultRow, x: number, y: number): Promise<void> {
  if (r.kind === "rest") return;
  await viewCall(props.viewId, "view.reveal", { key: r.key });
  set({ menu: { viewId: props.viewId, scanId: scanId.value, key: r.key, node: r.kind === "group" ? null : r.node, x, y } });
}
const zoom = (r: ResultRow) => r.kind !== "rest" && void viewCall(props.viewId, "view.zoom", { key: r.key });
const toBasket = (r: ResultRow) => {
  if (r.kind === "file" || r.kind === "folder") void basketAdd(scanId.value, r.node);
};

function moveTo(i: number): void {
  const k = Math.max(0, Math.min(total.value - 1, i));
  active.value = k;
  const el = list.value;
  if (el) {
    const top = scrollToRow(k, el.scrollTop, el.clientHeight);
    if (top !== null) el.scrollTop = top;
  }
  const r = rowAt(k);
  if (r) hover(r.kind === "rest" ? null : r.key);
}

function onKey(e: KeyboardEvent): void {
  if (e.altKey || e.ctrlKey || e.metaKey) return; // Alt+↑, Ctrl+Home… are view shortcuts
  const a = active.value;
  const r = a >= 0 ? rowAt(a) : undefined;
  let handled = true;
  if (e.key === "ArrowDown") moveTo(a + 1);
  else if (e.key === "ArrowUp") moveTo(a - 1);
  else if (e.key === "Home") moveTo(0);
  else if (e.key === "End") moveTo(total.value - 1);
  else if (e.key === "PageDown") moveTo(a + Math.max(1, range.value.count - 1));
  else if (e.key === "PageUp") moveTo(a - Math.max(1, range.value.count - 1));
  else if (e.key === "Enter" && r) zoom(r);
  else if (e.key === "Delete" && r) toBasket(r);
  else if (e.key === " " && r) locate(r);
  else handled = false;
  if (handled) {
    e.preventDefault();
    e.stopPropagation();
  }
}

const files = computed(() => filter.value?.files ?? 0);
const alloc = computed(() => filter.value?.alloc ?? 0);
const big = computed(() => sizeParts(alloc.value));
const share = computed(() => splitPlaceholders(t("占已扫描 {size} 的 {share}")));

/** The rendered rows: index, row (undefined while its page is on its way) and what it says. */
const rows = computed(() => {
  const [first, last] = renderedRows(range.value, total.value);
  const out: { i: number; r: ResultRow | undefined; d: ReturnType<typeof describeRow> | null }[] = [];
  for (let i = first; i < last; i++) {
    const r = rowAt(i);
    out.push({ i, r, d: r ? describeRow(r, rootLabel.value, extClass.value) : null });
  }
  return out;
});
const iconStyle = (cls: string | null) => {
  if (!cls) return undefined;
  const hex = classHex(vocab.value, cls);
  return { background: hex, color: inkOn(hex) };
};

function onRowClick(i: number, r: ResultRow | undefined): void {
  active.value = i;
  list.value?.focus({ preventScroll: true });
  if (r) locate(r);
}
function onRowMenu(i: number, r: ResultRow | undefined, e: MouseEvent): void {
  active.value = i;
  if (r) void menuFor(r, e.clientX, e.clientY);
}

// ------------------------------------------------------------------ sort menu

const sortOpen = ref(false);
const sortBox = ref<HTMLDivElement | null>(null);
const closeSort = (e: MouseEvent) => {
  if (!sortBox.value?.contains(e.target as Node)) sortOpen.value = false;
};
watch(sortOpen, async (open) => {
  if (!open) return window.removeEventListener("mousedown", closeSort);
  window.addEventListener("mousedown", closeSort);
  await nextTick();
  sortBox.value?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus(); // autoFocus
});
onScopeDispose(() => window.removeEventListener("mousedown", closeSort));
function pickSort(s: Sort): void {
  sortOpen.value = false;
  if (s !== sort.value) changeSort(s);
}
function onSortKey(e: KeyboardEvent): void {
  if (e.key === "Escape") {
    e.stopPropagation();
    sortOpen.value = false;
  }
}

// ------------------------------------------------------------------ conditions (UI-W3-30)

let lastValid: FilterNormal | null = null;
const conditions = computed(() => {
  const p = parse.value;
  if (p?.normal) lastValid = p.normal;
  const normal = p?.normal ?? lastValid;
  if (!normal) return null;
  const v = vocab.value;
  const caps = capsules(normal, (id) => v?.classes.find((c) => c.id === id)?.name ?? id);
  const pending = p?.pending?.category ?? null;
  if (!caps.length && !pending) return null;
  return { caps, pending };
});
</script>

<template>
  <aside class="inspector results" :aria-label="t('匹配结果')">
    <div class="res-head">
      <h2>{{ t("匹配结果") }}</h2>
      <span class="flex" />
      <button class="ghost small res-exit" :title="t('清除过滤，回到详情（Esc）')" @click="clearFilter(viewId)">
        <Icon name="close" :size="12" />
        {{ t("清除过滤") }}
      </button>
      <div ref="sortBox" class="res-sort">
        <button class="ghost small" aria-haspopup="menu" :aria-expanded="sortOpen" :aria-label="t('排序：{sort}', { sort: sortLabel(sort) })" @click="sortOpen = !sortOpen">
          {{ sortLabel(sort) }}
          <Icon name="chevronDown" :size="12" />
        </button>
        <div v-if="sortOpen" class="menu" role="menu" :aria-label="t('排序方式')" @keydown="onSortKey">
          <button v-for="s in SORTS" :key="s" role="menuitemradio" :aria-checked="s === sort" class="menu-item" @click="pickSort(s)">
            <span class="menu-check"><Icon v-if="s === sort" name="check" :size="14" /></span>
            {{ sortLabel(s) }}
          </button>
        </div>
      </div>
      <button class="icon-btn small" :aria-label="t('隐藏侧栏')" :title="t('隐藏侧栏')" @click="set({ inspectorHidden: true })">
        <Icon name="sidebar" />
      </button>
    </div>
    <div class="res-top">
      <div class="res-summary">
        <div class="big-num">
          <span class="num">{{ big[0] }}</span>
          <span class="unit">{{ big[1] }}</span>
          <span class="res-count mono">{{ t("{n} 项", { n: fmtInt(files) }) }}</span>
        </div>
        <div class="bar" role="img" :aria-label="t('占已扫描的 {share}', { share: fmtShare(alloc, scanned) })">
          <div :style="{ width: `${Math.min(100, scanned ? (alloc / scanned) * 100 : 0)}%` }" />
        </div>
        <div class="small muted"
          ><template v-for="(part, k) in share" :key="k"
            ><template v-if="!part.slot">{{ part.text }}</template
            ><span v-else-if="part.text === 'size'" class="mono strong">{{ fmtSize(scanned) }}</span
            ><span v-else-if="part.text === 'share'" class="mono strong">{{ fmtShare(alloc, scanned) }}</span></template
          ></div
        >
      </div>
      <section v-if="conditions" class="res-conds" :aria-label="t('条件')">
        <h3>{{ t("条件") }}</h3>
        <div class="conds">
          <span v-for="(c, ci) in conditions.caps" :key="ci" class="cond">
            <span class="cond-label">{{ c.label }}</span>
            <span v-for="(v, k) in c.values" :key="k"
              ><span v-if="k > 0" class="cond-join">{{ c.joiner }}</span><code :class="v.cls">{{ v.text }}</code></span
            >
          </span>
          <span v-if="conditions.pending" class="cond pending">
            <span class="cond-label">{{ pendingLabel(conditions.pending) }}</span>
            <span class="muted">{{ t("选择中…") }}</span>
          </span>
        </div>
      </section>
    </div>
    <div class="res-cols" aria-hidden="true">
      <span>{{ t("名称") }}</span>
      <span>{{ t("大小") }}</span>
    </div>
    <div
      ref="list"
      class="res-list"
      role="listbox"
      :aria-label="t('匹配结果列表')"
      tabindex="0"
      :aria-activedescendant="active >= 0 ? `${viewId}-res-${active}` : undefined"
      @scroll="onScroll"
      @keydown="onKey"
      @mouseleave="hover(null)"
      @blur="hover(null)"
    >
      <div :style="{ height: `${total * ROW_H}px` }" class="res-spacer">
        <template v-for="{ i, r, d } in rows" :key="i">
          <div v-if="!r || !d" class="res-row loading" :style="{ top: `${i * ROW_H}px` }" aria-hidden="true" />
          <div
            v-else
            :id="`${viewId}-res-${i}`"
            :class="['res-row', { on: i === active, rest: r.kind === 'rest' }]"
            :style="{ top: `${i * ROW_H}px` }"
            role="option"
            :aria-selected="i === active"
            :aria-label="d.spoken"
            @mouseenter="hover(r.kind === 'rest' ? null : r.key)"
            @click="onRowClick(i, r)"
            @dblclick="zoom(r)"
            @contextmenu.prevent="onRowMenu(i, r, $event)"
          >
            <span :class="['res-icon', r.kind]" :style="iconStyle(d.cls)">
              <Icon :name="r.kind === 'file' ? 'file' : r.kind === 'folder' ? 'folder' : 'layers'" :size="15" />
            </span>
            <span class="res-text">
              <span class="res-name">
                <span class="res-label" :title="d.name">{{ d.name }}</span>
                <span v-if="r.tag" :class="['tag-mini', r.tag, { inherited: r.tag_inherited }]" role="img" :aria-label="d.tag" :title="d.tag" />
                <span v-if="r.kind === 'folder'" class="res-n mono">{{ t("{n} 项", { n: fmtInt(r.count) }) }}</span>
                <span v-if="r.kind === 'group' && r.name && r.count > 1" class="res-n mono">{{ t("{n} 项", { n: fmtInt(r.count) }) }}</span>
              </span>
              <span class="res-path mono" :title="d.path">
                <span>{{ d.path }}</span>
              </span>
            </span>
            <span class="res-size">
              <span class="mono">{{ fmtSize(r.alloc) }}</span>
              <span class="res-bar">
                <span :style="{ width: `${meta?.max_alloc ? Math.max(2, (r.alloc / meta.max_alloc) * 100) : 0}%` }" />
              </span>
            </span>
          </div>
        </template>
      </div>
      <div v-if="meta && total === 0" class="res-empty">
        <Icon name="search" :size="20" />
        <div>{{ t("当前位置下没有匹配项") }}</div>
        <div class="small muted">{{ t("放宽条件，或回到上一级（Alt+↑）在更大的范围里找") }}</div>
      </div>
    </div>
    <div class="res-hints small muted">
      <span>{{ t("单击定位") }}</span>
      <span><Kbd>Enter</Kbd> {{ t("放大进入") }}</span>
      <span><Kbd>Delete</Kbd> {{ t("加入删除篮") }}</span>
    </div>
  </aside>
</template>

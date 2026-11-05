<!-- Details panel (UI-MAIN-32…47, G11): details / class statistics / largest files of the selection
     (shell/Inspector.tsx). -->
<script setup lang="ts">
import { computed, ref, shallowRef, watch } from "vue";

import InspectorClassStats from "@/components/shell/InspectorClassStats.vue";
import InspectorTopRow from "@/components/shell/InspectorTopRow.vue";
import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { useThrottled } from "@/composables/useThrottled";
import { classHex, inkOn } from "@/lib/classes";
import { fmtAgo, fmtInt, fmtShare, fmtSize, plural, sizeParts } from "@/lib/format";
import { inBrowser } from "@/lib/host";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import { notOnDisk, revealLabel } from "@/lib/reveal";
import { isRunning } from "@/lib/scanProgress";
import { tagLabel } from "@/lib/tags";
import { summaryInterval } from "@/lib/throttle";
import type { TreeSummaryResult } from "@/protocol/generated";
import { basketAdd, cancellable, viewCall } from "@/state/actions";
import { set, toast, useApp } from "@/state/store";
import { Kind } from "@/visual/frame";

type Page = "details" | "classes" | "largest";
const PAGES: readonly Page[] = ["details", "classes", "largest"];
// red ▲, yellow ■, green ●, blue ◆: the names come from tagLabel (W7 「标签」 · 显示名)
const TAGS = [
  { color: "red", key: 1 },
  { color: "yellow", key: 2 },
  { color: "green", key: 3 },
  { color: "blue", key: 4 },
] as const;
const pageLabel = (p: Page) => ({ details: t("详情"), classes: t("类别统计"), largest: t("最大文件") })[p];

const props = defineProps<{ viewId: string }>();

const view = useApp((s) => s.views[props.viewId]);
const vocab = useApp((s) => s.vocab);
const settings = useApp((s) => s.settings); // tag display names (W7 标签)
const page = ref<Page>("details");
const summary = shallowRef<TreeSummaryResult | null>(null);
const frame = computed(() => view.value?.frame ?? null);
const scanId = computed(() => view.value?.state.scan_id);
const selRow = computed(() => frame.value?.info.selected_row ?? null);
const selKind = computed(() => (selRow.value != null && frame.value ? frame.value.kind[selRow.value]! : null));
// the selection, or the zoom root when nothing is selected (00 待定 6)
const node = computed(() => (selRow.value != null && frame.value && selKind.value! < Kind.SMALL ? frame.value.node[selRow.value]! : (view.value?.state.zoom_node ?? null)));
const synthetic = computed(() => (selRow.value != null && frame.value && selKind.value! >= Kind.SMALL ? selRow.value : null));
const seq = computed(() => frame.value?.info.seq ?? 0);
const running = useApp((s) => (scanId.value ? isRunning(s.scans[scanId.value]) : false));
// every frame may change the totals, but a summary walks the whole subtree: refresh it at most
// once a second while the scan runs (four frames a second), and the less often the larger the
// subtree (live monitoring of a busy disk changes something every few seconds); a request the
// next one replaces is dropped
const items = computed(() => (summary.value && summary.value.node.id === node.value ? summary.value.node.files + summary.value.node.dirs : 0));
const tick = useThrottled(seq, () => summaryInterval(items.value, running.value));

watch(
  [scanId, node, page, tick] as const,
  ([scan, n, p], _old, onCleanup) => {
    if (!scan || n == null || n < 0) return;
    let alive = true;
    const job = cancellable("tree.summary", { scan_id: scan, node: n, top: p === "largest" ? 50 : 5 });
    job.promise
      .then((r) => {
        if (alive && r) summary.value = r;
      })
      .catch(() => {});
    onCleanup(() => {
      alive = false;
      job.cancel();
    });
  },
  { immediate: true },
);

const className = (id: string) => vocab.value?.classes.find((c) => c.id === id)?.name ?? id;
const hexOf = (id: string) => classHex(vocab.value, id);

const moreActions = () => document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "F10", shiftKey: true, bubbles: true }));

const actions = computed(() => {
  const s = summary.value;
  if (!s) return [];
  const list: { icon: IconName; label: string; keys: string; danger?: boolean; disabled?: boolean; run: () => void }[] = [];
  if (!inBrowser()) list.push({ icon: "reveal", label: revealLabel(), keys: "Ctrl+E", disabled: !s.on_disk, run: () => window.sk.shell.showItemInFolder(s.path) });
  list.push({
    icon: "copy",
    label: t("复制路径"),
    keys: "Ctrl+C",
    run: () => {
      window.sk.shell.copyText(s.path);
      toast(t("已复制"));
    },
  });
  list.push({ icon: "basket", danger: true, label: t("加入删除篮"), keys: "Delete", disabled: !s.on_disk, run: () => void (scanId.value && basketAdd(scanId.value, s.node.id)) });
  return list;
});

// ------------------------------------------------------------------ details

const details = computed(() => {
  const s = summary.value;
  if (!s) return null;
  const n = s.node;
  const dir = n.kind === "dir";
  const [num, unit] = sizeParts(n.alloc);
  const sep = s.path.includes("\\") ? "\\" : "/";
  const cut = s.path.lastIndexOf(sep, s.path.length - 2);
  const vol = s.volume;
  const used = vol ? vol.total - vol.free : 0;
  const cls = !dir ? s.classes[0]?.class_id : undefined;
  let stats: string;
  if (dir) {
    const fields = { files: fmtInt(n.files), dirs: fmtInt(n.dirs), ago: fmtAgo(s.latest_mtime) };
    const text = t("{files} 个文件 · {dirs} 个文件夹 · 修改于 {ago}", fields);
    const one =
      n.files === 1 && n.dirs === 1 ? "{files} file · {dirs} folder · modified {ago}" : n.files === 1 ? "{files} file · {dirs} folders · modified {ago}" : "{files} files · {dirs} folder · modified {ago}";
    stats = n.files === 1 || n.dirs === 1 ? plural(1, text, one, fields) : text;
  } else stats = t("{cls} · 修改于 {ago}", { cls: className(cls ?? "other"), ago: fmtAgo(n.mtime) });
  const shares: { label: string; part: number; whole: number }[] = [];
  if (s.parent) shares.push({ label: t("占 {name}", { name: s.parent.name.split(/[\\/]/).filter(Boolean).pop() ?? s.parent.name }), part: n.alloc, whole: s.parent.alloc });
  if (vol && used > 0) shares.push({ label: t("占 {volume} 已用", { volume: vol.mount_point.replace(/[\\/]+$/, "") }), part: n.alloc, whole: used });
  const fc = s.folder_class;
  const folderClass = fc
    ? {
        hex: hexOf(fc.class_id),
        text: fc.fallback ? t("其中没有类别的文件算作「{cls}」 · {reason}", { cls: className(fc.class_id), reason: fc.reason }) : t("整个文件夹算作「{cls}」 · {reason}", { cls: className(fc.class_id), reason: fc.reason }),
        title: t("由 {folder} 决定。右键菜单的「归类」可以更改", { folder: fc.folder }),
      }
    : null;
  return {
    n,
    dir,
    num,
    unit,
    head: cut > 0 ? s.path.slice(0, cut + 1) : "",
    tail: cut > 0 ? s.path.slice(cut + 1) : s.path,
    icon: cls ? { background: hexOf(cls), color: inkOn(hexOf(cls)) } : undefined,
    stats,
    shares,
    folderClass,
  };
});

const tags = computed(() => TAGS.map((tg) => ({ ...tg, name: t("{tag}（Ctrl+{key}）", { tag: tagLabel(settings.value, tg.color), key: tg.key }) })));

// ------------------------------------------------------------------ a synthetic block

const syntheticView = computed(() => {
  const row = synthetic.value;
  const f = frame.value;
  if (row == null || !f) return null;
  const kind = f.kind[row]!;
  const count = f.count_[row]!;
  const [num, unit] = sizeParts(Number(f.size[row]));
  const text =
    kind === Kind.SMALL
      ? [plural(count, t("{n} 个小项", { n: fmtInt(count) }), "1 small item"), t("同一文件夹里单独画不下的小文件合并成一块。双击可以把它们逐个铺开。")]
      : kind === Kind.MORE
        ? [t("+{n} 项", { n: fmtInt(count) }), t("同级里画不下的条目合并成一块。双击可以把它们铺开。")]
        : kind === Kind.FREE
          ? [t("可用空间"), t("系统报告的卷可用空间。按 F 显示或隐藏。")]
          : [t("未知空间"), t("未知空间 = 系统报告的已用 − 已扫描合计，通常是无权限或系统保留的区域。以管理员身份扫描可以减少这部分（M4）。")];
  return { num, unit, title: text[0], text: text[1] };
});
</script>

<template>
  <aside class="inspector" :aria-label="t('详情面板')">
    <div class="inspector-head">
      <div class="segtabs" role="tablist" :aria-label="t('侧栏视图')">
        <button v-for="p in PAGES" :key="p" role="tab" :aria-selected="page === p" :class="page === p ? 'on' : ''" @click="page = p">
          {{ pageLabel(p) }}
        </button>
      </div>
      <div class="inspector-tools">
        <button class="icon-btn small" :aria-label="t('更多操作')" :title="t('更多操作（Shift+F10）')" @click="moreActions">
          <Icon name="more" />
        </button>
        <button class="icon-btn small" :aria-label="t('隐藏详情面板')" :title="t('隐藏详情面板')" @click="set({ inspectorHidden: true })">
          <Icon name="sidebar" />
        </button>
      </div>
    </div>
    <div class="inspector-body">
      <template v-if="syntheticView">
        <div class="ins-name">
          <h2>{{ syntheticView.title }}</h2>
        </div>
        <div class="big-num">
          <span class="num">{{ syntheticView.num }}</span>
          <span class="unit">{{ syntheticView.unit }}</span>
        </div>
        <p class="muted">{{ syntheticView.text }}</p>
      </template>
      <div v-else-if="!summary" class="muted">{{ t("正在读取…") }}</div>
      <template v-else-if="page === 'details' && details">
        <div class="ins-title">
          <div class="ins-icon" :style="details.icon">
            <Icon :name="details.dir ? 'folder' : 'file'" :size="18" />
          </div>
          <div class="ins-name">
            <h2>{{ details.n.name.split(/[\\/]/).filter(Boolean).pop() ?? details.n.name }}</h2>
            <div class="ins-path mono">{{ details.head }}<span>{{ details.tail }}</span></div>
          </div>
        </div>
        <div class="ins-size">
          <div class="big-num">
            <span class="num">{{ details.num }}</span>
            <span class="unit">{{ details.unit }}</span>
          </div>
          <div class="ins-stats" :title="t('精确 {alloc} 字节（逻辑 {size} 字节）', { alloc: fmtInt(details.n.alloc), size: fmtInt(details.n.size) })">
            {{ details.stats }}
          </div>
          <div v-if="details.folderClass" class="ins-fclass" :title="details.folderClass.title">
            <span class="swatch" :style="{ background: details.folderClass.hex }" />
            <span class="ins-fclass-text">{{ details.folderClass.text }}</span>
          </div>
        </div>
        <section class="ins-section">
          <h3>{{ t("占比") }}</h3>
          <div v-for="sh in details.shares" :key="sh.label" class="share">
            <div class="row-between small">
              <span class="muted">{{ sh.label }}</span>
              <span class="mono">
                <span class="strong">{{ fmtShare(sh.part, sh.whole) }}</span> <span class="muted">/ {{ fmtSize(sh.whole) }}</span>
              </span>
            </div>
            <div class="bar">
              <div :style="{ width: `${Math.min(100, sh.whole ? (sh.part / sh.whole) * 100 : 0)}%` }" />
            </div>
          </div>
        </section>
        <section v-if="details.dir && summary.top_files.length > 0" class="ins-section">
          <h3>{{ t("最大的子项") }}</h3>
          <div class="top-list">
            <InspectorTopRow
              v-for="f in summary.top_files.slice(0, 5)"
              :key="f.node.id"
              :node="f.node"
              :rel="f.rel_parent"
              :tag="f.tag ?? null"
              :cls="f.class_id"
              :whole="details.n.alloc"
              :view-id="viewId"
            />
          </div>
        </section>
        <section class="ins-section">
          <div class="row-between">
            <h3>{{ t("标签") }}</h3>
            <span class="muted small">{{ t("Ctrl+1–4 · Ctrl+0 清除") }}</span>
          </div>
          <div class="tag-grid" role="group" :aria-label="t('颜色标签')">
            <button
              v-for="tg in tags"
              :key="tg.color"
              :aria-label="tg.name"
              :title="tg.name"
              :aria-pressed="summary.tag === tg.color"
              :class="['tag-btn', { on: summary.tag === tg.color }]"
              @click="viewCall(viewId, 'view.tag', { color: summary.tag === tg.color ? null : tg.color })"
            >
              <span :class="['tag-shape', tg.color]" />
              <span class="mono muted">{{ tg.key }}</span>
            </button>
          </div>
        </section>
      </template>
      <InspectorClassStats v-else-if="page === 'classes'" :s="summary" :view-id="viewId" :class-name="className" />
      <section v-else class="ins-section">
        <h3>{{ plural(summary.top_files.length, t("最大的 {n} 个文件", { n: summary.top_files.length }), "1 largest file") }}</h3>
        <div class="top-list">
          <InspectorTopRow
            v-for="f in summary.top_files"
            :key="f.node.id"
            :node="f.node"
            :rel="f.rel_parent"
            :tag="f.tag ?? null"
            :cls="f.class_id"
            :whole="summary.node.alloc"
            :view-id="viewId"
          />
        </div>
      </section>
    </div>
    <div v-if="summary && synthetic == null" class="inspector-actions">
      <button v-for="a in actions" :key="a.icon" class="action-row" :disabled="a.disabled" @click="a.run">
        <span :class="a.danger ? 'danger' : 'muted'">
          <Icon :name="a.icon" />
        </span>
        <span class="flex">{{ a.label }}</span>
        <Kbd>{{ a.keys }}</Kbd>
      </button>
      <div v-if="!summary.on_disk" class="muted small inspector-note">{{ notOnDisk() }}</div>
    </div>
  </aside>
</template>

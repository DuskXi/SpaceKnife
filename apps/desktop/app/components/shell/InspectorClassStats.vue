<!-- 「类别统计」 (UI-MAIN-65; ClassStats in shell/Inspector.tsx): the scope's make-up as one stacked
     bar, then a row per class. SpaceKnife's own classes are niche: listed only when they hold a real
     share of the scope, the rest behind 「显示全部类别」. -->
<script setup lang="ts">
import { computed, ref, watch } from "vue";

import { classHex } from "@/lib/classes";
import { fmtCount, fmtInt, fmtShare, fmtSize, plural } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import type { ClassStat, TreeSummaryResult } from "@/protocol/generated";
import { editFilter } from "@/state/actions";
import { patchView, readFlag, useApp, writeFlag } from "@/state/store";

const NICHE = new Set(["code", "data"]);
const NICHE_SHARE = 0.02;
const NICHE_BYTES = 8 * 2 ** 30;
const ALL_KEY = "sk.classStatsAll";

const props = defineProps<{ s: TreeSummaryResult; viewId: string; className: (id: string) => string }>();

const vocab = useApp((st) => st.vocab);
const hexOf = (id: string) => classHex(vocab.value, id);
const all = ref(readFlag(ALL_KEY, false));
const hot = ref<string | null>(null);
// the class under the pointer lights up in the picture (UI-MAIN-65), only within this folder
function setHot(id: string | null): void {
  hot.value = id;
  patchView(props.viewId, { hoverClass: id === null ? null : { id, node: props.s.node.id } });
}
watch(
  [() => props.viewId, () => props.s.node.id] as const,
  ([viewId], _old, onCleanup) => onCleanup(() => patchView(viewId, { hoverClass: null })),
  { immediate: true },
);

const total = computed(() => props.s.classes.reduce((a, c) => a + c.bytes, 0));
const files = computed(() => props.s.classes.reduce((a, c) => a + c.files, 0));
const minor = (c: ClassStat) => NICHE.has(c.class_id) && c.bytes < NICHE_SHARE * total.value && c.bytes < NICHE_BYTES;
const folded = computed(() => props.s.classes.filter(minor));
const rows = computed(() => (all.value ? props.s.classes : props.s.classes.filter((c) => !minor(c))));
// columns as wide as their longest value (mono: 1ch a character), never narrower than the heading
const cols = computed(() => {
  const widest = (f: (c: ClassStat) => string) => Math.max(5, ...rows.value.map((c) => f(c).length));
  return {
    "--cs-files": `${widest((c) => fmtInt(c.files))}ch`,
    "--cs-files-short": `${widest((c) => fmtCount(c.files))}ch`,
    "--cs-size": `${widest((c) => fmtSize(c.bytes))}ch`,
  };
});
const name = computed(() => props.s.node.name.split(/[\\/]/).filter(Boolean).pop() ?? props.s.node.name);

function describe(c: ClassStat): string {
  const about = t("{name}：{files} 个文件，{size}，占 {share}", { name: props.className(c.class_id), files: fmtInt(c.files), size: fmtSize(c.bytes), share: fmtShare(c.bytes, total.value) });
  if (!c.parts.length) return about;
  // a folder class took files over (「游戏 · 其中视频 3 GB」)
  const parts = c.parts.slice(0, 5).map((p) => t("{name} {size}", { name: props.className(p.class_id), size: fmtSize(p.bytes) }));
  return t("{about}。其中按扩展名本是：{parts}", { about, parts: parts.join(byLang("、", ", ")) });
}

function toggle(): void {
  writeFlag(ALL_KEY, !all.value);
  all.value = !all.value;
}
</script>

<template>
  <section class="cstat" :style="cols">
    <header class="cstat-head">
      <h3 class="cstat-title" :title="s.path">{{ name }}</h3>
      <span class="cstat-total mono">{{ fmtSize(total) }}</span>
    </header>
    <div class="cstat-sub">{{ plural(files, t("按文件类别 · {n} 个文件", { n: fmtInt(files) }), "By file class · 1 file") }}</div>
    <div class="cstat-stack" role="img" :aria-label="s.classes.map(describe).join(' · ')">
      <span
        v-for="c in s.classes.filter((x) => x.bytes > 0)"
        :key="c.class_id"
        :class="hot && hot !== c.class_id ? 'dim' : undefined"
        :title="describe(c)"
        :style="{ flexGrow: c.bytes, background: hexOf(c.class_id) }"
        @mouseenter="setHot(c.class_id)"
        @mouseleave="setHot(null)"
      />
    </div>
    <div class="cstat-cols" aria-hidden="true">
      <span />
      <span>{{ t("类别") }}</span>
      <span class="cstat-files">{{ t("文件数") }}</span>
      <span class="cstat-size">{{ t("大小") }}</span>
      <span class="cstat-pct">{{ t("占比") }}</span>
    </div>
    <ul class="cstat-list">
      <li v-for="c in rows" :key="c.class_id">
        <button
          :class="['cstat-row', { hot: hot === c.class_id, minor: minor(c) }]"
          :title="t('{about}。点击按这一类过滤', { about: describe(c) })"
          @click="editFilter(viewId, `:class:${className(c.class_id)}`, null, true)"
          @mouseenter="setHot(c.class_id)"
          @mouseleave="setHot(null)"
          @focus="setHot(c.class_id)"
          @blur="setHot(null)"
        >
          <span class="swatch" :style="{ background: hexOf(c.class_id) }" />
          <span class="cstat-name">{{ className(c.class_id) }}</span>
          <span class="cstat-files mono">
            <span class="full">{{ fmtInt(c.files) }}</span>
            <span class="short">{{ fmtCount(c.files) }}</span>
          </span>
          <span class="cstat-size mono">{{ fmtSize(c.bytes) }}</span>
          <span class="cstat-pct mono">{{ fmtShare(c.bytes, total) }}</span>
          <span class="cstat-bar">
            <span :style="{ width: `${total ? (c.bytes / total) * 100 : 0}%`, background: hexOf(c.class_id) }" />
          </span>
        </button>
      </li>
    </ul>
    <button v-if="folded.length > 0" class="cstat-more" :aria-pressed="all" @click="toggle">
      {{ all ? t("只显示主要类别") : t("显示全部类别（另有 {n} 个小众类别，共 {size}）", { n: folded.length, size: fmtSize(folded.reduce((a, c) => a + c.bytes, 0)) }) }}
    </button>
  </section>
</template>

<!-- W8 pane header: its own breadcrumb and filter (G7; PaneHead in pages/ScanPage.tsx). -->
<script setup lang="ts">
import { computed } from "vue";

import FilterBox from "@/components/shell/FilterBox.vue";
import Icon from "@/components/ui/Icon.vue";
import { fmtInt, fmtSize } from "@/lib/format";
import { splitPlaceholders } from "@/lib/hoverCard";
import { t } from "@/lib/i18n";
import { crumbLabel, jumpUp } from "@/lib/scanPage";
import { closePane } from "@/state/actions";
import { type Tab, useApp } from "@/state/store";

const props = defineProps<{ tab: Tab; viewId: string; index: number }>();

const crumbs = useApp((s) => s.views[props.viewId]?.info?.breadcrumb ?? []);
const scanned = useApp((s) => s.views[props.viewId]?.info?.scanned ?? 0);
const filter = useApp((s) => s.views[props.viewId]?.filter ?? null);
const scan = useApp((s) => (props.tab.scanId ? s.scans[props.tab.scanId] : undefined));
const focused = computed(() => props.tab.focusedPane === props.index);
const left = computed(() => props.index === 0);
const side = computed(() => (left.value ? t("左窗格") : t("右窗格")));
const size = computed(() => {
  const root = scan.value?.roots.length === 1 ? scan.value.roots[0] : undefined;
  return crumbs.value.length <= 1 && root?.whole_volume && root.volume ? t("整个卷 · {size}", { size: fmtSize(root.volume.total, true) }) : fmtSize(scanned.value);
});
</script>

<template>
  <div :class="['pane-head', { focused }]" role="region" :aria-label="focused ? (left ? t('左窗格（焦点）') : t('右窗格（焦点）')) : side">
    <nav class="pane-crumbs" :aria-label="left ? t('左窗格路径') : t('右窗格路径')">
      <span v-for="(c, i) in crumbs" :key="`${c.key}-${i}`" class="crumb-seg">
        <span v-if="i > 0" class="crumb-arrow"><Icon name="chevronRight" :size="11" /></span>
        <span v-if="i === crumbs.length - 1" class="pane-crumb cur">{{ crumbLabel(c, i) }}</span>
        <button v-else class="pane-crumb" @click="jumpUp(viewId, crumbs.length - 1 - i)">{{ crumbLabel(c, i) }}</button>
      </span>
    </nav>
    <span class="pane-size muted">{{ size }}</span>
    <span class="flex" />
    <span v-if="filter?.active" class="pane-match" role="status"
      ><template v-for="(part, k) in splitPlaceholders(t('匹配 {n} 项 · {size}'))" :key="k"
        ><template v-if="!part.slot">{{ part.text }}</template
        ><span v-else-if="part.text === 'n'" class="mono">{{ fmtInt(filter.files) }}</span
        ><span v-else-if="part.text === 'size'" class="mono">{{ fmtSize(filter.alloc) }}</span></template
      ></span
    >
    <FilterBox :view-id="viewId" :pane="side" />
    <button class="icon-btn tiny" :aria-label="left ? t('关闭左窗格') : t('关闭右窗格')" :title="left ? t('关闭左窗格') : t('关闭右窗格')" @click="closePane(tab.id, viewId)">
      <Icon name="close" :size="14" />
    </button>
  </div>
</template>

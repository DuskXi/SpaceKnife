<!-- A scan tab (pages/ScanPage.tsx): the view (one or two panes, W8) + details panel + filter floater
     (W3) + hover card. In compare mode (W6) the change tree and the 「快照对比」 panel; while new ends
     compute, the old result stays, dimmed (UI-W6-41). -->
<script setup lang="ts">
import { computed } from "vue";

import ScanPane from "@/components/pages/ScanPane.vue";
import BasketDrawer from "@/components/shell/BasketDrawer.vue";
import ColumnResizer from "@/components/shell/ColumnResizer.vue";
import CompareSidebar from "@/components/shell/CompareSidebar.vue";
import Inspector from "@/components/shell/Inspector.vue";
import Results from "@/components/shell/Results.vue";
import ScanBand from "@/components/shell/ScanBand.vue";
import ScanDetails from "@/components/shell/ScanDetails.vue";
import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { isRunning } from "@/lib/scanProgress";
import { columnWidth } from "@/lib/scanPage";
import { focusPane } from "@/state/actions";
import { set, type Tab, useApp } from "@/state/store";

const props = defineProps<{ tab: Tab }>();

const basketOpen = useApp((s) => s.basketOpen);
const hidden = useApp((s) => s.inspectorHidden);
const width = useApp((s) => s.inspectorWidth);

// ------------------------------------------------------------------ compare mode
const busy = computed(() => props.tab.compare != null && props.tab.compare.phase !== "ready");

// ------------------------------------------------------------------ a scan
const focused = computed(() => props.tab.viewIds[props.tab.focusedPane] ?? props.tab.viewIds[0]);
// the side panel follows the focused view: match results while it filters (UI-W3-27, UI-W3-36)
const filtering = useApp((s) => (focused.value ? !!s.views[focused.value]?.filter?.active : false));
const split = computed(() => props.tab.viewIds.length > 1);
// while scanning: the progress band, and 「扫描详情」 in the side panel unless something is selected (W2 待定 15)
const scan = useApp((s) => (props.tab.scanId ? s.scans[props.tab.scanId] : undefined));
const selected = useApp((s) => (focused.value ? s.views[focused.value]?.state.selected != null : false));
const running = computed(() => isRunning(scan.value));
</script>

<template>
  <div v-if="tab.compare" class="content column">
    <div class="content-row" :style="columnWidth(width)">
      <div :class="['panes', { 'cmp-busy': busy }]">
        <ScanPane v-if="tab.compare.viewId" :key="tab.compare.viewId" :view-id="tab.compare.viewId" compare />
        <div v-else class="cmp-loading" role="status">
          <span class="spinner" aria-hidden="true" />
          <span>{{ tab.compare.message || t("正在对比…") }}</span>
          <span v-if="tab.compare.progress != null && tab.compare.phase === 'comparing'" class="mono muted">{{ Math.round(tab.compare.progress * 100) }}%</span>
        </div>
      </div>
      <BasketDrawer v-if="basketOpen" />
      <button v-else-if="hidden" class="inspector-open icon-btn" :aria-label="t('显示侧栏')" :title="t('显示侧栏')" @click="set({ inspectorHidden: false })">
        <Icon name="sidebar" />
      </button>
      <template v-else>
        <ColumnResizer />
        <CompareSidebar :tab="tab" />
      </template>
    </div>
  </div>
  <div v-else class="content column">
    <ScanBand v-if="running && scan" :scan="scan" :title="tab.title" />
    <div class="content-row" :style="columnWidth(width)">
      <div :class="['panes', { split }]">
        <ScanPane
          v-for="(v, i) in tab.viewIds"
          :key="v"
          :tab="tab"
          :index="i"
          :view-id="v"
          :split="split"
          :focused="i === tab.focusedPane"
          @focus="focusPane(tab.id, i)"
        />
      </div>
      <BasketDrawer v-if="basketOpen" />
      <button v-else-if="hidden" class="inspector-open icon-btn" :aria-label="t('显示侧栏')" :title="t('显示侧栏')" @click="set({ inspectorHidden: false })">
        <Icon name="sidebar" />
      </button>
      <template v-else>
        <ColumnResizer />
        <Results v-if="focused && filtering" :key="focused" :view-id="focused" />
        <ScanDetails v-else-if="running && scan && !selected" :scan="scan" />
        <Inspector v-else-if="focused" :view-id="focused" />
      </template>
    </div>
  </div>
</template>

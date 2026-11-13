<!-- Toolbar (UI-MAIN-10…26): navigation, breadcrumb / path edit, filter, visualization, detail,
     colour, switches, split, command palette, delete basket. Acts on the focused view (G7). In a
     narrow window it folds groups into the 「更多」 menu (lib/toolbarFit.ts, composables/useCollapseLevel). -->
<script setup lang="ts">
import { computed, ref } from "vue";

import ColorModes from "@/components/shell/ColorModes.vue";
import ComparePicker from "@/components/shell/ComparePicker.vue";
import CompareDirection from "@/components/shell/CompareDirection.vue";
import CompareExit from "@/components/shell/CompareExit.vue";
import HelpSearch from "@/components/pages/HelpSearch.vue";
import SettingsSearch from "@/components/pages/SettingsSearch.vue";
import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { useCollapseLevel } from "@/composables/useCollapseLevel";
import { OVERVIEW } from "@/help/docs";
import { helpTitle } from "@/help/links";
import { t } from "@/lib/i18n";
import { PAGES } from "@/lib/settingsPages";
import { CRUMBS_MIN, spaceSwitches } from "@/lib/toolbar";
import { collapse } from "@/lib/toolbarFit";
import { setViewSettings, settingsGo, settingsHistory, toggleSplit, viewCall } from "@/state/actions";
import { helpGo, helpHistory } from "@/state/help";
import { activeTab, basketCount, focusedViewId, set, useApp } from "@/state/store";
import { levelsThatFit } from "@/state/visuals";

import FilterBox from "./FilterBox.vue";
import ToolbarButton from "./ToolbarButton.vue";
import ToolbarCrumbs from "./ToolbarCrumbs.vue";
import ToolbarDetail from "./ToolbarDetail.vue";
import ToolbarMore from "./ToolbarMore.vue";
import ToolbarVisuals from "./ToolbarVisuals.vue";

const tab = useApp((s) => activeTab(s));
const viewId = useApp((s) => focusedViewId(s));
const view = useApp((s) => (viewId.value ? s.views[viewId.value] : undefined));
const paletteOpen = useApp((s) => s.paletteOpen);
const basket = useApp(basketCount);
const basketOpen = useApp((s) => s.basketOpen);
const st = computed(() => view.value?.state);
const scanTab = computed(() => tab.value.kind === "scan");
const settingsTab = computed(() => tab.value.kind === "settings");
const helpTab = computed(() => tab.value.kind === "help");
const settingsPage = useApp((s) => s.settingsPage);
const helpPage = useApp((s) => (helpTab.value ? helpTitle(s.lang, s.help.page) : ""));
const canBack = useApp((s) => (settingsTab.value ? s.settingsBack.length > 0 : helpTab.value ? s.helpBack.length > 0 : !!st.value?.can_back));
const canForward = useApp((s) => (settingsTab.value ? s.settingsForward.length > 0 : helpTab.value ? s.helpForward.length > 0 : !!st.value?.can_forward));
const back = () => (settingsTab.value ? settingsHistory(-1) : helpTab.value ? helpHistory(-1) : void viewCall(viewId.value, "view.back"));
const forward = () => (settingsTab.value ? settingsHistory(1) : helpTab.value ? helpHistory(1) : void viewCall(viewId.value, "view.forward"));
const cmp = computed(() => (scanTab.value ? tab.value.compare : undefined));
const split = computed(() => scanTab.value && !cmp.value && tab.value.viewIds.length > 1);
const lang = useApp((s) => s.lang);
const bar = ref<HTMLDivElement | null>(null);
// what decides the toolbar's width: when it changes, the toolbar works out again what fits
const shown = computed(() =>
  [
    lang.value,
    tab.value.id,
    tab.value.kind,
    cmp.value ? "compare" : "",
    split.value ? "split" : "",
    st.value?.has_trash ? "trash" : "",
    st.value?.hidden_folders ? "folders" : "",
    levelsThatFit(view.value?.info, st.value?.detail ?? 0) ?? "",
    (view.value?.info?.breadcrumb ?? []).map((c) => c.label).join("/"),
  ].join("|"),
);
const level = useCollapseLevel(bar, shown);
const fold = computed(() => collapse(level.value));
</script>

<template>
  <div ref="bar" class="toolbar" role="toolbar" :aria-label="t('视图工具栏')">
    <nav class="nav" :aria-label="t('导航')">
      <ToolbarButton icon="back" :label="t('后退')" keys="Backspace" :disabled="!canBack" @click="back" />
      <ToolbarButton icon="forward" :label="t('前进')" keys="Alt+→" :disabled="!canForward" @click="forward" />
      <ToolbarButton icon="up" :label="t('上一级')" keys="Alt+↑" :disabled="!st?.can_up" @click="viewCall(viewId, 'view.up')" />
      <ToolbarButton icon="home" :label="t('根')" keys="Ctrl+Home" :disabled="!st?.can_up" @click="viewCall(viewId, 'view.home')" />
    </nav>
    <div class="vsep" />
    <nav v-if="settingsTab" class="crumbs" :aria-label="t('当前位置')" :data-fit-min="CRUMBS_MIN">
      <span class="crumb-seg">
        <button class="crumb" @click="settingsGo('general')">
          <Icon name="settings" />
          {{ t("设置") }}
        </button>
      </span>
      <span class="crumb-seg">
        <span class="crumb-arrow"><Icon name="chevronRight" :size="12" /></span>
        <span class="crumb-cur" aria-current="page">{{ PAGES.find((p) => p.id === settingsPage)?.name }}</span>
      </span>
    </nav>
    <nav v-else-if="helpTab" class="crumbs" :aria-label="t('当前位置')" :data-fit-min="CRUMBS_MIN">
      <span class="crumb-seg">
        <button class="crumb" @click="helpGo(OVERVIEW)">
          <Icon name="info" />
          {{ t("帮助") }}
        </button>
      </span>
      <span class="crumb-seg">
        <span class="crumb-arrow"><Icon name="chevronRight" :size="12" /></span>
        <span class="crumb-cur" aria-current="page">{{ helpPage }}</span>
      </span>
    </nav>
    <ComparePicker v-else-if="cmp" :tab="tab" :compact="fold.compact" />
    <ToolbarCrumbs v-else-if="scanTab && !split" :view-id="viewId" />
    <nav v-else-if="!scanTab" class="crumbs" :aria-label="t('当前位置')" :data-fit-min="CRUMBS_MIN">
      <span class="crumb-cur">
        <Icon :name="tab.kind === 'machine' ? 'diskGauge' : 'drive'" />
        {{ tab.kind === "machine" ? t("本机磁盘") : t("新扫描") }}
      </span>
    </nav>
    <span v-else class="flexless" />
    <SettingsSearch v-if="settingsTab" />
    <HelpSearch v-if="helpTab" />
    <FilterBox v-if="!split && !settingsTab && !helpTab" :view-id="scanTab ? viewId : null" :disabled="!scanTab" />
    <div class="toolbar-right">
      <template v-if="cmp">
        <template v-if="st && viewId">
          <ToolbarVisuals v-if="!fold.visualInMenu" :view-id="viewId" :st="st" />
          <ToolbarDetail :view-id="viewId" :st="st" :compact="fold.compact" />
        </template>
        <CompareDirection v-if="viewId && !fold.coloringInMenu" :view-id="viewId" />
        <ToolbarMore v-if="st && viewId" :view-id="viewId" :st="st" :fold="fold" compare />
        <CompareExit :tab="tab" :compact="fold.compact" />
        <div class="vsep wide" />
        <ToolbarButton icon="split" :label="t('对比模式下不能分屏')" disabled />
      </template>
      <template v-if="scanTab && st && viewId && !cmp">
        <ToolbarVisuals v-if="!fold.visualInMenu" :view-id="viewId" :st="st" />
        <ToolbarDetail :view-id="viewId" :st="st" :compact="fold.compact" />
        <ColorModes v-if="!fold.coloringInMenu" :view-id="viewId" :st="st" />
        <!-- the recycle bin's only when the scan holds one (and not in a comparison) -->
        <template v-if="!fold.chipsInMenu">
          <button
            v-for="s in spaceSwitches(st)"
            :key="s.id"
            :class="['chip', { on: s.on(st), 'swatch-only': fold.chipSwatches }]"
            :aria-pressed="s.on(st)"
            :aria-label="s.label()"
            :title="s.title(st)"
            @click="setViewSettings(viewId, s.flip(st))"
          >
            <Icon v-if="s.icon" class="chip-icon" :name="s.icon" :size="12" />
            <span v-else :class="['chip-swatch', s.id]" />
            <template v-if="!fold.chipSwatches">{{ s.label() }}</template>
          </button>
        </template>
        <ToolbarMore :view-id="viewId" :st="st" :fold="fold" :compare="false" />
        <div class="vsep wide" />
        <ToolbarButton icon="split" :label="t('分屏')" :pressed="split" @click="toggleSplit()" />
      </template>
      <button
        :class="['palette-btn', { on: paletteOpen, compact: fold.compact }]"
        :aria-label="t('命令面板（Ctrl+K）')"
        :title="t('命令面板（Ctrl+K）')"
        :aria-expanded="paletteOpen"
        @click="set({ paletteOpen: !paletteOpen })"
      >
        <Icon name="command" />
        <Kbd v-if="!fold.compact">Ctrl+K</Kbd>
      </button>
      <button
        v-if="!settingsTab && !helpTab"
        :class="['icon-btn basket', { on: basketOpen }]"
        :aria-label="t('删除篮，{n} 项', { n: basket })"
        :aria-pressed="basketOpen"
        :title="t('删除篮')"
        @click="set({ basketOpen: !basketOpen })"
      >
        <Icon name="basket" />
        <span v-if="basket > 0" class="badge-count">{{ basket > 99 ? "99+" : basket }}</span>
      </button>
    </div>
  </div>
</template>

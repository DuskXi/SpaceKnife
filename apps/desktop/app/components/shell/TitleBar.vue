<!-- Title bar (UI-MAIN-02…09): brand, tabs, new tab, update badge, settings, window buttons; the empty
     space drags the window (data-tauri-drag-region; a double click there maximizes, Tauri's own). In a
     browser tab (headless mode) no window buttons: the computer's chip instead. -->
<script setup lang="ts">
import { onScopeDispose, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import { activate, closeTab, moveTab, newStartTab, openMachineTab, openSettingsTab, restoreTabOrder } from "@/state/actions";
import { openHelp } from "@/state/help";
import { type Tab, useApp } from "@/state/store";
import { dropIndex } from "@/state/tabOrder";

import RemoteChip from "./RemoteChip.vue";
import UpdateBadge from "./UpdateBadge.vue";

const sk = window.sk;
const tabs = useApp((s) => s.tabs);
const active = useApp((s) => s.active);
const scans = useApp((s) => s.scans);
const native = sk.host === "app";
const mac = native && sk.platform === "darwin"; // the traffic lights' room
const maximized = ref(false);
void sk.window.isMaximized().then((m) => (maximized.value = m));
onScopeDispose(sk.window.onMaximized((m) => (maximized.value = m)));

/** What the tab says after its title: the scan's state (a stopped or failed scan keeps its partial
 * results, W2 待定 16), and its tooltip. */
function tabState(tab: Tab): { suffix: string; title: string } {
  const status = tab.scanId ? scans.value[tab.scanId] : undefined;
  const scanning = status?.state === "scanning" || status?.state === "queued";
  const state = scanning ? (status?.paused ? t("已暂停") : t("扫描中")) : status?.state === "cancelled" ? t("未完成") : status?.state === "failed" ? t("扫描失败") : "";
  const suffix = state ? ` · ${state}` : "";
  const vol = status?.roots[0]?.volume;
  const hint = vol ? `${vol.fs_type} · ${fmtSize(vol.total, true)}` : tab.title;
  return { suffix, title: `${tab.title}${suffix}\n${hint}\n${t("拖动可调整顺序")}` };
}

const tabIcon = (tab: Tab): IconName => (tab.kind === "settings" ? "settings" : tab.kind === "help" ? "info" : tab.kind === "machine" ? "diskGauge" : "drive");

// UI-MAIN-04: drag a tab sideways to move it; it takes its neighbour's slot once the pointer crosses
// that tab's middle. Esc puts every tab back where the drag started.
const row = ref<HTMLDivElement | null>(null);
let drag: { id: string; x: number; order: string[]; moved: boolean } | null = null;
const dragging = ref<string | null>(null);

function onEscape(e: KeyboardEvent): void {
  if (e.key !== "Escape" || !drag) return;
  e.preventDefault();
  e.stopPropagation();
  restoreTabOrder(drag.order);
  drag = null;
  dragging.value = null;
}
watch(dragging, (now) => {
  if (now) window.addEventListener("keydown", onEscape, true);
  else window.removeEventListener("keydown", onEscape, true);
});
onScopeDispose(() => window.removeEventListener("keydown", onEscape, true));

const boxes = () =>
  [...(row.value?.querySelectorAll<HTMLElement>("[data-tab-id]") ?? [])].map((el) => {
    const r = el.getBoundingClientRect();
    return { id: el.dataset.tabId!, left: r.left, width: r.width };
  });

function onPointerDown(tab: Tab, e: PointerEvent): void {
  if (e.button === 1) return void closeTab(tab.id);
  if (e.button !== 0) return;
  activate(tab.id);
  drag = { id: tab.id, x: e.clientX, order: tabs.value.map((x) => x.id), moved: false };
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
}

function onPointerMove(tab: Tab, e: PointerEvent): void {
  const d = drag;
  if (!d || d.id !== tab.id) return;
  if (!d.moved) {
    if (Math.abs(e.clientX - d.x) < 5) return; // a click, not a drag
    d.moved = true;
    dragging.value = tab.id;
  }
  const list = boxes();
  const to = dropIndex(list, tab.id, e.clientX);
  if (list.findIndex((b) => b.id === tab.id) !== to) moveTab(tab.id, to);
}

function endDrag(): void {
  drag = null;
  dragging.value = null;
}
</script>

<template>
  <header :class="['titlebar', { mac }]" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <span class="logo"><Icon name="logo" /></span>
      <span data-tauri-drag-region>SpaceKnife</span>
    </div>
    <div ref="row" :class="['tabs', { reordering: dragging }]" role="tablist" :aria-label="t('扫描视图')">
      <div
        v-for="tab in tabs"
        :key="tab.id"
        role="tab"
        :aria-selected="tab.id === active"
        :tabindex="tab.id === active ? 0 : -1"
        :data-tab-id="tab.id"
        :class="['tab', { on: tab.id === active, dragging: dragging === tab.id }]"
        :title="dragging ? undefined : tabState(tab).title"
        @pointerdown="onPointerDown(tab, $event)"
        @pointermove="onPointerMove(tab, $event)"
        @pointerup="endDrag"
        @pointercancel="endDrag"
        @lostpointercapture="endDrag"
      >
        <Icon :name="tabIcon(tab)" :size="14" />
        <span class="tab-title">
          {{ tab.title }}
          <span v-if="tabState(tab).suffix" class="tab-suffix">{{ tabState(tab).suffix }}</span>
        </span>
        <button
          class="tab-close"
          :aria-label="t('关闭标签 {title}（Ctrl+W）', { title: tab.title })"
          :title="t('关闭标签（Ctrl+W）')"
          @pointerdown.stop
          @mousedown.stop
          @click="closeTab(tab.id)"
        >
          <Icon name="close" :size="12" />
        </button>
      </div>
      <button class="tab-new" :aria-label="t('新建标签（Ctrl+T）')" :title="t('新建标签（Ctrl+T）')" @click="newStartTab()">
        <Icon name="plus" />
      </button>
    </div>
    <div class="drag" data-tauri-drag-region />
    <RemoteChip v-if="!native" />
    <UpdateBadge />
    <button class="title-btn" :aria-label="t('本机磁盘')" :title="t('本机磁盘：每块盘的容量、链路、读写量与健康')" @click="openMachineTab()">
      <Icon name="diskGauge" />
    </button>
    <button class="title-btn" :aria-label="t('帮助（F1）')" :title="t('帮助（F1）')" @click="openHelp()">
      <Icon name="info" />
    </button>
    <button class="title-btn" :aria-label="t('设置（Ctrl+,）')" :title="t('设置（Ctrl+,）')" @click="openSettingsTab()">
      <Icon name="settings" />
    </button>
    <div v-if="native && !mac" class="window-btns">
      <button :aria-label="t('最小化')" :title="t('最小化')" @click="sk.window.minimize()">
        <Icon name="windowMinimize" :stroke-width="1.25" />
      </button>
      <button :aria-label="maximized ? t('还原') : t('最大化')" :title="maximized ? t('还原') : t('最大化')" @click="sk.window.toggleMaximize()">
        <Icon name="windowMaximize" :stroke-width="1.25" />
      </button>
      <button class="close" :aria-label="t('关闭')" :title="t('关闭')" @click="sk.window.close()">
        <Icon name="windowClose" :stroke-width="1.25" />
      </button>
    </div>
  </header>
</template>

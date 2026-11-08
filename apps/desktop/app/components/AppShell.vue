<!-- The window's layout (App.tsx in the Electron renderer): title bar, toolbar, the active tab's page,
     log panel, status bar, and the layers above them (palette, menus, dialogs). Boots the connection
     to the backend and listens for the global shortcuts (state/shortcuts.ts). -->
<script setup lang="ts">
import { computed, onMounted, onScopeDispose } from "vue";

import HelpPage from "@/components/pages/HelpPage.vue";
import MachinePage from "@/components/pages/MachinePage.vue";
import ScanPage from "@/components/pages/ScanPage.vue";
import SettingsPage from "@/components/pages/SettingsPage.vue";
import StartPage from "@/components/pages/StartPage.vue";
import BrowserLayer from "@/components/shell/BrowserLayer.vue";
import ContextMenu from "@/components/shell/ContextMenu.vue";
import ExportDialog from "@/components/shell/ExportDialog.vue";
import FullDiskAccessDialog from "@/components/shell/FullDiskAccessDialog.vue";
import LogPanel from "@/components/shell/LogPanel.vue";
import OpToast from "@/components/shell/OpToast.vue";
import Palette from "@/components/shell/Palette.vue";
import StatusBar from "@/components/shell/StatusBar.vue";
import TitleBar from "@/components/shell/TitleBar.vue";
import Toolbar from "@/components/shell/Toolbar.vue";
import UpdatePrompt from "@/components/shell/UpdatePrompt.vue";
import { inBrowser } from "@/lib/host";
import { t } from "@/lib/i18n";
import { boot } from "@/state/actions";
import { onKey } from "@/state/shortcuts";
import { activeTab, useApp } from "@/state/store";
import { visibleSurface } from "@/state/updates";

const tab = useApp((s) => activeTab(s));
const conn = useApp((s) => s.conn);
const message = useApp((s) => s.connMessage);
const toastText = useApp((s) => s.toast);
const paletteOpen = useApp((s) => s.paletteOpen);
const updateDialog = useApp((s) => visibleSurface(s.update, s.updateOpen) === "dialog");
// a modal layer above the window: nothing behind it takes the focus or the pointer
const inert = computed(() => paletteOpen.value || updateDialog.value);

onMounted(() => {
  void boot();
  window.addEventListener("keydown", onKey);
});
onScopeDispose(() => window.removeEventListener("keydown", onKey));
</script>

<template>
  <div class="app" :inert="inert || undefined">
    <TitleBar />
    <Toolbar />
    <ScanPage v-if="tab.kind === 'scan'" :tab="tab" />
    <SettingsPage v-else-if="tab.kind === 'settings'" />
    <HelpPage v-else-if="tab.kind === 'help'" />
    <MachinePage v-else-if="tab.kind === 'machine'" />
    <StartPage v-else :tab="tab" />
    <LogPanel />
    <StatusBar />
    <div v-if="conn === 'failed'" class="fatal">
      <h2>{{ inBrowser() ? t("连不上 SpaceKnife") : t("后端没有启动") }}</h2>
      <p class="mono">{{ message }}</p>
    </div>
    <div v-if="toastText" class="toast" role="status">{{ toastText }}</div>
  </div>
  <Palette />
  <ContextMenu />
  <OpToast />
  <ExportDialog />
  <FullDiskAccessDialog />
  <UpdatePrompt />
  <BrowserLayer />
</template>

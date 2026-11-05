<!-- The window's layout (App.tsx in the Electron renderer): title bar, toolbar, the active tab's page,
     log panel, status bar, and the layers above them (palette, menus, dialogs). Boots the connection
     to the backend and listens for the global shortcuts (state/shortcuts.ts).

     Phase 1 of the port: only the title bar is a component yet. Every other slot names the component
     that goes there (PORTING.md); until it exists, a placeholder keeps the grid's rows. -->
<script setup lang="ts">
import { computed, onMounted, onScopeDispose } from "vue";

import TitleBar from "@/components/shell/TitleBar.vue";
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
    <!-- TODO(port): <Toolbar /> (shell/Toolbar.tsx) -->
    <div class="toolbar" />
    <!-- TODO(port): ScanPage / SettingsPage / HelpPage / MachinePage / StartPage by tab.kind (pages/*.tsx) -->
    <main class="port-placeholder" :data-tab-kind="tab.kind">
      <p class="muted">{{ tab.title }}</p>
    </main>
    <!-- TODO(port): <LogPanel /> (shell/LogPanel.tsx) -->
    <div />
    <!-- TODO(port): <StatusBar /> (shell/StatusBar.tsx) -->
    <footer class="statusbar" />
    <div v-if="conn === 'failed'" class="fatal">
      <h2>{{ inBrowser() ? t("连不上 SpaceKnife") : t("后端没有启动") }}</h2>
      <p class="mono">{{ message }}</p>
    </div>
    <div v-if="toastText" class="toast" role="status">{{ toastText }}</div>
  </div>
  <!-- TODO(port): <Palette /> <ContextMenu /> <OpToast /> <ExportDialog /> <FullDiskAccessDialog />
       <UpdatePrompt /> <BrowserLayer /> -->
</template>

<style scoped>
.port-placeholder {
  display: grid;
  place-content: center;
}
</style>

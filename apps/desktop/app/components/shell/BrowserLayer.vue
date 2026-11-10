<!-- Browser mode (headless): what a browser tab adds to the interface. The path dialog (a browser can't
     show the server's folders, shim.ts asks here), a question before the tab closes while a scan or a
     deletion runs or scan tabs are open (Ctrl+W closes the browser's tab, not SpaceKnife's), and the
     tab's title naming the computer. The title bar's chip is RemoteChip.vue. -->
<script setup lang="ts">
import { nextTick, onScopeDispose, ref, shallowRef, watch } from "vue";

import { inBrowser, serverName } from "@/lib/host";
import { t } from "@/lib/i18n";
import { answerPath, onPathChange, shownPath } from "@/state/browser";
import { useApp } from "@/state/store";

const browser = inBrowser();
const busy = useApp(
  (s) => Object.values(s.scans).some((x) => x.state === "scanning" || x.state === "queued") || !!(s.op && (s.op.phase === "running" || s.op.phase === "waiting")),
);
const scanTabs = useApp((s) => s.tabs.some((tab) => tab.kind === "scan"));

if (browser) {
  const name = serverName();
  document.documentElement.classList.add("sk-browser");
  document.title = name ? `SpaceKnife · ${name}` : "SpaceKnife";
  watch(
    [busy, scanTabs],
    ([b, s], _, onCleanup) => {
      if (!b && !s) return;
      const guard = (e: BeforeUnloadEvent) => {
        e.preventDefault();
        e.returnValue = ""; // older browsers ask only with this set
      };
      window.addEventListener("beforeunload", guard);
      onCleanup(() => window.removeEventListener("beforeunload", guard));
    },
    { immediate: true },
  );
}

// the path dialog
const request = shallowRef(shownPath());
onScopeDispose(onPathChange(() => (request.value = shownPath())));
const value = ref("");
const input = ref<HTMLInputElement | null>(null);
watch(
  request,
  (r) => {
    if (!r) return;
    value.value = r.value;
    void nextTick(() =>
      requestAnimationFrame(() => {
        const el = input.value;
        if (!el) return;
        el.focus();
        el.setSelectionRange(el.value.length, el.value.length);
      }),
    );
  },
  { immediate: true },
);
</script>

<template>
  <div v-if="browser && request" class="modal-back" role="presentation" @mousedown="answerPath(null)">
    <form class="modal path-dialog" role="dialog" aria-modal="true" aria-labelledby="path-dialog-title" @mousedown.stop @submit.prevent="answerPath(value)">
      <h3 id="path-dialog-title">{{ request.title }}</h3>
      <p class="muted small">{{ request.hint }}</p>
      <!-- typing is not a shortcut -->
      <input
        ref="input"
        v-model="value"
        class="path-dialog-input mono"
        spellcheck="false"
        autocomplete="off"
        :aria-label="request.hint"
        @keydown.stop="$event.key === 'Escape' && answerPath(null)"
      />
      <div class="modal-buttons">
        <button type="button" class="ghost" @click="answerPath(null)">{{ t("取消") }}</button>
        <button type="submit" class="primary" :disabled="!value.trim()">{{ t("确定") }}</button>
      </div>
    </form>
  </div>
</template>

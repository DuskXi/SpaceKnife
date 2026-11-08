<!-- The versions of the parts, as a settings group; one button copies them all. The shell is Tauri or
     Electron, with its web view (window.sk.versions); a browser tab has neither. -->
<script setup lang="ts">
import { computed, shallowRef, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import { PROTOCOL_VERSION, type SystemInfo } from "@/protocol/generated";
import { call } from "@/state/actions";
import { toast, useApp } from "@/state/store";
import { installKindName } from "@/state/updates";

const conn = useApp((s) => s.conn);
const kind = useApp((s) => s.update?.install_kind ?? "unknown");
const sys = shallowRef<SystemInfo | null>(null);
watch(
  conn,
  (c, _, onCleanup) => {
    if (c !== "ready") return;
    let alive = true;
    call("system.info", {})
      .then((r) => alive && (sys.value = r))
      .catch(() => {});
    onCleanup(() => (alive = false));
  },
  { immediate: true },
);

const rows = computed((): [string, string][] => {
  const v = window.sk.versions;
  const s = sys.value;
  return [
    ["SpaceKnife", window.sk.version],
    [t("安装方式"), installKindName(kind.value)],
    [v.runtime || t("外壳"), v.runtimeVersion || "—"],
    ["WebView", v.webview || "—"],
    ["Rust", s?.server.rust ?? "—"],
    [t("协议"), PROTOCOL_VERSION],
    [t("系统"), s ? `${s.platform.system} ${s.platform.release} (${s.platform.machine})` : "—"],
  ];
});

function copy(): void {
  window.sk.shell.copyText(rows.value.map(([k, val]) => `${k}: ${val}`).join("\n"));
  toast(t("已复制版本信息"));
}
</script>

<template>
  <section class="set-group" :aria-label="t('组件版本')">
    <div class="set-group-head">
      <h2>{{ t("组件版本") }}</h2>
      <button class="link small" @click="copy">
        <Icon name="copy" :size="12" />
        {{ t("复制版本信息") }}
      </button>
    </div>
    <div class="set-card about-versions">
      <div v-for="[k, val] in rows" :key="k" class="about-version-row">
        <span class="muted">{{ k }}</span>
        <span class="mono">{{ val }}</span>
      </div>
    </div>
  </section>
</template>

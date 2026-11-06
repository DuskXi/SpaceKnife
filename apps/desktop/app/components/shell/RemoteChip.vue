<!-- The title bar's chip in a browser tab (headless mode): SpaceKnife on which computer, and whether
     the connection is encrypted. Nothing in the app's own window (no window.sk.remote). -->
<script setup lang="ts">
import { computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { isLoopback } from "@/lib/host";
import { t } from "@/lib/i18n";

const remote = window.sk.remote;
// plain HTTP matters once it leaves this computer, not through localhost
const plain = !!remote && !remote.secure && !isLoopback(location.hostname);
const tip = computed(() =>
  remote
    ? [
        t("这是 {host} 上的 SpaceKnife：扫描、移到回收站和删除，都作用于那台电脑上的文件", { host: remote.hostname }),
        remote.secure ? t("连接经 HTTPS 加密") : plain ? t("明文 HTTP：同一网络上的人能看到传输的内容") : "",
      ]
        .filter(Boolean)
        .join("\n")
    : "",
);
</script>

<template>
  <span v-if="remote" :class="['remote-chip', { plain }]" :title="tip" :aria-label="tip">
    <Icon name="globe" :size="13" />
    <span class="remote-name">{{ remote.hostname }}</span>
    <span v-if="plain" class="remote-plain">HTTP</span>
  </span>
</template>

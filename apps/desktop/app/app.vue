<!-- Nuxt's root. The interface loads after the plugins put window.sk in place (plugins/01.host.client.ts):
     modules read window.sk.platform when they load, so the shell is imported only now. A rendering
     error shows a recoverable message (ui/ErrorBoundary.vue) instead of a blank window. -->
<script setup lang="ts">
import { defineAsyncComponent } from "vue";

import ErrorBoundary from "@/components/ui/ErrorBoundary.vue";

const AppShell = defineAsyncComponent(() => import("@/components/AppShell.vue"));
</script>

<template>
  <ErrorBoundary>
    <AppShell />
  </ErrorBoundary>
</template>

<!-- Nuxt's root. The interface loads after the plugins put window.sk in place (plugins/01.host.client.ts):
     modules read window.sk.platform when they load, so the shell is imported only now. -->
<script setup lang="ts">
import { defineAsyncComponent } from "vue";

const AppShell = defineAsyncComponent(() => import("@/components/AppShell.vue"));
</script>

<template>
  <AppShell />
</template>

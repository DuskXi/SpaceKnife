<!-- A group of settings rows under its heading (the backend's group and note). -->
<script setup lang="ts">
import SettingRow from "@/components/pages/SettingRow.vue";
import type { SettingMeta } from "@/protocol/generated";

defineProps<{ title: string; note: string; rows: SettingMeta[]; query?: string }>();
</script>

<template>
  <section class="set-group" :aria-label="title">
    <div class="set-group-head">
      <h2>{{ title }}</h2>
      <span v-if="note" class="muted small">{{ note }}</span>
    </div>
    <div class="set-card">
      <SettingRow v-for="m in rows" :key="m.key" :meta="m" :query="query" />
    </div>
  </section>
</template>

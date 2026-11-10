<!-- A danger button that asks once more in place. -->
<script setup lang="ts">
import { ref } from "vue";

import { t } from "@/lib/i18n";

defineProps<{ label: string; ask: string; disabled?: boolean }>();
const emit = defineEmits<{ yes: [] }>();
const asking = ref(false);

function yes(): void {
  asking.value = false;
  emit("yes");
}
</script>

<template>
  <button v-if="!asking" class="ghost small" :disabled="disabled" @click="asking = true">{{ label }}</button>
  <span v-else class="class-confirm">
    <span class="small">{{ ask }}</span>
    <button class="danger-btn small" @click="yes">{{ t("确定") }}</button>
    <button class="ghost small" @click="asking = false">{{ t("取消") }}</button>
  </span>
</template>

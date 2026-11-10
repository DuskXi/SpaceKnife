<!-- Last line of defence: a rendering error below shows a recoverable message instead of a blank window. -->
<script setup lang="ts">
import { onErrorCaptured, shallowRef } from "vue";

import { t } from "@/lib/i18n";

const error = shallowRef<Error | null>(null);

onErrorCaptured((err, _instance, info) => {
  error.value = err instanceof Error ? err : new Error(String(err));
  console.error("UI error", err, info);
  return false; // handled here: not on to the app's error handler
});
const reload = () => location.reload();
</script>

<template>
  <slot v-if="!error" />
  <div v-else class="fatal" role="alert">
    <h2>{{ t("界面出错了") }}</h2>
    <p class="mono">{{ error.message }}</p>
    <div class="fatal-actions">
      <button class="ghost" @click="error = null">{{ t("重试") }}</button>
      <button class="primary" @click="reload">{{ t("重新加载界面") }}</button>
    </div>
  </div>
</template>

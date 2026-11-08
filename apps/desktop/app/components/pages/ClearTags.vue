<!-- 「标签」: forget every remembered tag (UI-W7-34). -->
<script setup lang="ts">
import ConfirmButton from "@/components/pages/ConfirmButton.vue";
import SetRow from "@/components/pages/SetRow.vue";
import { useMaintenance } from "@/composables/useMaintenance";
import { fmtInt } from "@/lib/format";
import { t } from "@/lib/i18n";
import { call } from "@/state/actions";

const [info, refresh] = useMaintenance();
</script>

<template>
  <section class="set-group" :aria-label="t('已保存的标签')">
    <div class="set-group-head">
      <h2>{{ t("已保存的标签") }}</h2>
    </div>
    <div class="set-card">
      <SetRow :name="t('清除所有已保存的标签')" :desc="info ? t('{n} 个扫描位置有已保存的标签', { n: fmtInt(info.saved_tags) }) : '…'">
        <ConfirmButton :label="t('清除…')" :ask="t('所有扫描位置的标签都会被清除，确定？')" :disabled="!info?.saved_tags" @yes="refresh(call('tags.clear_saved', {}))" />
      </SetRow>
    </div>
  </section>
</template>

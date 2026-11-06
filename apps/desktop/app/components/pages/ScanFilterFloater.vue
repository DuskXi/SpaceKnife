<!-- W3 floater over the picture: match count and size, dim / hide switch (UI-W3-24, UI-W3-25;
     FilterFloater in pages/ScanPage.tsx). -->
<script setup lang="ts">
import Icon from "@/components/ui/Icon.vue";
import { fmtInt, fmtShare, fmtSize } from "@/lib/format";
import { splitPlaceholders } from "@/lib/hoverCard";
import { t } from "@/lib/i18n";
import { clearFilter, setViewSettings } from "@/state/actions";
import { useApp } from "@/state/store";

const props = defineProps<{ viewId: string }>();
const filter = useApp((s) => s.views[props.viewId]?.filter ?? null);
const mode = useApp((s) => s.views[props.viewId]?.state.filter_mode ?? "dim");
const scanned = useApp((s) => s.views[props.viewId]?.info?.scanned ?? 0);
const MODES = ["dim", "hide"] as const;
</script>

<template>
  <div v-if="filter?.active" class="floater" role="group" :aria-label="t('过滤结果')">
    <span class="floater-stat" role="status" aria-live="polite"
      ><template v-for="(part, k) in splitPlaceholders(t('匹配 {n} 项 · {size}'))" :key="k"
        ><template v-if="!part.slot">{{ part.text }}</template
        ><span v-else-if="part.text === 'n'" class="mono strong">{{ fmtInt(filter.files) }}</span
        ><span v-else-if="part.text === 'size'" class="mono strong">{{ fmtSize(filter.alloc) }}</span></template
      ><span class="muted"> · {{ t("占 {share}", { share: fmtShare(filter.alloc, scanned) }) }}</span
      ><span v-if="filter.stale" class="warn"> · {{ t("表达式有误，显示的是上一次的结果") }}</span></span
    >
    <div class="segmented small" role="radiogroup" :aria-label="t('不匹配项的显示方式')">
      <button v-for="m in MODES" :key="m" role="radio" :aria-checked="mode === m" :class="mode === m ? 'on' : ''" @click="setViewSettings(viewId, { filter_mode: m })">
        {{ m === "dim" ? t("暗化不匹配") : t("隐藏不匹配") }}
      </button>
    </div>
    <button class="icon-btn small" :aria-label="t('清除过滤')" :title="t('清除过滤（Esc）')" @click="clearFilter(viewId)">
      <Icon name="close" :size="14" />
    </button>
  </div>
</template>

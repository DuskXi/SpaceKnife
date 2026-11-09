<!-- An icon button of the toolbar (Toolbar.vue): the label, with its keys, is the tooltip too. -->
<script setup lang="ts">
import { computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";

const props = defineProps<{ icon: IconName; label: string; keys?: string; disabled?: boolean; pressed?: boolean }>();
defineEmits<{ click: [] }>();
const title = computed(() => (props.keys ? t("{label}（{keys}）", { label: props.label, keys: props.keys }) : props.label));
</script>

<template>
  <button :class="['icon-btn', { on: pressed }]" :aria-label="title" :title="title" :aria-pressed="pressed" :disabled="disabled" @click="$emit('click')">
    <Icon :name="icon" />
  </button>
</template>

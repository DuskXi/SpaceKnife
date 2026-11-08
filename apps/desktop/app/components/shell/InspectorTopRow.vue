<!-- One of the largest files in the details panel (TopRow in shell/Inspector.tsx); a click locates it.
     `cls` comes from the backend: by extension, and by size for ts / mts. -->
<script setup lang="ts">
import { fmtPct, fmtSize } from "@/lib/format";
import { classHex } from "@/lib/classes";
import { t } from "@/lib/i18n";
import type { NodeInfo } from "@/protocol/generated";
import { reveal } from "@/state/reveal";
import { useApp } from "@/state/store";

defineProps<{ node: NodeInfo; rel: string; tag: string | null; cls: string; whole: number; viewId: string }>();
// the class colour follows the vocabulary and the theme
const vocab = useApp((s) => s.vocab);
const hex = (id: string) => classHex(vocab.value, id);
</script>

<template>
  <button class="top-row" @click="reveal(viewId, node.id)">
    <span class="top-line">
      <span class="swatch" :style="{ background: hex(cls) }" />
      <span class="top-name">{{ node.name }}</span>
      <span v-if="tag" :class="['tag-mini', tag]" role="img" :aria-label="t('{tag} 标签', { tag })" />
      <span class="flex" />
      <span class="mono">{{ fmtSize(node.alloc) }}</span>
    </span>
    <span class="top-sub mono">
      <span>{{ rel }}</span>
      <span>{{ fmtPct(node.alloc, whole) }}</span>
    </span>
    <span class="top-bar">
      <span :style="{ width: `${whole ? (node.alloc / whole) * 100 : 0}%`, background: hex(cls) }" />
    </span>
  </button>
</template>

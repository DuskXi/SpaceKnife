<!-- Command palette (W4, UI-W4-01…49): mounted while `paletteOpen`, so each opening starts afresh and
     closing gives the focus back (PaletteDialog.vue). -->
<script setup lang="ts">
import { useApp } from "@/state/store";

import PaletteDialog from "./PaletteDialog.vue";

const open = useApp((s) => s.paletteOpen);
</script>

<template>
  <PaletteDialog v-if="open" />
</template>

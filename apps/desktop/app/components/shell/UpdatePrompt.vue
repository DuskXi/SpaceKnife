<!-- The update card that opens by itself: at the top left for a minor release, as a dialog for a
     major one (state/updates.ts visibleSurface). -->
<script setup lang="ts">
import { useApp } from "@/state/store";
import { hasUpdate, visibleSurface } from "@/state/updates";

import UpdateCard from "./UpdateCard.vue";

const status = useApp((s) => s.update);
const surface = useApp((s) => visibleSurface(s.update, s.updateOpen));
</script>

<template>
  <template v-if="hasUpdate(status)">
    <UpdateCard v-if="surface === 'corner'" :status="status" placement="corner" />
    <!-- no closing by a click beside it: it may open while the user is clicking around -->
    <div v-else-if="surface === 'dialog'" class="modal-back" role="presentation">
      <UpdateCard :status="status" placement="dialog" />
    </div>
  </template>
</template>

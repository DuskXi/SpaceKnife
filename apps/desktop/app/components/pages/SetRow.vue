<!-- A plain settings row: name, description and the control in the slot (高级, 标签, 文件与诊断). -->
<script setup lang="ts">
defineProps<{ name: string; desc?: string }>();
</script>

<template>
  <div class="set-row">
    <div class="set-text">
      <div class="set-name">{{ name }}</div>
      <div v-if="desc" class="set-desc">{{ desc }}</div>
    </div>
    <div class="set-control"><slot /></div>
  </div>
</template>

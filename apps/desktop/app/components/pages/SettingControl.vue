<!-- A setting's control, as the backend's metadata says: a switch, segmented buttons, colour swatches,
     a slider (saved when let go), a number box or a text box (saved on Enter or leaving it). -->
<script setup lang="ts">
import { ref, watch } from "vue";

import { t } from "@/lib/i18n";
import type { SettingValue } from "@/lib/settingsPages";
import { accentOf } from "@/lib/theme";
import type { SettingMeta } from "@/protocol/generated";

const props = defineProps<{ meta: SettingMeta; value: SettingValue | undefined; disabled: boolean; describedBy: string }>();
const emit = defineEmits<{ save: [value: SettingValue] }>();

// the slider's value while it moves, the number / text box's while typing; the stored value wins
// whenever it changes
const live = ref(Number(props.value ?? 0));
const text = ref(String(props.value ?? (props.meta.control === "number" ? 0 : "")));
const error = ref("");
watch(
  () => props.value,
  (v) => {
    live.value = Number(v ?? 0);
    text.value = String(v ?? (props.meta.control === "number" ? 0 : ""));
  },
);

function commitSlider(): void {
  if (live.value !== Number(props.value ?? 0)) emit("save", live.value);
}

function commitNumber(): void {
  const value = Number(props.value ?? 0);
  const n = Number(text.value.trim());
  const lo = props.meta.min ?? -Infinity;
  const hi = props.meta.max ?? Infinity;
  if (!Number.isInteger(n) || n < lo || n > hi) {
    error.value = t("请输入 {lo}–{hi} 的整数", { lo, hi });
    text.value = String(value);
    return;
  }
  error.value = "";
  if (n !== value) emit("save", n);
}

function commitText(): void {
  if (text.value.trim() !== String(props.value ?? "")) emit("save", text.value.trim());
}

function onBoxKey(e: KeyboardEvent, commit: () => void): void {
  e.stopPropagation();
  if (e.key === "Enter") commit();
}

// an accent preset as it is in the theme on screen (the setting keeps the dark one's hex)
const swatch = (v: SettingValue) => (props.meta.key === "appearance.accent" ? accentOf(String(v)) : String(v));
</script>

<template>
  <button
    v-if="meta.control === 'switch'"
    role="switch"
    :aria-checked="Boolean(value)"
    :aria-label="meta.name"
    :aria-describedby="describedBy"
    :class="['switch', { on: value }]"
    :disabled="disabled"
    :title="disabled ? meta.reason : undefined"
    @click="emit('save', !value)"
  >
    <span />
  </button>
  <div v-else-if="meta.control === 'segmented'" class="segmented small" role="radiogroup" :aria-label="meta.name" :aria-describedby="describedBy">
    <button
      v-for="o in meta.options"
      :key="String(o.value)"
      role="radio"
      :aria-checked="value === o.value"
      :class="value === o.value ? 'on' : ''"
      :disabled="disabled || !!o.reason"
      :title="o.reason || undefined"
      @click="value !== o.value && emit('save', o.value)"
    >
      {{ o.label }}
    </button>
  </div>
  <div v-else-if="meta.control === 'color'" class="set-swatches" role="radiogroup" :aria-label="meta.name">
    <button
      v-for="o in meta.options"
      :key="String(o.value)"
      role="radio"
      :aria-checked="value === o.value"
      :aria-label="String(o.label)"
      :class="['set-swatch', { on: value === o.value }]"
      :style="{ background: swatch(o.value) }"
      :disabled="disabled"
      @click="emit('save', o.value)"
    />
  </div>
  <div v-else-if="meta.control === 'slider'" class="set-slider">
    <input
      type="range"
      :aria-label="t('{name}滑块', { name: meta.name })"
      :min="meta.min ?? 0"
      :max="meta.max ?? 100"
      :step="meta.step ?? 1"
      :value="live"
      :disabled="disabled"
      @input="live = Number(($event.target as HTMLInputElement).value)"
      @pointerup="commitSlider"
      @keyup="commitSlider"
      @blur="commitSlider"
    />
    <span class="set-value mono">
      {{ live }}<span v-if="meta.unit" class="muted"> {{ meta.unit }}</span>
    </span>
  </div>
  <div v-else-if="meta.control === 'number'" class="set-number-wrap">
    <span :class="['set-number', { invalid: error }]">
      <input
        inputmode="numeric"
        :aria-label="meta.name"
        :aria-invalid="!!error"
        :value="text"
        :disabled="disabled"
        @input="text = ($event.target as HTMLInputElement).value"
        @blur="commitNumber"
        @keydown="onBoxKey($event, commitNumber)"
      />
      <span v-if="meta.unit" class="muted">{{ meta.unit }}</span>
    </span>
    <span v-if="error" class="set-error">{{ error }}</span>
  </div>
  <input
    v-else
    class="set-text-input"
    :aria-label="meta.name"
    :placeholder="t('未命名')"
    maxlength="64"
    :value="text"
    :disabled="disabled"
    @input="text = ($event.target as HTMLInputElement).value"
    @blur="commitText"
    @keydown="onBoxKey($event, commitText)"
  />
</template>

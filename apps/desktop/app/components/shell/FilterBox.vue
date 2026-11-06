<!-- Filter box (UI-MAIN-17, W3): live syntax colouring, completion panel, error bar, cheat-sheet row.
     One per view (split panes each have their own, `pane` names it). The words are in lib/filterText.ts. -->
<script setup lang="ts">
import { computed, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { classHexNow } from "@/lib/classes";
import { filterInputId } from "@/lib/filterInput";
import { ATTR_LETTER, attrLabel, colourSegments, CONTEXT_BADGE, contextLabel, diagnosticText, unitLabel } from "@/lib/filterText";
import { t } from "@/lib/i18n";
import { editFilter, focusPane } from "@/state/actions";
import { get, useApp } from "@/state/store";

const props = defineProps<{ viewId: string | null; disabled?: boolean; pane?: string }>();

const input = ref<HTMLInputElement | null>(null);
const mirror = ref<HTMLPreElement | null>(null);
const text = useApp((s) => (props.viewId ? (s.views[props.viewId]?.filterText ?? "") : ""));
const parse = useApp((s) => (props.viewId ? (s.views[props.viewId]?.parse ?? null) : null));
const classes = useApp((s) => s.vocab?.classes ?? []);
const focused = ref(false);
const active = ref(0);
const closed = ref(false);
const inputId = computed(() => filterInputId(props.viewId));

// the input is not bound: what the user types stays while the store catches up
watch(
  [text, () => props.viewId, input],
  () => {
    if (input.value && input.value.value !== text.value) input.value.value = text.value;
  },
  { immediate: true, flush: "post" },
);

const completion = computed(() => parse.value?.completion ?? null);
const candidates = computed(() => completion.value?.candidates ?? []);
const exact = computed(() => candidates.value.length === 1 && candidates.value[0] === completion.value?.typed);
const errors = computed(() => (parse.value?.diagnostics ?? []).filter((d) => d.severity === "error"));
const panelOpen = computed(() => focused.value && !closed.value && !!parse.value && (candidates.value.length > 0 ? !exact.value : errors.value.length > 0));
const badge = computed(() => (completion.value ? CONTEXT_BADGE[completion.value.context] : undefined));
const segments = computed(() => colourSegments(text.value, parse.value));

function onInput(immediate = false): void {
  const el = input.value;
  if (!props.viewId || !el) return;
  closed.value = false;
  active.value = 0;
  void editFilter(props.viewId, el.value, el.selectionStart, immediate);
}

function accept(k: number): void {
  const c = completion.value;
  const el = input.value;
  if (!c || !el) return;
  const cand = candidates.value[k];
  if (cand === undefined) return;
  const v = el.value;
  el.value = v.slice(0, c.start) + cand + v.slice(c.end);
  const pos = c.start + cand.length;
  el.setSelectionRange(pos, pos);
  onInput(true);
}

// only the keys handled here stay here: Ctrl+K, Ctrl+T, Ctrl+Tab… still reach the window
// (single-key shortcuts are already ignored by the shell while a text field has the focus)
function onKey(e: KeyboardEvent): void {
  const n = candidates.value.length;
  if (panelOpen.value && n) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      e.stopPropagation();
      active.value = (active.value + (e.key === "ArrowDown" ? 1 : -1) + n) % n;
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      e.stopPropagation();
      accept(active.value);
      return;
    }
  }
  if (e.key === "Escape" || e.key === "Enter") e.stopPropagation();
  if (e.key === "Escape") {
    e.preventDefault();
    if (panelOpen.value) closed.value = true;
    else if (input.value?.value) {
      input.value.value = "";
      onInput(true);
    } else input.value?.blur();
  } else if (e.key === "Enter") onInput(true);
}

function onFocus(): void {
  focused.value = true;
  const viewId = props.viewId;
  const tab = get().tabs.find((x) => viewId && x.viewIds.includes(viewId));
  if (tab && viewId && tab.viewIds.length > 1) focusPane(tab.id, tab.viewIds.indexOf(viewId));
}

// a click in the panel lands before the panel goes
const onBlur = () => setTimeout(() => (focused.value = false), 120);

function clear(): void {
  if (input.value) input.value.value = "";
  onInput(true);
}

/** What a candidate's row says beside it: a class's extensions, an attribute's letter, a unit's name. */
function detail(c: string): string {
  const ctx = completion.value?.context;
  if (ctx === "class") return classes.value.find((x) => x.name === c)?.extensions.slice(0, 6).join(" ") ?? "";
  if (ctx === "attr") return ATTR_LETTER[c] ? `${attrLabel(c)} · ${ATTR_LETTER[c]}` : "";
  if (ctx === "unit") return unitLabel(c);
  return "";
}
const classOf = (c: string) => (completion.value?.context === "class" ? classes.value.find((x) => x.name === c) : undefined);
</script>

<template>
  <div :class="['filter', { disabled, 'pane-filter': pane }]" data-help="filtering">
    <label :for="inputId" class="sr-only">{{ pane ? t("过滤{pane}", { pane }) : t("过滤") }}</label>
    <span class="filter-icon">
      <Icon name="filter" />
    </span>
    <pre ref="mirror" class="filter-mirror" aria-hidden="true"><template v-for="s in segments" :key="s.start"><span v-if="s.cls !== null" :class="s.cls">{{ s.text }}</span><template v-else>{{ s.text }}</template></template></pre>
    <input
      :id="inputId"
      ref="input"
      class="filter-input"
      spellcheck="false"
      autocomplete="off"
      :disabled="disabled || !viewId"
      role="combobox"
      :aria-expanded="panelOpen"
      :aria-controls="`${inputId}-panel`"
      :aria-invalid="errors.length > 0"
      :placeholder="disabled ? t('开始扫描后可以过滤') : pane ? t('过滤此窗格') : t('按名称、类型、大小过滤，例如 *.mp4; >1gb; :class:视频')"
      @focus="onFocus"
      @input="onInput()"
      @keydown="onKey"
      @keyup="['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes($event.key) && onInput()"
      @click="onInput()"
      @scroll="mirror && (mirror.scrollLeft = ($event.currentTarget as HTMLInputElement).scrollLeft)"
      @blur="onBlur"
    />
    <button v-if="text" class="filter-clear" :aria-label="pane ? t('清除{pane}过滤', { pane }) : t('清除过滤')" :title="t('清除过滤（Esc）')" @mousedown.prevent @click="clear">
      <Icon name="close" :size="12" />
    </button>
    <kbd v-else-if="!pane" class="kbd filter-kbd">/</kbd>
    <div v-if="panelOpen" :id="`${inputId}-panel`" class="filter-panel" role="listbox" @mousedown.prevent>
      <template v-if="candidates.length > 0 && completion">
        <div class="panel-head">
          <span>{{ contextLabel(completion.context) }}</span>
          <code v-if="badge" class="badge">{{ badge }}</code>
          <span class="flex" />
          <span class="hint"> <kbd class="kbd">↑</kbd><kbd class="kbd">↓</kbd> {{ t("选择") }} <kbd class="kbd">Tab</kbd> {{ t("补全") }} </span>
        </div>
        <div class="panel-list">
          <div
            v-for="(c, k) in candidates.slice(0, 50)"
            :key="c"
            role="option"
            :aria-selected="k === active"
            :class="['panel-row', { on: k === active }]"
            @mouseenter="active = k"
            @click="accept(k)"
          >
            <span v-if="classOf(c)" class="swatch" :style="{ background: classHexNow(classOf(c)!.id) }" />
            <span class="cand">{{ c }}</span>
            <span class="detail">{{ detail(c) }}</span>
            <code v-if="k === active && completion.context === 'class'" class="preview">:class:{{ c }}</code>
          </div>
        </div>
      </template>
      <div v-if="errors[0]" class="panel-error" role="alert">
        <Icon name="warning" :size="14" />
        <code>{{ errors[0].text || "∅" }}</code>
        <span>{{ errors.length > 1 ? t("{error}（还有 {n} 处）", { error: diagnosticText(errors[0]), n: errors.length - 1 }) : diagnosticText(errors[0]) }}</span>
      </div>
      <div class="panel-cheats">
        <span>{{ t("大小") }} <code class="t-compare">&gt;1gb</code></span>
        <span>{{ t("时间") }} <code class="t-compare">m&lt;7d</code></span>
        <span>{{ t("标签") }} <code class="t-keyword">:red</code></span>
        <span>{{ t("属性") }} <code class="t-keyword">:attr:</code><code class="t-value">h</code></span>
        <span>{{ t("取反") }} <code class="t-negate">|</code></span>
      </div>
    </div>
  </div>
</template>

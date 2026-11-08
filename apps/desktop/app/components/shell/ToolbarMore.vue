<!-- 「更多」: the groups a narrow window folded away (lib/toolbarFit.ts), as one menu. Shown only when
     something is folded. Picking an item applies it and closes the menu, as the group would have. -->
<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { DIRECTIONS, directionLabel } from "@/lib/compare";
import { t } from "@/lib/i18n";
import { colorLabel, spaceSwitches } from "@/lib/toolbar";
import type { Collapse } from "@/lib/toolbarFit";
import type { ViewState } from "@/protocol/generated";
import { setViewSettings } from "@/state/actions";
import { pickVisual } from "@/state/commands";
import { useApp } from "@/state/store";
import { COLOR_MODES, offered, visualIcon } from "@/state/visuals";
import { registeredLayouts } from "@/visual/registry";

const props = defineProps<{ viewId: string; st: ViewState; fold: Collapse; compare: boolean }>();
const visuals = useApp((s) => s.visuals);
const dir = useApp((s) => s.views[props.viewId]?.state.direction ?? "all");
const open = ref(false);
const box = ref<HTMLDivElement | null>(null);
const button = ref<HTMLButtonElement | null>(null);

watch(open, async (now, _, onCleanup) => {
  if (!now) return;
  const close = (e: MouseEvent) => !box.value?.contains(e.target as Node) && (open.value = false);
  window.addEventListener("mousedown", close);
  onCleanup(() => window.removeEventListener("mousedown", close));
  await nextTick();
  // the checked item of the first group takes the focus, so arrows and Enter work at once
  box.value?.querySelector<HTMLElement>('[role^="menuitem"][aria-checked="true"]:not(:disabled), [role^="menuitem"]:not(:disabled)')?.focus();
});

const visualItems = computed(() => (props.fold.visualInMenu ? offered(visuals.value, registeredLayouts(), props.st.compare) : []));
const chips = computed(() => (props.fold.chipsInMenu && !props.compare ? spaceSwitches(props.st) : []));
/** the groups shown, in order; a separator goes before every one but the first */
const groups = computed(() =>
  [
    visualItems.value.length ? "visual" : "",
    props.fold.coloringInMenu && props.compare ? "direction" : "",
    props.fold.coloringInMenu && !props.compare ? "color" : "",
    chips.value.length ? "space" : "",
  ].filter(Boolean),
);

function pick(act: () => unknown): void {
  open.value = false;
  button.value?.focus();
  void act();
}

function onKey(e: KeyboardEvent): void {
  if (e.key === "Escape") {
    e.stopPropagation();
    open.value = false;
    button.value?.focus();
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Home" || e.key === "End") {
    e.preventDefault();
    e.stopPropagation();
    const items = Array.from(box.value?.querySelectorAll<HTMLElement>('[role^="menuitem"]:not(:disabled)') ?? []);
    const at = items.indexOf(document.activeElement as HTMLElement);
    const to = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : (at + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
    items[to]?.focus();
  }
}
</script>

<template>
  <div v-if="groups.length" ref="box" class="menu-anchor">
    <button ref="button" :class="['icon-btn', { on: open }]" :aria-label="t('更多')" :title="t('更多')" aria-haspopup="menu" :aria-expanded="open" @click="open = !open">
      <Icon name="more" />
    </button>
    <div v-if="open" class="menu toolbar-more" role="menu" :aria-label="t('更多')" @keydown="onKey">
      <template v-for="(g, i) in groups" :key="g">
        <div v-if="i" class="menu-sep" role="separator" />
        <div v-if="g === 'visual'" role="group" :aria-label="t('可视化')">
          <div class="menu-label" aria-hidden="true">{{ t("可视化（V）") }}</div>
          <button
            v-for="{ info, enabled, reason } in visualItems"
            :key="info.id"
            role="menuitemradio"
            :aria-checked="st.visual === info.id"
            class="menu-item"
            :disabled="!enabled"
            :title="reason ?? undefined"
            @click="pick(() => pickVisual(viewId, info.id))"
          >
            <span class="menu-check"><Icon v-if="st.visual === info.id" name="check" :size="14" /></span>
            <Icon :name="visualIcon(info.icon)" :size="14" />
            {{ info.name }}
          </button>
        </div>
        <div v-else-if="g === 'direction'" role="group" :aria-label="t('显示的变化')">
          <div class="menu-label" aria-hidden="true">{{ t("显示的变化（C）") }}</div>
          <button v-for="d in DIRECTIONS" :key="d" role="menuitemradio" :aria-checked="dir === d" class="menu-item" @click="pick(() => setViewSettings(viewId, { direction: d }))">
            <span class="menu-check"><Icon v-if="dir === d" name="check" :size="14" /></span>
            {{ directionLabel(d) }}
          </button>
        </div>
        <div v-else-if="g === 'color'" role="group" :aria-label="t('配色')">
          <div class="menu-label" aria-hidden="true">{{ t("配色（C）") }}</div>
          <button
            v-for="m in COLOR_MODES"
            :key="m"
            role="menuitemradio"
            :aria-checked="st.color_mode === m"
            class="menu-item"
            @click="pick(() => setViewSettings(viewId, { color_mode: m }))"
          >
            <span class="menu-check"><Icon v-if="st.color_mode === m" name="check" :size="14" /></span>
            {{ colorLabel(m) }}
          </button>
        </div>
        <div v-else role="group" :aria-label="t('显示')">
          <div class="menu-label" aria-hidden="true">{{ t("显示") }}</div>
          <button v-for="s in chips" :key="s.id" role="menuitemcheckbox" :aria-checked="s.on(st)" class="menu-item" @click="pick(() => setViewSettings(viewId, s.flip(st)))">
            <span class="menu-check"><Icon v-if="s.on(st)" name="check" :size="14" /></span>
            <span :class="['chip-swatch', s.id]" />
            {{ s.label() }}
            <span class="menu-key">{{ s.key }}</span>
          </button>
        </div>
      </template>
    </div>
  </div>
</template>

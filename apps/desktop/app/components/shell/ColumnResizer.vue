<!-- The left edge of the details column (UI-MAIN-64): drag to resize, double-click for the default
     width, arrow keys when focused. The map follows live through its own resize observer. -->
<script setup lang="ts">
import { clampInspector, INSPECTOR_DEFAULT, INSPECTOR_MAX, INSPECTOR_MIN, storeInspectorWidth } from "@/lib/inspectorWidth";
import { t } from "@/lib/i18n";
import { get, set, useApp } from "@/state/store";

const STEP = 16;

function apply(width: number, remember: boolean): void {
  const w = clampInspector(width, window.innerWidth);
  set({ inspectorWidth: w });
  if (remember) storeInspectorWidth(w);
}

const width = useApp((s) => s.inspectorWidth);
let drag: { x: number; w: number } | null = null;

function end(): void {
  if (!drag) return;
  drag = null;
  document.body.classList.remove("col-resizing");
  storeInspectorWidth(get().inspectorWidth);
}

function onPointerDown(e: PointerEvent): void {
  if (e.button !== 0) return;
  e.preventDefault();
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  drag = { x: e.clientX, w: get().inspectorWidth };
  document.body.classList.add("col-resizing");
}

function onPointerMove(e: PointerEvent): void {
  if (drag) apply(drag.w + drag.x - e.clientX, false); // the edge moves left: the column widens
}

function onDoubleClick(): void {
  set({ inspectorWidth: clampInspector(INSPECTOR_DEFAULT, window.innerWidth) });
  storeInspectorWidth(null);
}

function onKeyDown(e: KeyboardEvent): void {
  const step = e.shiftKey ? STEP * 4 : STEP;
  const w = width.value;
  const next = e.key === "ArrowLeft" ? w + step : e.key === "ArrowRight" ? w - step : e.key === "Home" ? INSPECTOR_MIN : e.key === "End" ? INSPECTOR_MAX : null;
  if (next == null) return;
  e.preventDefault();
  apply(next, true);
}
</script>

<template>
  <div
    class="col-resizer"
    role="separator"
    aria-orientation="vertical"
    :aria-label="t('详情栏宽度')"
    :aria-valuemin="INSPECTOR_MIN"
    :aria-valuemax="INSPECTOR_MAX"
    :aria-valuenow="width"
    tabindex="0"
    :title="t('拖动调整详情栏宽度，双击恢复默认')"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="end"
    @pointercancel="end"
    @lostpointercapture="end"
    @dblclick="onDoubleClick"
    @keydown="onKeyDown"
  />
</template>

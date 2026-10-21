// The left edge of the details column (UI-MAIN-64): drag to resize, double-click for the default
// width, arrow keys when focused. The map follows live through its own resize observer.

import { useRef } from "react";

import { clampInspector, INSPECTOR_DEFAULT, INSPECTOR_MAX, INSPECTOR_MIN, storeInspectorWidth } from "@/lib/inspectorWidth";
import { t } from "@/lib/i18n";
import { get, set, useApp } from "@/state/store";

const STEP = 16;

function apply(width: number, remember: boolean): void {
  const w = clampInspector(width, window.innerWidth);
  set({ inspectorWidth: w });
  if (remember) storeInspectorWidth(w);
}

export function ColumnResizer() {
  const width = useApp((s) => s.inspectorWidth);
  const drag = useRef<{ x: number; w: number } | null>(null);
  const end = () => {
    if (!drag.current) return;
    drag.current = null;
    document.body.classList.remove("col-resizing");
    storeInspectorWidth(get().inspectorWidth);
  };
  return (
    <div
      className="col-resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label={t("详情栏宽度")}
      aria-valuemin={INSPECTOR_MIN}
      aria-valuemax={INSPECTOR_MAX}
      aria-valuenow={width}
      tabIndex={0}
      title={t("拖动调整详情栏宽度，双击恢复默认")}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        drag.current = { x: e.clientX, w: get().inspectorWidth };
        document.body.classList.add("col-resizing");
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (d) apply(d.w + d.x - e.clientX, false); // the edge moves left: the column widens
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onLostPointerCapture={end}
      onDoubleClick={() => {
        set({ inspectorWidth: clampInspector(INSPECTOR_DEFAULT, window.innerWidth) });
        storeInspectorWidth(null);
      }}
      onKeyDown={(e) => {
        const step = e.shiftKey ? STEP * 4 : STEP;
        const next =
          e.key === "ArrowLeft"
            ? width + step
            : e.key === "ArrowRight"
              ? width - step
              : e.key === "Home"
                ? INSPECTOR_MIN
                : e.key === "End"
                  ? INSPECTOR_MAX
                  : null;
        if (next == null) return;
        e.preventDefault();
        apply(next, true);
      }}
    />
  );
}

// Plugins' colour modes in the toolbar (colors.v1): after the four built-in segments, one more that
// opens a list of the plugins' modes, each with its legend; picked, the segment shows its name. A
// mode that does not apply to the view's visualization is listed, disabled, with the reason.

import { useEffect, useRef, useState } from "react";

import type { ViewState } from "@/protocol/generated";
import { setViewSettings } from "@/state/actions";
import { useApp } from "@/state/store";
import { classHexNow } from "@/lib/classes";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { type ColorChoice, colorChoices, legendOf } from "./colors";
import { pluginName } from "./store";

/** The plugins' colour modes for a view (empty without any). */
export function usePluginColors(viewId: string): ColorChoice[] {
  const ext = useApp((s) => s.extensions);
  const layout = useApp((s) => {
    const v = s.views[viewId];
    return s.visuals.find((x) => x.id === v?.state.visual)?.layout ?? v?.info?.layout ?? null;
  });
  return colorChoices(ext, layout).filter((c) => c.plugin);
}

/** The menu items of the plugins' modes (the toolbar's 「更多」 when the colour group folds away). */
export function PluginColorItems({ viewId, st, onPick }: { viewId: string; st: ViewState; onPick: (act: () => unknown) => () => void }) {
  const choices = usePluginColors(viewId);
  return (
    <>
      {choices.map((c) => (
        <button key={c.id} role="menuitemradio" aria-checked={st.color_mode === c.id} className="menu-item" disabled={!!c.reason} title={c.reason ?? pluginName(c.plugin)} onClick={onPick(() => setViewSettings(viewId, { color_mode: c.id }))}>
          <span className="menu-check">{st.color_mode === c.id && <Icon name="check" size={14} />}</span>
          {c.title}
        </button>
      ))}
    </>
  );
}

/** The extra segment: the picked plugin mode's name (or 「插件」) and its list. */
export function PluginColorSegment({ viewId, st }: { viewId: string; st: ViewState }) {
  const choices = usePluginColors(viewId);
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !box.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    box.current?.querySelector<HTMLElement>('[role="menuitemradio"]:not(:disabled)')?.focus();
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  if (!choices.length) return null;
  const current = choices.find((c) => c.id === st.color_mode) ?? null;
  const pick = (id: string) => {
    setOpen(false);
    void setViewSettings(viewId, { color_mode: id });
  };
  return (
    <span className="menu-anchor plugin-colors" ref={box}>
      <button
        role="radio"
        aria-checked={!!current}
        aria-haspopup="menu"
        aria-expanded={open}
        className={current ? "on" : ""}
        title={current ? t("插件配色：{name}", { name: current.title }) : t("插件提供的配色")}
        onClick={() => setOpen(!open)}
      >
        {current ? current.title : t("插件")}
        <Icon name="chevronDown" size={12} />
      </button>
      {open && (
        <div
          className="menu plugin-colors-menu"
          role="menu"
          aria-label={t("插件提供的配色")}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              setOpen(false);
            }
          }}
        >
          {choices.map((c) => (
            <button key={c.id} role="menuitemradio" aria-checked={st.color_mode === c.id} className="menu-item two-line" disabled={!!c.reason} title={c.reason ?? undefined} onClick={() => pick(c.id)}>
              <span className="menu-check">{st.color_mode === c.id && <Icon name="check" size={14} />}</span>
              <span className="flex">
                <span>{c.title}</span>
                <span className="muted small">{c.reason ?? pluginName(c.plugin)}</span>
                {c.info && <LegendRow choice={c} />}
              </span>
            </button>
          ))}
        </div>
      )}
    </span>
  );
}

function LegendRow({ choice }: { choice: ColorChoice }) {
  const items = choice.info ? legendOf(choice.info, classHexNow) : [];
  if (!items.length) return null;
  return (
    <span className="color-legend">
      {items.map((x, i) => (
        <span key={i} className="color-legend-item">
          <span className="swatch" style={{ background: x.color }} />
          {x.text}
        </span>
      ))}
    </span>
  );
}

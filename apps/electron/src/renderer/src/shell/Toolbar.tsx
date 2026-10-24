// Toolbar (UI-MAIN-10…26): navigation, breadcrumb / path edit, filter, visualization, detail,
// colour, switches, split, command palette, delete basket. Acts on the focused view (G7). In a
// narrow window it folds groups into the 「更多」 menu (toolbarFit.ts).

import { useEffect, useRef, useState } from "react";

import type { ViewState } from "@/protocol/generated";
import { call, setViewSettings, settingsGo, settingsHistory, toggleSplit, viewCall } from "@/state/actions";
import { pickVisual } from "@/state/commands";
import { OVERVIEW } from "@/help/docs";
import { HelpSearch, helpTitle } from "@/pages/HelpPage";
import { PAGES, SettingsSearch } from "@/pages/SettingsPage";
import { helpGo, helpHistory } from "@/state/help";
import { activeTab, basketCount, focusedViewId, set, useApp } from "@/state/store";
import { COLOR_MODES, levelsThatFit, offered, visualIcon } from "@/state/visuals";
import { t } from "@/lib/i18n";
import { Icon, Kbd } from "@/ui/Icon";
import { registeredLayouts } from "@/visual/registry";
import { runShortcut } from "@/state/commands";
import { PluginColorItems, PluginColorSegment } from "@/plugins/ColorMenu";
import { PluginIcon } from "@/plugins/icons";
import { pluginTabTitle } from "@/plugins/slots";

import { ComparePicker, DIRECTIONS, DirectionSegment, directionLabel, ExitCompare } from "./Compare";
import { FilterBox } from "./FilterBox";
import { type Collapse, collapse, useCollapseLevel } from "./toolbarFit";

/** The breadcrumb keeps this much of itself (or all, when shorter) before groups fold away. */
const CRUMBS_MIN = 160;

export function Toolbar() {
  const tab = useApp((s) => activeTab(s));
  const viewId = useApp((s) => focusedViewId(s));
  const view = useApp((s) => (viewId ? s.views[viewId] : undefined));
  const paletteOpen = useApp((s) => s.paletteOpen);
  const basket = useApp(basketCount);
  const basketOpen = useApp((s) => s.basketOpen);
  const st = view?.state;
  const extensions = useApp((s) => s.extensions); // a plugin tab's title in the language on screen
  const scanTab = tab.kind === "scan";
  const settingsTab = tab.kind === "settings";
  const helpTab = tab.kind === "help";
  const settingsPage = useApp((s) => s.settingsPage);
  const helpPage = useApp((s) => (helpTab ? helpTitle(s.lang, s.help.page) : ""));
  const canBack = useApp((s) => (settingsTab ? s.settingsBack.length > 0 : helpTab ? s.helpBack.length > 0 : !!st?.can_back));
  const canForward = useApp((s) => (settingsTab ? s.settingsForward.length > 0 : helpTab ? s.helpForward.length > 0 : !!st?.can_forward));
  const back = () => (settingsTab ? settingsHistory(-1) : helpTab ? helpHistory(-1) : void viewCall(viewId, "view.back"));
  const forward = () => (settingsTab ? settingsHistory(1) : helpTab ? helpHistory(1) : void viewCall(viewId, "view.forward"));
  const cmp = scanTab ? tab.compare : undefined;
  const split = scanTab && !cmp && tab.viewIds.length > 1;
  const lang = useApp((s) => s.lang);
  const bar = useRef<HTMLDivElement>(null);
  // what decides the toolbar's width: when it changes, the toolbar works out again what fits
  const shown = [
    lang,
    tab.id,
    tab.kind,
    cmp ? "compare" : "",
    split ? "split" : "",
    st?.has_trash ? "trash" : "",
    levelsThatFit(view?.info, st?.detail ?? 0) ?? "",
    (view?.info?.breadcrumb ?? EMPTY).map((c) => c.label).join("/"),
  ].join("|");
  const fold = collapse(useCollapseLevel(bar, shown));

  return (
    <div className="toolbar" role="toolbar" aria-label={t("视图工具栏")} ref={bar}>
      <nav className="nav" aria-label={t("导航")}>
        <IconBtn icon="back" label={t("后退")} keys="Backspace" disabled={!canBack} onClick={back} />
        <IconBtn icon="forward" label={t("前进")} keys="Alt+→" disabled={!canForward} onClick={forward} />
        <IconBtn icon="up" label={t("上一级")} keys="Alt+↑" disabled={!st?.can_up} onClick={() => void viewCall(viewId, "view.up")} />
        <IconBtn icon="home" label={t("根")} keys="Ctrl+Home" disabled={!st?.can_up} onClick={() => void viewCall(viewId, "view.home")} />
      </nav>
      <div className="vsep" />
      {settingsTab ? (
        <nav className="crumbs" aria-label={t("当前位置")} data-fit-min={CRUMBS_MIN}>
          <span className="crumb-seg">
            <button className="crumb" onClick={() => settingsGo("general")}>
              <Icon name="settings" />
              {t("设置")}
            </button>
          </span>
          <span className="crumb-seg">
            <span className="crumb-arrow">
              <Icon name="chevronRight" size={12} />
            </span>
            <span className="crumb-cur" aria-current="page">
              {PAGES.find((p) => p.id === settingsPage)?.name}
            </span>
          </span>
        </nav>
      ) : helpTab ? (
        <nav className="crumbs" aria-label={t("当前位置")} data-fit-min={CRUMBS_MIN}>
          <span className="crumb-seg">
            <button className="crumb" onClick={() => helpGo(OVERVIEW)}>
              <Icon name="info" />
              {t("帮助")}
            </button>
          </span>
          <span className="crumb-seg">
            <span className="crumb-arrow">
              <Icon name="chevronRight" size={12} />
            </span>
            <span className="crumb-cur" aria-current="page">
              {helpPage}
            </span>
          </span>
        </nav>
      ) : cmp ? (
        <ComparePicker tab={tab} compact={fold.compact} />
      ) : scanTab && !split ? (
        <Breadcrumb viewId={viewId} />
      ) : !scanTab ? (
        <nav className="crumbs" aria-label={t("当前位置")} data-fit-min={CRUMBS_MIN}>
          <span className="crumb-cur">
            <Icon name={tab.kind === "machine" ? "diskGauge" : tab.kind === "plugin" ? "layers" : "drive"} />
            {tab.kind === "machine" ? t("本机磁盘") : tab.kind === "plugin" ? pluginTabTitle(tab, { extensions }) : t("新扫描")}
          </span>
        </nav>
      ) : (
        <span className="flexless" />
      )}
      {settingsTab && <SettingsSearch />}
      {helpTab && <HelpSearch />}
      {/* a plugin's tab has no view of its own: no filter box there (its filter buttons filter the scan's tab) */}
      {!split && !settingsTab && !helpTab && tab.kind !== "plugin" && <FilterBox viewId={scanTab ? viewId : null} disabled={!scanTab} />}
      <div className="toolbar-right">
        {cmp && (
          <>
            {st && viewId && (
              <>
                {!fold.visualInMenu && <VisualSwitch viewId={viewId} st={st} />}
                <DetailStepper viewId={viewId} st={st} compact={fold.compact} />
              </>
            )}
            {viewId && !fold.coloringInMenu && <DirectionSegment viewId={viewId} />}
            {st && viewId && <MoreMenu viewId={viewId} st={st} fold={fold} compare />}
            <ExitCompare tab={tab} compact={fold.compact} />
            <div className="vsep wide" />
            <IconBtn icon="split" label={t("对比模式下不能分屏")} disabled onClick={() => undefined} />
          </>
        )}
        {scanTab && st && viewId && !cmp && (
          <>
            {!fold.visualInMenu && <VisualSwitch viewId={viewId} st={st} />}
            <DetailStepper viewId={viewId} st={st} compact={fold.compact} />
            {!fold.coloringInMenu && (
              <div className="segmented" role="radiogroup" aria-label={t("配色")} title={t("配色（C）")}>
                <span className="seg-icon">
                  <Icon name="palette" size={14} />
                </span>
                {COLOR_MODES.map((m) => (
                  <button key={m} role="radio" aria-checked={st.color_mode === m} className={st.color_mode === m ? "on" : ""} onClick={() => void setViewSettings(viewId, { color_mode: m })}>
                    {colorLabel(m)}
                  </button>
                ))}
                <PluginColorSegment viewId={viewId} st={st} />
              </div>
            )}
            {!fold.chipsInMenu &&
              SPACE_SWITCHES.filter((s) => s.id !== "trash" || st.has_trash).map((s) => (
                // the recycle bin's only when the scan holds one (and not in a comparison)
                <button
                  key={s.id}
                  className={`chip${s.on(st) ? " on" : ""}${fold.chipSwatches ? " swatch-only" : ""}`}
                  aria-pressed={s.on(st)}
                  aria-label={s.label()}
                  title={s.title()}
                  onClick={() => void setViewSettings(viewId, s.flip(st))}
                >
                  <span className={`chip-swatch ${s.id}`} />
                  {!fold.chipSwatches && s.label()}
                </button>
              ))}
            <MoreMenu viewId={viewId} st={st} fold={fold} compare={false} />
            <div className="vsep wide" />
            <IconBtn icon="split" label={t("分屏")} pressed={split} onClick={() => void toggleSplit()} />
          </>
        )}
        <button className={`palette-btn${paletteOpen ? " on" : ""}${fold.compact ? " compact" : ""}`} aria-label={t("命令面板（Ctrl+K）")} title={t("命令面板（Ctrl+K）")} aria-expanded={paletteOpen} onClick={() => set({ paletteOpen: !paletteOpen })}>
          <Icon name="command" />
          {!fold.compact && <Kbd>Ctrl+K</Kbd>}
        </button>
        {!settingsTab && !helpTab && (
          <button
            className={`icon-btn basket${basketOpen ? " on" : ""}`}
            aria-label={t("删除篮，{n} 项", { n: basket })}
            aria-pressed={basketOpen}
            title={t("删除篮")}
            onClick={() => set({ basketOpen: !basketOpen })}
          >
            <Icon name="basket" />
            {basket > 0 && <span className="badge-count">{basket > 99 ? "99+" : basket}</span>}
          </button>
        )}
      </div>
    </div>
  );
}

const colorLabel = (m: (typeof COLOR_MODES)[number]): string => ({ class: t("类别"), flat: t("扁平"), time: t("时间"), branch: t("分支") })[m];

/** The free / unknown space and recycle bin switches (UI-MAIN-21, 22; the bin's when the scan holds one). */
const SPACE_SWITCHES = [
  { id: "free", key: "F", label: () => t("可用空间"), title: () => t("可用空间（F）"), on: (st: ViewState) => st.show_free, flip: (st: ViewState) => ({ show_free: !st.show_free }) },
  { id: "unknown", key: "U", label: () => t("未知空间"), title: () => t("未知空间（U）"), on: (st: ViewState) => st.show_unknown, flip: (st: ViewState) => ({ show_unknown: !st.show_unknown }) },
  { id: "trash", key: "R", label: () => binName(), title: () => t("回收站（R）"), on: (st: ViewState) => st.show_trash, flip: (st: ViewState) => ({ show_trash: !st.show_trash }) },
] as const;

/** 「更多」: the groups a narrow window folded away (toolbarFit.ts), as one menu. Shown only when
 * something is folded. Picking an item applies it and closes the menu, as the group would have. */
function MoreMenu({ viewId, st, fold, compare }: { viewId: string; st: ViewState; fold: Collapse; compare: boolean }) {
  const visuals = useApp((s) => s.visuals);
  const commands = useApp((s) => s.commands);
  const dir = useApp((s) => s.views[viewId]?.state.direction ?? "all");
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !box.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    // the checked item of the first group takes the focus, so arrows and Enter work at once
    box.current?.querySelector<HTMLElement>('[role^="menuitem"][aria-checked="true"]:not(:disabled), [role^="menuitem"]:not(:disabled)')?.focus();
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  const visualItems = fold.visualInMenu ? offered(visuals, registeredLayouts(), st.compare) : [];
  const chips = fold.chipsInMenu && !compare ? SPACE_SWITCHES.filter((s) => s.id !== "trash" || st.has_trash) : [];
  // plugins' commands for the toolbar (menus "toolbar", thin): always in this menu
  const extra = commands.filter((c) => c.menus.includes("toolbar"));
  if (!visualItems.length && !fold.coloringInMenu && !chips.length && !extra.length) return null;
  const pick = (act: () => unknown) => () => {
    setOpen(false);
    button.current?.focus();
    void act();
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      setOpen(false);
      button.current?.focus();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Home" || e.key === "End") {
      e.preventDefault();
      e.stopPropagation();
      const items = Array.from(box.current?.querySelectorAll<HTMLElement>('[role^="menuitem"]:not(:disabled)') ?? []);
      const at = items.indexOf(document.activeElement as HTMLElement);
      const to = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : (at + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
      items[to]?.focus();
    }
  };
  const check = (on: boolean) => <span className="menu-check">{on && <Icon name="check" size={14} />}</span>;
  const groups: React.ReactNode[] = [];
  if (visualItems.length)
    groups.push(
      <div key="visual" role="group" aria-label={t("可视化")}>
        <div className="menu-label" aria-hidden="true">{t("可视化（V）")}</div>
        {visualItems.map(({ info, enabled, reason }) => (
          <button key={info.id} role="menuitemradio" aria-checked={st.visual === info.id} className="menu-item" disabled={!enabled} title={reason ?? undefined} onClick={pick(() => pickVisual(viewId, info.id))}>
            {check(st.visual === info.id)}
            <Icon name={visualIcon(info.icon)} size={14} />
            {info.name}
          </button>
        ))}
      </div>,
    );
  if (fold.coloringInMenu && compare)
    groups.push(
      <div key="direction" role="group" aria-label={t("显示的变化")}>
        <div className="menu-label" aria-hidden="true">{t("显示的变化（C）")}</div>
        {DIRECTIONS.map((d) => (
          <button key={d} role="menuitemradio" aria-checked={dir === d} className="menu-item" onClick={pick(() => setViewSettings(viewId, { direction: d }))}>
            {check(dir === d)}
            {directionLabel(d)}
          </button>
        ))}
      </div>,
    );
  if (fold.coloringInMenu && !compare)
    groups.push(
      <div key="color" role="group" aria-label={t("配色")}>
        <div className="menu-label" aria-hidden="true">{t("配色（C）")}</div>
        {COLOR_MODES.map((m) => (
          <button key={m} role="menuitemradio" aria-checked={st.color_mode === m} className="menu-item" onClick={pick(() => setViewSettings(viewId, { color_mode: m }))}>
            {check(st.color_mode === m)}
            {colorLabel(m)}
          </button>
        ))}
        <PluginColorItems viewId={viewId} st={st} onPick={pick} />
      </div>,
    );
  if (chips.length)
    groups.push(
      <div key="space" role="group" aria-label={t("显示")}>
        <div className="menu-label" aria-hidden="true">{t("显示")}</div>
        {chips.map((s) => (
          <button key={s.id} role="menuitemcheckbox" aria-checked={s.on(st)} className="menu-item" onClick={pick(() => setViewSettings(viewId, s.flip(st)))}>
            {check(s.on(st))}
            <span className={`chip-swatch ${s.id}`} />
            {s.label()}
            <span className="menu-key">{s.key}</span>
          </button>
        ))}
      </div>,
    );
  if (extra.length)
    groups.push(
      <div key="plugins" role="group" aria-label={t("插件")}>
        <div className="menu-label" aria-hidden="true">{t("插件")}</div>
        {extra.map((c) => (
          <button key={c.id} role="menuitem" className="menu-item" title={c.status || undefined} onClick={pick(() => runShortcut(c.id))}>
            <span className="menu-check" />
            <PluginIcon plugin={c.plugin} name={c.icon} size={14} />
            {c.name}
            {c.keys[0] && <span className="menu-key">{c.keys[0]}</span>}
          </button>
        ))}
      </div>,
    );
  return (
    <div className="menu-anchor" ref={box}>
      <button ref={button} className={`icon-btn${open ? " on" : ""}`} aria-label={t("更多")} title={t("更多")} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon name="more" />
      </button>
      {open && (
        <div className="menu toolbar-more" role="menu" aria-label={t("更多")} onKeyDown={onKey}>
          {groups.flatMap((g, i) => (i ? [<div key={`sep-${i}`} className="menu-sep" role="separator" />, g] : [g]))}
        </div>
      )}
    </div>
  );
}

/** Visualization switch (V / Shift+V): an icon segment for each visualization the backend offers.
 * One this renderer or this view can't show stays in place, disabled, with the reason as its tooltip.
 * Switching keeps the zoom, the selection, the filter and the history (they live in the backend's view). */
function VisualSwitch({ viewId, st }: { viewId: string; st: ViewState }) {
  const visuals = useApp((s) => s.visuals);
  const items = offered(visuals, registeredLayouts(), st.compare);
  if (!items.length) return null; // a backend without visual.list: the treemap only
  return (
    <div className="segmented icon-only" role="radiogroup" aria-label={t("可视化")} title={t("可视化（V）")}>
      {items.map(({ info, enabled, reason }) => {
        const on = st.visual === info.id;
        return (
          <button key={info.id} role="radio" aria-checked={on} aria-label={info.name} className={on ? "on" : ""} disabled={!enabled} title={reason ?? info.name} onClick={() => void pickVisual(viewId, info.id)}>
            <Icon name={visualIcon(info.icon)} size={14} />
          </button>
        );
      })}
    </div>
  );
}

/** Detail level stepper (Ctrl+- / Ctrl+=). The flame graph and the polar flame graph may have room for
 * fewer levels than asked for: the stepper then says how many are drawn. `compact` (a narrow window)
 * leaves out the word 「层级」; the group's name and the buttons' tooltips still say it. */
function DetailStepper({ viewId, st, compact }: { viewId: string; st: ViewState; compact: boolean }) {
  const info = useApp((s) => s.views[viewId]?.info);
  const fit = levelsThatFit(info, st.detail);
  const fitText = fit == null ? null : t("画得下 {n} 层", { n: fit });
  return (
    <div className="stepper" role="group" aria-label={t("细节层级")}>
      <button aria-label={t("降低细节层级（Ctrl+-）")} title={t("降低细节层级（Ctrl+-）")} disabled={st.detail <= 1} onClick={() => void setViewSettings(viewId, { detail: st.detail - 1 })}>
        <Icon name="minus" size={14} />
      </button>
      <div className="stepper-mid" title={fitText ?? undefined}>
        <Icon name="layers" size={14} />
        {!compact && <span className="muted">{t("层级")}</span>}
        <span className="num">{st.detail}</span>
        {fitText && <span className="muted">{fitText}</span>}
      </div>
      <button aria-label={t("提高细节层级（Ctrl+=）")} title={t("提高细节层级（Ctrl+=）")} disabled={st.detail >= 15} onClick={() => void setViewSettings(viewId, { detail: st.detail + 1 })}>
        <Icon name="plus" size={14} />
      </button>
    </div>
  );
}

function IconBtn(props: { icon: Parameters<typeof Icon>[0]["name"]; label: string; keys?: string; disabled?: boolean; pressed?: boolean; onClick: () => void }) {
  const title = props.keys ? t("{label}（{keys}）", { label: props.label, keys: props.keys }) : props.label;
  return (
    <button className={`icon-btn${props.pressed ? " on" : ""}`} aria-label={title} title={title} aria-pressed={props.pressed} disabled={props.disabled} onClick={props.onClick}>
      <Icon name={props.icon} />
    </button>
  );
}

/** Breadcrumb with path editing (UI-MAIN-14…16). */
function Breadcrumb({ viewId }: { viewId: string | null }) {
  const view = useApp((s) => (viewId ? s.views[viewId] : undefined));
  const tab = useApp((s) => activeTab(s));
  const scan = useApp((s) => (tab.scanId ? s.scans[tab.scanId] : undefined));
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const crumbs = view?.info?.breadcrumb ?? EMPTY;
  const atRoot = crumbs.length <= 1;
  const whole = scan?.roots.length === 1 && scan.roots[0]!.whole_volume;

  useEffect(() => {
    const onEdit = () => setEditing(true);
    window.addEventListener("sk:edit-path", onEdit);
    return () => window.removeEventListener("sk:edit-path", onEdit);
  }, []);
  useEffect(() => {
    if (editing && input.current) {
      input.current.value = fullPath(crumbs, tab.title);
      input.current.select();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editing]);

  const go = async () => {
    const path = input.current?.value.trim() ?? "";
    if (!path || !tab.scanId) return;
    try {
      const found = await call("tree.find", { scan_id: tab.scanId, path });
      if (!found.node) {
        setError(t("路径不存在，或不在本次扫描范围内"));
        return;
      }
      await viewCall(viewId, "view.zoom", { key: found.node.id });
      setEditing(false);
      setError("");
    } catch (e) {
      setError(String(e));
    }
  };

  if (editing) {
    return (
      <div className="crumb-edit">
        <input
          ref={input}
          spellCheck={false}
          aria-label={t("路径")}
          aria-invalid={!!error}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") void go();
            if (e.key === "Escape") {
              setEditing(false);
              setError("");
            }
          }}
          onBlur={() => setTimeout(() => setEditing(false), 150)}
        />
        {error && <span className="crumb-error">{error}</span>}
      </div>
    );
  }
  const label = (c: { label: string }, i: number) => {
    if (c.label.startsWith("small:")) return t("{n} 个小项", { n: Number(c.label.slice(6)).toLocaleString("en-US") });
    if (c.label.startsWith("more:")) return t("+{n} 项", { n: Number(c.label.slice(5)).toLocaleString("en-US") });
    if (i === 0 && whole) return tab.title;
    return c.label.split(/[\\/]/).filter(Boolean).pop() ?? c.label;
  };
  return (
    <nav className="crumbs" aria-label={t("当前位置")} data-fit-min={CRUMBS_MIN}>
      {crumbs.map((c, i) => {
        const last = i === crumbs.length - 1;
        return (
          <span key={`${c.key}-${i}`} className="crumb-seg">
            {i > 0 && (
              <span className="crumb-arrow">
                <Icon name="chevronRight" size={12} />
              </span>
            )}
            <button
              className={last ? "crumb-cur" : "crumb"}
              aria-label={last ? t("{name}，编辑路径（Ctrl+L）", { name: label(c, i) }) : label(c, i)}
              onClick={() => {
                if (last) setEditing(true);
                else void jumpUp(viewId, crumbs.length - 1 - i);
              }}
            >
              {i === 0 && <Icon name="drive" />}
              {label(c, i)}
            </button>
          </span>
        );
      })}
      {atRoot && whole && <span className="crumb-note">{t("整个卷")}</span>}
    </nav>
  );
}

const EMPTY: { key: number; label: string }[] = [];

async function jumpUp(viewId: string | null, steps: number): Promise<void> {
  for (let s = 0; s < steps; s++) await viewCall(viewId, "view.up");
}

function fullPath(crumbs: { key: number; label: string }[], fallback: string): string {
  if (!crumbs.length) return fallback;
  const sep = window.sk.platform === "win32" ? "\\" : "/";
  const parts = crumbs.filter((c) => !c.label.startsWith("small:") && !c.label.startsWith("more:")).map((c) => c.label);
  const [first, ...rest] = parts;
  return [first!.replace(/[\\/]+$/, ""), ...rest].join(sep) + (rest.length ? "" : sep);
}

/** The recycle bin's name on this system: macOS calls it the Trash. */
const binName = () => (window.sk.platform === "darwin" ? t("废纸篓") : t("回收站"));

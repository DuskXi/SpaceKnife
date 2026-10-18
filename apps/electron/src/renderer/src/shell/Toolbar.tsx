// Toolbar (UI-MAIN-10…26): navigation, breadcrumb / path edit, filter, detail, colour, switches,
// split, command palette, delete basket. Acts on the focused view (G7).

import { useEffect, useRef, useState } from "react";

import { call, setViewSettings, settingsGo, settingsHistory, toggleSplit, viewCall } from "@/state/actions";
import { PAGES, SettingsSearch } from "@/pages/SettingsPage";
import { activeTab, focusedViewId, set, toast, useApp } from "@/state/store";
import { Icon, Kbd } from "@/ui/Icon";

import { FilterBox } from "./FilterBox";

export function Toolbar() {
  const tab = useApp((s) => activeTab(s));
  const viewId = useApp((s) => focusedViewId(s));
  const view = useApp((s) => (viewId ? s.views[viewId] : undefined));
  const paletteOpen = useApp((s) => s.paletteOpen);
  const basket = useApp((s) => s.basket.length);
  const st = view?.state;
  const scanTab = tab.kind === "scan";
  const settingsTab = tab.kind === "settings";
  const settingsPage = useApp((s) => s.settingsPage);
  const canBack = useApp((s) => (settingsTab ? s.settingsBack.length > 0 : !!st?.can_back));
  const canForward = useApp((s) => (settingsTab ? s.settingsForward.length > 0 : !!st?.can_forward));
  const split = scanTab && tab.viewIds.length > 1;

  return (
    <div className="toolbar" role="toolbar" aria-label="视图工具栏">
      <nav className="nav" aria-label="导航">
        <IconBtn icon="back" label="后退" keys="Backspace" disabled={!canBack} onClick={() => (settingsTab ? settingsHistory(-1) : void viewCall(viewId, "view.back"))} />
        <IconBtn icon="forward" label="前进" keys="Alt+→" disabled={!canForward} onClick={() => (settingsTab ? settingsHistory(1) : void viewCall(viewId, "view.forward"))} />
        <IconBtn icon="up" label="上一级" keys="Alt+↑" disabled={!st?.can_up} onClick={() => void viewCall(viewId, "view.up")} />
        <IconBtn icon="home" label="根" keys="Ctrl+Home" disabled={!st?.can_up} onClick={() => void viewCall(viewId, "view.home")} />
      </nav>
      <div className="vsep" />
      {settingsTab ? (
        <nav className="crumbs" aria-label="当前位置">
          <span className="crumb-seg">
            <button className="crumb" onClick={() => settingsGo("general")}>
              <Icon name="settings" />
              设置
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
      ) : scanTab && !split ? (
        <Breadcrumb viewId={viewId} />
      ) : !scanTab ? (
        <nav className="crumbs" aria-label="当前位置">
          <span className="crumb-cur">
            <Icon name="drive" />
            新扫描
          </span>
        </nav>
      ) : (
        <span className="flexless" />
      )}
      {settingsTab && <SettingsSearch />}
      {!split && !settingsTab && <FilterBox viewId={scanTab ? viewId : null} disabled={!scanTab} />}
      <div className="toolbar-right">
        {scanTab && st && (
          <>
            <div className="stepper" role="group" aria-label="细节层级">
              <button aria-label="降低细节层级（Ctrl+-）" title="降低细节层级（Ctrl+-）" disabled={st.detail <= 1} onClick={() => void setViewSettings(viewId, { detail: st.detail - 1 })}>
                <Icon name="minus" size={14} />
              </button>
              <div className="stepper-mid">
                <Icon name="layers" size={14} />
                <span className="muted">层级</span>
                <span className="num">{st.detail}</span>
              </div>
              <button aria-label="提高细节层级（Ctrl+=）" title="提高细节层级（Ctrl+=）" disabled={st.detail >= 15} onClick={() => void setViewSettings(viewId, { detail: st.detail + 1 })}>
                <Icon name="plus" size={14} />
              </button>
            </div>
            <div className="segmented" role="radiogroup" aria-label="配色" title="配色（C）">
              <span className="seg-icon">
                <Icon name="palette" size={14} />
              </span>
              {(["class", "flat", "time"] as const).map((m) => (
                <button key={m} role="radio" aria-checked={st.color_mode === m} className={st.color_mode === m ? "on" : ""} onClick={() => void setViewSettings(viewId, { color_mode: m })}>
                  {{ class: "类别", flat: "扁平", time: "时间" }[m]}
                </button>
              ))}
            </div>
            <button className={`chip${st.show_free ? " on" : ""}`} aria-pressed={st.show_free} title="可用空间（F）" onClick={() => void setViewSettings(viewId, { show_free: !st.show_free })}>
              <span className="chip-swatch free" />
              可用空间
            </button>
            <button className={`chip${st.show_unknown ? " on" : ""}`} aria-pressed={st.show_unknown} title="未知空间（U）" onClick={() => void setViewSettings(viewId, { show_unknown: !st.show_unknown })}>
              <span className="chip-swatch unknown" />
              未知空间
            </button>
            <div className="vsep wide" />
            <IconBtn icon="split" label="分屏" pressed={split} onClick={() => void toggleSplit()} />
          </>
        )}
        <button className={`palette-btn${paletteOpen ? " on" : ""}`} aria-label="命令面板（Ctrl+K）" title="命令面板（Ctrl+K）" aria-expanded={paletteOpen} onClick={() => set({ paletteOpen: !paletteOpen })}>
          <Icon name="command" />
          <Kbd>Ctrl+K</Kbd>
        </button>
        {tab.kind !== "settings" && (
          <button className="icon-btn basket" aria-label={`删除篮，${basket} 项`} title="删除篮（M6 接入）" onClick={() => toast("删除篮在 M6 接入")}>
            <Icon name="basket" />
            {basket > 0 && <span className="badge-count">{basket > 99 ? "99+" : basket}</span>}
          </button>
        )}
      </div>
    </div>
  );
}

function IconBtn(props: { icon: Parameters<typeof Icon>[0]["name"]; label: string; keys?: string; disabled?: boolean; pressed?: boolean; onClick: () => void }) {
  const title = props.keys ? `${props.label}（${props.keys}）` : props.label;
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
        setError("路径不存在，或不在本次扫描范围内");
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
          aria-label="路径"
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
    if (c.label.startsWith("small:")) return `${Number(c.label.slice(6)).toLocaleString("en-US")} 个小项`;
    if (c.label.startsWith("more:")) return `+${Number(c.label.slice(5)).toLocaleString("en-US")} 项`;
    if (i === 0 && whole) return tab.title;
    return c.label.split(/[\\/]/).filter(Boolean).pop() ?? c.label;
  };
  return (
    <nav className="crumbs" aria-label="当前位置">
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
              aria-label={last ? `${label(c, i)}，编辑路径（Ctrl+L）` : label(c, i)}
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
      {atRoot && whole && <span className="crumb-note">整个卷</span>}
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

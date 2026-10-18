// A scan tab: treemap (one or two panes, W8) + details panel + filter floater (W3) + hover card.

import { useLayoutEffect, useRef, useState } from "react";

import { closePane, focusPane, setViewSettings, viewCall } from "@/state/actions";
import { set, type Tab, useApp } from "@/state/store";
import { type HoverInfo, Treemap } from "@/treemap/Treemap";
import { fmtInt, fmtShare, fmtSize } from "@/lib/format";
import { HoverCard } from "@/shell/HoverCard";
import { Inspector } from "@/shell/Inspector";
import { Results } from "@/shell/Results";
import { isRunning, ScanBand, ScanDetails } from "@/shell/ScanProgress";
import { FilterBox } from "@/shell/FilterBox";
import { Icon } from "@/ui/Icon";

export function ScanPage({ tab }: { tab: Tab }) {
  const hidden = useApp((s) => s.inspectorHidden);
  const focused = tab.viewIds[tab.focusedPane] ?? tab.viewIds[0];
  // the side panel follows the focused view: match results while it filters (UI-W3-27, UI-W3-36)
  const filtering = useApp((s) => (focused ? !!s.views[focused]?.filter?.active : false));
  const split = tab.viewIds.length > 1;
  // while scanning: the progress band, and 「扫描详情」 in the side panel unless something is selected (W2 待定 15)
  const scan = useApp((s) => (tab.scanId ? s.scans[tab.scanId] : undefined));
  const selected = useApp((s) => (focused ? s.views[focused]?.state.selected != null : false));
  const running = isRunning(scan);
  return (
    <div className="content column">
      {running && scan && <ScanBand scan={scan} title={tab.title} />}
      <div className="content-row">
        <div className={`panes${split ? " split" : ""}`}>
          {tab.viewIds.map((v, i) => (
            <Pane key={v} tab={tab} index={i} viewId={v} split={split} focused={i === tab.focusedPane} onFocus={() => focusPane(tab.id, i)} />
          ))}
        </div>
        {hidden ? (
          <button className="inspector-open icon-btn" aria-label="显示侧栏" title="显示侧栏" onClick={() => set({ inspectorHidden: false })}>
            <Icon name="sidebar" />
          </button>
        ) : focused && filtering ? (
          <Results key={focused} viewId={focused} />
        ) : running && scan && !selected ? (
          <ScanDetails scan={scan} />
        ) : (
          focused && <Inspector viewId={focused} />
        )}
      </div>
    </div>
  );
}

function Pane(props: { tab: Tab; index: number; viewId: string; split: boolean; focused: boolean; onFocus: () => void }) {
  const { tab, index, viewId, split, focused, onFocus } = props;
  const [hover, setHover] = useState<HoverInfo | null>(null);
  const host = useRef<HTMLDivElement>(null);
  const [bounds, setBounds] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    const el = host.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setBounds({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return (
    <section className={`pane${focused && split ? " focused" : ""}`} onMouseDownCapture={onFocus}>
      {split && <PaneHead tab={tab} viewId={viewId} index={index} />}
      <div className="pane-map" ref={host}>
        <Treemap viewId={viewId} onHover={setHover} />
        <FilterFloater viewId={viewId} />
        <HoverCard hover={hover} bounds={bounds} />
      </div>
    </section>
  );
}

const NO_CRUMBS: { key: number; label: string }[] = [];

/** W8 pane header: its own breadcrumb and filter (G7). */
function PaneHead({ tab, viewId, index }: { tab: Tab; viewId: string; index: number }) {
  const crumbs = useApp((s) => s.views[viewId]?.info?.breadcrumb) ?? NO_CRUMBS;
  const scanned = useApp((s) => s.views[viewId]?.info?.scanned ?? 0);
  const filter = useApp((s) => s.views[viewId]?.filter ?? null);
  const scan = useApp((s) => (tab.scanId ? s.scans[tab.scanId] : undefined));
  const focused = tab.focusedPane === index;
  const side = index === 0 ? "左窗格" : "右窗格";
  const root = scan?.roots.length === 1 ? scan.roots[0] : undefined;
  const atRoot = crumbs.length <= 1;
  const label = (c: { label: string }, i: number) => {
    if (c.label.startsWith("small:")) return `${fmtInt(Number(c.label.slice(6)))} 个小项`;
    if (c.label.startsWith("more:")) return `+${fmtInt(Number(c.label.slice(5)))} 项`;
    if (i === 0) return c.label.replace(/[\\/]+$/, "") || c.label;
    return c.label.split(/[\\/]/).filter(Boolean).pop() ?? c.label;
  };
  return (
    <div className={`pane-head${focused ? " focused" : ""}`} role="region" aria-label={focused ? `${side}（焦点）` : side}>
      <nav className="pane-crumbs" aria-label={`${side}路径`}>
        {crumbs.map((c, i) => {
          const last = i === crumbs.length - 1;
          return (
            <span key={`${c.key}-${i}`} className="crumb-seg">
              {i > 0 && (
                <span className="crumb-arrow">
                  <Icon name="chevronRight" size={11} />
                </span>
              )}
              {last ? (
                <span className="pane-crumb cur">{label(c, i)}</span>
              ) : (
                <button className="pane-crumb" onClick={() => void jumpUp(viewId, crumbs.length - 1 - i)}>
                  {label(c, i)}
                </button>
              )}
            </span>
          );
        })}
      </nav>
      <span className="pane-size muted">{atRoot && root?.whole_volume && root.volume ? `整个卷 · ${fmtSize(root.volume.total, true)}` : fmtSize(scanned)}</span>
      <span className="flex" />
      {filter?.active && (
        <span className="pane-match" role="status">
          匹配 <span className="mono">{fmtInt(filter.files)}</span> 项 · <span className="mono">{fmtSize(filter.alloc)}</span>
        </span>
      )}
      <FilterBox viewId={viewId} pane={side} />
      <button className="icon-btn tiny" aria-label={`关闭${side}`} title={`关闭${side}`} onClick={() => closePane(tab.id, viewId)}>
        <Icon name="close" size={14} />
      </button>
    </div>
  );
}

async function jumpUp(viewId: string, steps: number): Promise<void> {
  for (let k = 0; k < steps; k++) await viewCall(viewId, "view.up");
}

/** W3 floater: match count and size, dim / hide switch (UI-W3-24, UI-W3-25). */
function FilterFloater({ viewId }: { viewId: string }) {
  const filter = useApp((s) => s.views[viewId]?.filter ?? null);
  const mode = useApp((s) => s.views[viewId]?.state.filter_mode ?? "dim");
  const scanned = useApp((s) => s.views[viewId]?.info?.scanned ?? 0);
  if (!filter?.active) return null;
  return (
    <div className="floater" role="group" aria-label="过滤结果">
      <span className="floater-stat" role="status" aria-live="polite">
        匹配 <span className="mono strong">{fmtInt(filter.files)}</span> 项 · <span className="mono strong">{fmtSize(filter.alloc)}</span>
        <span className="muted"> · 占 {fmtShare(filter.alloc, scanned)}</span>
        {filter.stale && <span className="warn"> · 表达式有误，显示的是上一次的结果</span>}
      </span>
      <div className="segmented small" role="radiogroup" aria-label="不匹配项的显示方式">
        {(["dim", "hide"] as const).map((m) => (
          <button key={m} role="radio" aria-checked={mode === m} className={mode === m ? "on" : ""} onClick={() => void setViewSettings(viewId, { filter_mode: m })}>
            {m === "dim" ? "暗化不匹配" : "隐藏不匹配"}
          </button>
        ))}
      </div>
    </div>
  );
}

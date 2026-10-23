// A scan tab: the view (one or two panes, W8) + details panel + filter floater (W3) + hover card.

import { type CSSProperties, Fragment, type ReactNode, useLayoutEffect, useRef, useState } from "react";

import { clearFilter, closePane, focusPane, setViewSettings, viewCall } from "@/state/actions";
import { get, set, type Tab, useApp } from "@/state/store";
import { pointerBox, VisualHost } from "@/visual/Host";
import { hosts } from "@/visual/hosts";
import { componentFor } from "@/visual/registry";
import { HIT_ROOT, type Hit, type HoverInfo, type VisualFrame } from "@/visual/types";
import { fmtInt, fmtShare, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { HoverCard } from "@/shell/HoverCard";
import { BasketDrawer } from "@/shell/BasketDrawer";
import { DupesDrawer } from "@/shell/DupesDrawer";
import { useDupes } from "@/state/dupes";
import { ColumnResizer } from "@/shell/ColumnResizer";
import { CompareHoverCard, CompareSidebar } from "@/shell/Compare";
import { Inspector } from "@/shell/Inspector";
import { Results } from "@/shell/Results";
import { isRunning, ScanBand, ScanDetails } from "@/shell/ScanProgress";
import { FilterBox } from "@/shell/FilterBox";
import { Icon } from "@/ui/Icon";

/** The details column's width for the CSS of `.inspector` (UI-MAIN-64). */
const columnWidth = (w: number) => ({ "--inspector-w": `${w}px` }) as CSSProperties;

export function ScanPage({ tab }: { tab: Tab }) {
  if (tab.compare) return <ComparePage tab={tab} />;
  return <NormalPage tab={tab} />;
}

/** Compare mode (W6): the change tree and the 「快照对比」 panel; while new ends compute, the old
 * result stays, dimmed (UI-W6-41). */
function ComparePage({ tab }: { tab: Tab }) {
  const c = tab.compare!;
  const basketOpen = useApp((s) => s.basketOpen);
  const hidden = useApp((s) => s.inspectorHidden);
  const width = useApp((s) => s.inspectorWidth);
  const busy = c.phase !== "ready";
  return (
    <div className="content column">
      <div className="content-row" style={columnWidth(width)}>
        <div className={`panes${busy ? " cmp-busy" : ""}`}>
          {c.viewId ? (
            <ComparePane viewId={c.viewId} />
          ) : (
            <div className="cmp-loading" role="status">
              <span className="spinner" aria-hidden="true" />
              <span>{c.message || t("正在对比…")}</span>
              {c.progress != null && c.phase === "comparing" && <span className="mono muted">{Math.round(c.progress * 100)}%</span>}
            </div>
          )}
        </div>
        {basketOpen ? (
          <BasketDrawer />
        ) : hidden ? (
          <button className="inspector-open icon-btn" aria-label={t("显示侧栏")} title={t("显示侧栏")} onClick={() => set({ inspectorHidden: false })}>
            <Icon name="sidebar" />
          </button>
        ) : (
          <>
            <ColumnResizer />
            <CompareSidebar tab={tab} />
          </>
        )}
      </div>
    </div>
  );
}

function ComparePane({ viewId }: { viewId: string }) {
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
    <section className="pane">
      <div className="pane-map" ref={host}>
        <VisualHost viewId={viewId} onHover={setHover} onContextMenu={(row, frame, ev) => openMenu(viewId, row, frame, ev)} />
        <FilterFloater viewId={viewId} />
        <CompareHoverCard hover={hover} bounds={bounds} />
      </div>
    </section>
  );
}

function NormalPage({ tab }: { tab: Tab }) {
  const hidden = useApp((s) => s.inspectorHidden);
  const basketOpen = useApp((s) => s.basketOpen);
  // the 「重复文件」 drawer comes after the basket (docs/dupes.md §10.5)
  const dupesOpen = useDupes((s) => s.open);
  const focused = tab.viewIds[tab.focusedPane] ?? tab.viewIds[0];
  // the side panel follows the focused view: match results while it filters (UI-W3-27, UI-W3-36)
  const filtering = useApp((s) => (focused ? !!s.views[focused]?.filter?.active : false));
  const split = tab.viewIds.length > 1;
  // while scanning: the progress band, and 「扫描详情」 in the side panel unless something is selected (W2 待定 15)
  const scan = useApp((s) => (tab.scanId ? s.scans[tab.scanId] : undefined));
  const selected = useApp((s) => (focused ? s.views[focused]?.state.selected != null : false));
  const running = isRunning(scan);
  const width = useApp((s) => s.inspectorWidth);
  return (
    <div className="content column">
      {running && scan && <ScanBand scan={scan} title={tab.title} />}
      <div className="content-row" style={columnWidth(width)}>
        <div className={`panes${split ? " split" : ""}`}>
          {tab.viewIds.map((v, i) => (
            <Pane key={v} tab={tab} index={i} viewId={v} split={split} focused={i === tab.focusedPane} onFocus={() => focusPane(tab.id, i)} />
          ))}
        </div>
        {basketOpen ? (
          <BasketDrawer />
        ) : dupesOpen ? (
          <DupesDrawer />
        ) : hidden ? (
          <button className="inspector-open icon-btn" aria-label={t("显示侧栏")} title={t("显示侧栏")} onClick={() => set({ inspectorHidden: false })}>
            <Icon name="sidebar" />
          </button>
        ) : (
          <>
            <ColumnResizer />
            {focused && filtering ? (
              <Results key={focused} viewId={focused} />
            ) : running && scan && !selected ? (
              <ScanDetails scan={scan} />
            ) : (
              focused && <Inspector viewId={focused} />
            )}
          </>
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
        <VisualHost viewId={viewId} onHover={setHover} onContextMenu={(row, frame, ev) => openMenu(viewId, row, frame, ev)} />
        <FilterFloater viewId={viewId} />
        <HoverCard hover={hover} bounds={bounds} />
      </div>
    </section>
  );
}

const NO_CRUMBS: { key: number; label: string }[] = [];

/** A translated text whose {placeholders} are React nodes: rich(t("匹配 {n} 项"), { n: <b>3</b> }). */
function rich(text: string, parts: Record<string, ReactNode>): ReactNode {
  return text.split(/\{(\w+)\}/).map((piece, i) => (i % 2 ? <Fragment key={i}>{parts[piece]}</Fragment> : piece));
}

/** W8 pane header: its own breadcrumb and filter (G7). */
function PaneHead({ tab, viewId, index }: { tab: Tab; viewId: string; index: number }) {
  const crumbs = useApp((s) => s.views[viewId]?.info?.breadcrumb) ?? NO_CRUMBS;
  const scanned = useApp((s) => s.views[viewId]?.info?.scanned ?? 0);
  const filter = useApp((s) => s.views[viewId]?.filter ?? null);
  const scan = useApp((s) => (tab.scanId ? s.scans[tab.scanId] : undefined));
  const focused = tab.focusedPane === index;
  const left = index === 0;
  const side = left ? t("左窗格") : t("右窗格");
  const root = scan?.roots.length === 1 ? scan.roots[0] : undefined;
  const atRoot = crumbs.length <= 1;
  const label = (c: { label: string }, i: number) => {
    if (c.label.startsWith("small:")) return t("{n} 个小项", { n: fmtInt(Number(c.label.slice(6))) });
    if (c.label.startsWith("more:")) return t("+{n} 项", { n: fmtInt(Number(c.label.slice(5))) });
    if (i === 0) return c.label.replace(/[\\/]+$/, "") || c.label;
    return c.label.split(/[\\/]/).filter(Boolean).pop() ?? c.label;
  };
  return (
    <div className={`pane-head${focused ? " focused" : ""}`} role="region" aria-label={focused ? (left ? t("左窗格（焦点）") : t("右窗格（焦点）")) : side}>
      <nav className="pane-crumbs" aria-label={left ? t("左窗格路径") : t("右窗格路径")}>
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
      <span className="pane-size muted">{atRoot && root?.whole_volume && root.volume ? t("整个卷 · {size}", { size: fmtSize(root.volume.total, true) }) : fmtSize(scanned)}</span>
      <span className="flex" />
      {filter?.active && (
        <span className="pane-match" role="status">
          {rich(t("匹配 {n} 项 · {size}"), { n: <span className="mono">{fmtInt(filter.files)}</span>, size: <span className="mono">{fmtSize(filter.alloc)}</span> })}
        </span>
      )}
      <FilterBox viewId={viewId} pane={side} />
      <button className="icon-btn tiny" aria-label={left ? t("关闭左窗格") : t("关闭右窗格")} title={left ? t("关闭左窗格") : t("关闭右窗格")} onClick={() => closePane(tab.id, viewId)}>
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
    <div className="floater" role="group" aria-label={t("过滤结果")}>
      <span className="floater-stat" role="status" aria-live="polite">
        {rich(t("匹配 {n} 项 · {size}"), { n: <span className="mono strong">{fmtInt(filter.files)}</span>, size: <span className="mono strong">{fmtSize(filter.alloc)}</span> })}
        <span className="muted"> · {t("占 {share}", { share: fmtShare(filter.alloc, scanned) })}</span>
        {filter.stale && <span className="warn"> · {t("表达式有误，显示的是上一次的结果")}</span>}
      </span>
      <div className="segmented small" role="radiogroup" aria-label={t("不匹配项的显示方式")}>
        {(["dim", "hide"] as const).map((m) => (
          <button key={m} role="radio" aria-checked={mode === m} className={mode === m ? "on" : ""} onClick={() => void setViewSettings(viewId, { filter_mode: m })}>
            {m === "dim" ? t("暗化不匹配") : t("隐藏不匹配")}
          </button>
        ))}
      </div>
      <button className="icon-btn small" aria-label={t("清除过滤")} title={t("清除过滤（Esc）")} onClick={() => clearFilter(viewId)}>
        <Icon name="close" size={14} />
      </button>
    </div>
  );
}

/** Right click on a block, a bar or a sector, or on the zoom root (UI-W5-01): the unified menu,
 * beside the target; where the target can be the whole disc, beside the pointer. */
function openMenu(viewId: string, row: Hit, frame: VisualFrame, ev: React.MouseEvent): void {
  const scanId = get().views[viewId]?.state.scan_id;
  const root = frame.info.root;
  if (!scanId || (row < 0 && !(row === HIT_ROOT && root))) return;
  const key = row >= 0 ? frame.key[row]! : root!.key;
  const node = row >= 0 ? frame.node[row]! : root!.node;
  const followsPointer = componentFor(frame.layout)?.hoverPlacement === "pointer";
  const avoid = followsPointer ? pointerBox({ x: ev.clientX, y: ev.clientY }) : hosts.get(viewId)?.menuAnchor(row)?.avoid;
  set({ menu: { viewId, scanId, key, node: node > 0 ? node : null, x: ev.clientX, y: ev.clientY, avoid } });
}

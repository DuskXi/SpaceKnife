// Compare mode (W6): the toolbar's end picker and 「显示的变化」, the 「快照对比」 side panel (net
// change, legend, growth / shrink lists, timeline) and the hover card of a change.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import type { ChangeItem, CompareNodeResult, CompareSide, CompareSourcesResult, CompareSummary, SnapshotInfo } from "@/protocol/generated";
import { call, setViewSettings, viewCall } from "@/state/actions";
import { exitCompare, NOW, setCompareEnds, swapCompare } from "@/state/compare";
import { runCommand } from "@/state/commands";
import { patchView, setting, type Tab, toast, useApp } from "@/state/store";
import { diffColor, diffTiers, GROW_COLORS, maxChange, SHRINK_COLORS, type Tiers } from "@/lib/diff";
import { fmtDelta, fmtInt, fmtSize, plural, sizeParts } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { cardAvoid, hoverFacts, sameTarget } from "@/shell/HoverCard";
import { Kind } from "@/visual/frame";
import { componentFor } from "@/visual/registry";
import type { HoverInfo } from "@/visual/types";
import { Icon, type IconName } from "@/ui/Icon";
import { CompareSections } from "@/plugins/slots";

const LIST_LIMIT = 12;
export const DIRECTIONS = ["all", "grow", "shrink"] as const;
export const directionLabel = (d: (typeof DIRECTIONS)[number]): string => ({ all: t("全部"), grow: t("增长"), shrink: t("减少") })[d];

const pad = (n: number) => String(n).padStart(2, "0");
function stamp(iso: string): string {
  const d = new Date(iso);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
const day = (iso: string) => {
  const d = new Date(iso);
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
};

/** "6 天 12 小时", "3 小时 20 分钟", "8 分钟" (UI-W6-24). */
export function fmtSpan(seconds: number): string {
  const s = Math.abs(seconds);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return h ? t("{d} 天 {h} 小时", { d, h }) : t("{d} 天", { d });
  if (h) return m ? t("{h} 小时 {m} 分钟", { h, m }) : t("{h} 小时", { h });
  return t("{m} 分钟", { m: Math.max(1, m) });
}

/** The label of one end in the picker and the timeline. */
function endLabel(spec: string, sources: CompareSourcesResult | null): string {
  if (spec === NOW) return t("现在");
  const snap = sources?.snapshots.find((s) => s.path === spec);
  return snap ? t("{time} 快照", { time: stamp(snap.created_at) }) : spec.split(/[\\/]/).pop() ?? spec;
}

const sideName = (side: CompareSide) => (side.kind === "now" ? t("现在") : stamp(side.created_at));

// ------------------------------------------------------------------ toolbar

/** 对比选择器 (UI-W6-03…06): 「对比」 · base ▾ › target ▾ · swap. */
/** The two ends of a comparison, in the toolbar. In a narrow window it shrinks (the snapshot names
 * get an ellipsis) down to 260 px; `compact` leaves out the word 「对比」. */
export function ComparePicker({ tab, compact = false }: { tab: Tab; compact?: boolean }) {
  const c = tab.compare!;
  return (
    <div className="cmp-picker" role="group" aria-label={t("快照对比")} data-fit-min={260}>
      <span className="cmp-label" title={compact ? t("对比") : undefined}>
        <Icon name="diff" size={14} />
        {!compact && t("对比")}
      </span>
      <EndMenu tab={tab} which="base" />
      <span className="cmp-arrow" aria-hidden="true">
        <Icon name="chevronRight" size={12} />
      </span>
      <EndMenu tab={tab} which="target" />
      <span className="cmp-vsep" />
      <button className="icon-btn small cmp-swap" aria-label={t("交换基准与对比目标")} title={t("交换基准与对比目标")} onClick={() => void swapCompare(tab.id)} disabled={c.phase !== "ready" && !c.shownId}>
        <Icon name="diff" size={14} />
      </button>
    </div>
  );
}

function EndMenu({ tab, which }: { tab: Tab; which: "base" | "target" }) {
  const c = tab.compare!;
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const spec = which === "base" ? c.base : c.target;
  const other = which === "base" ? c.target : c.base;
  const label = endLabel(spec, c.sources);
  useEffect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => box.current && !box.current.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", down, true);
    return () => window.removeEventListener("mousedown", down, true);
  }, [open]);
  const options: { spec: string; label: string; sub: string }[] = [];
  if (c.sources?.now) options.push({ spec: NOW, label: t("现在"), sub: fmtSize(c.sources.now.bytes_alloc) });
  for (const s of c.sources?.snapshots ?? []) options.push({ spec: s.path, label: t("{time} 快照", { time: stamp(s.created_at) }), sub: fmtSize(s.bytes_alloc) });
  const pick = (to: string) => {
    setOpen(false);
    if (to === spec) return;
    if (to === other) return void swapCompare(tab.id); // picking the other end swaps them
    void (which === "base" ? setCompareEnds(tab.id, to, c.target) : setCompareEnds(tab.id, c.base, to));
  };
  return (
    <div className={`cmp-end${spec === NOW ? " now" : ""}`} ref={box}>
      <button
        className="cmp-end-btn"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={which === "base" ? t("基准：{label}", { label }) : t("对比目标：{label}", { label })}
        onClick={() => setOpen(!open)}
        onKeyDown={(e) => e.key === "Escape" && setOpen(false)}
      >
        <span className="cmp-end-text">{label}</span>
        <Icon name="chevronDown" size={12} />
      </button>
      {open && (
        <div className="cmp-menu" role="listbox" aria-label={which === "base" ? t("基准") : t("对比目标")} onKeyDown={(e) => e.key === "Escape" && setOpen(false)}>
          {options.map((o) => (
            <button key={o.spec} role="option" aria-selected={o.spec === spec} className={`cmp-option${o.spec === spec ? " on" : ""}`} onClick={() => pick(o.spec)}>
              <span className="cmp-check">{o.spec === spec && <Icon name="check" size={14} />}</span>
              <span className="flex">{o.label}</span>
              <span className="muted mono small">{o.sub}</span>
            </button>
          ))}
          <div className="cmp-menu-sep" />
          <button
            className="cmp-option"
            onClick={async () => {
              setOpen(false);
              const path = await window.sk.dialog.openSnapshot();
              if (path) pick(path);
            }}
          >
            <span className="cmp-check" />
            <span className="flex">{t("打开快照文件…")}</span>
          </button>
        </div>
      )}
    </div>
  );
}

/** 「显示的变化：全部｜增长｜减少」 (UI-W6-07). */
export function DirectionSegment({ viewId }: { viewId: string }) {
  const dir = useApp((s) => s.views[viewId]?.state.direction ?? "all");
  return (
    <div className="segmented" role="radiogroup" aria-label={t("显示的变化")} title={t("显示的变化（C）")}>
      {DIRECTIONS.map((d) => (
        <button key={d} role="radio" aria-checked={dir === d} className={dir === d ? "on" : ""} onClick={() => void setViewSettings(viewId, { direction: d })}>
          {directionLabel(d)}
        </button>
      ))}
    </div>
  );
}

/** 「退出对比」; `compact` (a narrow window): the icon alone, the text stays its name and tooltip. */
export function ExitCompare({ tab, compact = false }: { tab: Tab; compact?: boolean }) {
  return (
    <button className={`outline-btn${compact ? " icon-only" : ""}`} onClick={() => void exitCompare(tab.id)} aria-label={t("退出对比")} title={t("退出对比（Esc）")}>
      <Icon name="close" size={14} />
      {!compact && t("退出对比")}
    </button>
  );
}

// ------------------------------------------------------------------ side panel

export function CompareSidebar({ tab }: { tab: Tab }) {
  const c = tab.compare!;
  const viewId = c.viewId;
  const seq = useApp((s) => (viewId ? s.views[viewId]?.info?.seq ?? -1 : -1));
  const frame = useApp((s) => (viewId ? s.views[viewId]?.frame ?? null : null));
  const dir = useApp((s) => (viewId ? s.views[viewId]?.state.direction ?? "all" : "all"));
  const selected = useApp((s) => (viewId ? s.views[viewId]?.state.selected ?? null : null));
  const [sum, setSum] = useState<CompareSummary | null>(null);
  const busy = c.phase !== "ready";

  // the numbers follow the zoom root and the filter (W6 待定 3): fetched again for every frame
  useEffect(() => {
    if (!viewId || seq < 0) return;
    let alive = true;
    void call("compare.summary", { view_id: viewId, limit: LIST_LIMIT })
      .then((r) => alive && setSum(r))
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [viewId, seq, c.info?.revision]);

  // the same scale as the map: the rows its visualization fills
  const legend = useMemo(() => {
    const g = frame ? componentFor(frame.layout)?.geometry : undefined;
    return diffTiers(frame?.delta ? maxChange(frame.delta, frame.kind, g ? (row) => g.filled(frame, row) : undefined) : 0);
  }, [frame]);
  const maxAbs = Math.max(1, ...(sum?.grow_items ?? []).map((i) => Math.abs(i.delta)), ...(sum?.shrink_items ?? []).map((i) => Math.abs(i.delta)));

  return (
    <aside className={`inspector cmp-side${busy ? " busy" : ""}`} aria-label={t("快照对比")} aria-busy={busy}>
      <div className="cmp-head">
        <strong>{t("快照对比")}</strong>
        {sum && !sum.whole && (
          <span className="cmp-scope muted small" title={sum.scope}>
            {t("范围：{name}", { name: sum.scope.split(/[\\/]/).filter(Boolean).pop() ?? "" })}
          </span>
        )}
        <span className="flex" />
        <MoreMenu sum={sum} />
      </div>
      {busy && (
        <div className="cmp-progress" role="status">
          <span className="spinner" aria-hidden="true" />
          <span>{c.message || t("正在对比…")}</span>
          {c.progress != null && c.phase === "comparing" && <span className="mono muted">{Math.round(c.progress * 100)}%</span>}
        </div>
      )}
      {sum ? (
        <div className="cmp-body">
          <Summary sum={sum} />
          <Legend heads={legend.heads} />
          {dir !== "shrink" && (
            <ChangeList
              title={t("增长最多")}
              right={t("前 {n} 项", { n: Math.min(LIST_LIMIT, sum.grow_items.length) })}
              items={sum.grow_items}
              empty={t("没有增长的项")}
              maxAbs={maxAbs}
              tiers={legend.tiers}
              viewId={viewId!}
              selected={selected}
            />
          )}
          {dir !== "grow" && (
            <ChangeList title={t("减少")} right={sum.shrink ? fmtDelta(-sum.shrink) : "0"} items={sum.shrink_items} empty={t("没有减少的项")} maxAbs={maxAbs} tiers={legend.tiers} viewId={viewId!} selected={selected} />
          )}
          {/* plugins.v1: reports placed in this panel (slot "compare") */}
          <CompareSections tab={tab} />
        </div>
      ) : (
        <div className="cmp-body muted cmp-wait">{c.phase === "failed" ? c.message : t("正在准备对比数据…")}</div>
      )}
      <Timeline tab={tab} />
    </aside>
  );
}

function MoreMenu({ sum }: { sum: CompareSummary | null }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => box.current && !box.current.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", down, true);
    return () => window.removeEventListener("mousedown", down, true);
  }, [open]);
  const copy = () => {
    setOpen(false);
    if (!sum) return;
    const lines = [
      t("快照对比：{base} → {target}（相隔 {span}）", { base: sideName(sum.base), target: sideName(sum.target), span: fmtSpan(sum.span_s) }),
      t("净变化 {net}，增长 {grow}，减少 {shrink}", { net: fmtDelta(sum.net), grow: fmtSize(sum.grow), shrink: fmtSize(sum.shrink) }),
      t("增长最多："),
      ...sum.grow_items.map((i) => `  ${fmtDelta(i.delta)}  ${i.path}`),
      t("减少："),
      ...sum.shrink_items.map((i) => `  ${fmtDelta(i.delta)}  ${i.path}`),
    ];
    window.sk.shell.copyText(lines.join("\n"));
    toast(t("已复制对比汇总"));
  };
  return (
    <div className="cmp-more" ref={box}>
      <button className="icon-btn" aria-label={t("更多对比选项")} title={t("更多对比选项")} aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon name="more" />
      </button>
      {open && (
        <div className="cmp-menu right" role="menu" onKeyDown={(e) => e.key === "Escape" && setOpen(false)}>
          <button role="menuitem" className="cmp-option" onClick={copy} disabled={!sum}>
            <span className="cmp-check">
              <Icon name="copy" size={14} />
            </span>
            {t("复制汇总")}
          </button>
          <button role="menuitem" className="cmp-option" disabled title={t("导出对比报告还没有接入（见 TODO）")}>
            <span className="cmp-check">
              <Icon name="upload" size={14} />
            </span>
            {t("导出对比报告…")}
          </button>
        </div>
      )}
    </div>
  );
}

function Summary({ sum }: { sum: CompareSummary }) {
  const label = sum.net > 0 ? t("净增长") : sum.net < 0 ? t("净减少") : t("无净变化");
  const [num, unit] = sum.net === 0 ? ["0", ""] : sizeParts(Math.abs(sum.net));
  const total = sum.grow + sum.shrink;
  const live = sum.target.kind === "now";
  const days = sum.days_to_full;
  // an estimate from a few minutes of change says nothing: at least a day between the ends (W6 待定 1)
  const showFill = live && sum.span_s >= 86400 && sum.daily != null && sum.daily > 0 && days != null && days <= 365;
  return (
    <section className="cmp-sum" aria-label={t("变化汇总")}>
      <div className="row-between small">
        <span className="muted">{label}</span>
        <span className="muted">{t("相隔 {span}", { span: fmtSpan(sum.span_s) })}</span>
      </div>
      <div className="cmp-big">
        <span className="mono">{sum.net === 0 ? "0" : `${sum.net > 0 ? "+" : "−"}${num}`}</span>
        {unit && <span className="cmp-unit">{unit}</span>}
      </div>
      <div className="cmp-ratio" role="img" aria-label={t("增长 {grow}，减少 {shrink}", { grow: fmtSize(sum.grow), shrink: fmtSize(sum.shrink) })}>
        {total > 0 && <span style={{ flexGrow: sum.grow, background: GROW_COLORS[3] }} />}
        {total > 0 && sum.shrink > 0 && <span style={{ flexGrow: sum.shrink, background: SHRINK_COLORS[2] }} />}
      </div>
      <div className="row-between small">
        <span>
          <span className="cmp-swatch" style={{ background: GROW_COLORS[3] }} /> {t("增长")} <span className="mono">{fmtSize(sum.grow)}</span>
        </span>
        <span>
          <span className="cmp-swatch" style={{ background: SHRINK_COLORS[2] }} /> {t("减少")} <span className="mono">{fmtSize(sum.shrink)}</span>
        </span>
      </div>
      {showFill && (
        <div className={`cmp-fill${days! <= 30 ? " warn" : ""}`} title={t("按两端之间的日均变化推算")}>
          <Icon name={days! <= 30 ? "warning" : "info"} size={14} />
          {plural(
            Math.floor(days!),
            t("日均 {daily} · 约 {days} 天后可用空间耗尽", { daily: fmtDelta(sum.daily!), days: fmtInt(Math.floor(days!)) }),
            "Daily average {daily} · free space runs out in about 1 day",
            { daily: fmtDelta(sum.daily!) },
          )}
        </div>
      )}
    </section>
  );
}

function Legend({ heads }: { heads: string[] }) {
  return (
    <section className="cmp-legend" aria-label={t("图例")}>
      <div className="cmp-grid">
        <span />
        {heads.map((h) => (
          <span key={h} className="muted small mono">
            {h}
          </span>
        ))}
        <span className="small">{t("增长")}</span>
        {GROW_COLORS.map((c) => (
          <span key={c} className="cmp-chip" style={{ background: c }} />
        ))}
        <span className="small">{t("减少")}</span>
        {SHRINK_COLORS.map((c, i) => (
          <span key={c} className="cmp-chip" style={{ background: c, gridColumn: i === 2 ? "span 2" : undefined }} />
        ))}
      </div>
      <div className="cmp-legend-notes small">
        <span>
          <span className="cmp-badge new">{t("新增")}</span> {t("新出现的项")}
        </span>
        <span className="muted">{t("面积 = 变化量")}</span>
      </div>
    </section>
  );
}

function ChangeList(props: { title: string; right: string; items: ChangeItem[]; empty: string; maxAbs: number; tiers: Tiers; viewId: string; selected: number | null }) {
  const { title, right, items, empty, maxAbs, tiers, viewId, selected } = props;
  return (
    <section className="cmp-list" aria-label={title}>
      <div className="row-between cmp-list-head">
        <strong className="small">{title}</strong>
        <span className="muted small mono">{right}</span>
      </div>
      {items.length === 0 && <div className="muted small cmp-empty">{empty}</div>}
      <div role="list">
        {items.map((it) => (
          <ChangeRow key={`${it.node}-${it.status}`} it={it} maxAbs={maxAbs} tiers={tiers} viewId={viewId} on={selected === it.node} />
        ))}
      </div>
    </section>
  );
}

const KIND_ICON: Record<ChangeItem["kind"], IconName> = { file: "file", dir: "folder", link: "file", other: "file", group: "layers" };

function ChangeRow({ it, maxAbs, tiers, viewId, on }: { it: ChangeItem; maxAbs: number; tiers: Tiers; viewId: string; on: boolean }) {
  const locate = () => void viewCall(viewId, "view.reveal", { key: it.node });
  return (
    <button
      role="listitem"
      className={`cmp-row${on ? " on" : ""}`}
      title={it.path}
      onClick={locate}
      onMouseEnter={() => patchView(viewId, { hoverKey: it.node })}
      onMouseLeave={() => patchView(viewId, { hoverKey: null })}
    >
      <Icon name={KIND_ICON[it.kind]} size={14} />
      <span className="cmp-row-main">
        <span className="cmp-row-name">
          <span className="cmp-row-label">{it.label}</span>
          {it.status === "new" && <span className="cmp-badge new">{t("新增")}</span>}
          {it.status === "deleted" && <span className="cmp-badge deleted">{t("已删除")}</span>}
          {it.reason && it.status !== "new" && it.status !== "deleted" && <span className="cmp-reason">{it.reason}</span>}
        </span>
        <span className="cmp-bar" style={{ width: `${(Math.abs(it.delta) / maxAbs) * 100}%`, background: diffColor(it.delta, tiers) }} />
      </span>
      <span className="mono cmp-row-delta">{fmtDelta(it.delta)}</span>
    </button>
  );
}

/** 快照时间轴 (UI-W6-33…35): click = base, Shift+click = target (W6 待定 13). */
function Timeline({ tab }: { tab: Tab }) {
  const c = tab.compare!;
  const snaps: SnapshotInfo[] = c.sources?.snapshots ?? [];
  const nowAt = c.sources?.now?.created_at ?? new Date().toISOString();
  const points = [...snaps.map((s) => ({ spec: s.path, at: s.created_at })), ...(c.sources?.now ? [{ spec: NOW, at: nowAt }] : [])].sort((a, b) => a.at.localeCompare(b.at));
  const t0 = points.length ? Date.parse(points[0]!.at) : 0;
  const t1 = points.length ? Date.parse(points[points.length - 1]!.at) : 1;
  const pos = (at: string) => (t1 > t0 ? ((Date.parse(at) - t0) / (t1 - t0)) * 100 : 100);
  const at = (spec: string) => points.find((p) => p.spec === spec)?.at;
  const a = at(c.base);
  const b = at(c.target);
  const lo = a && b ? Math.min(pos(a), pos(b)) : 0;
  const hi = a && b ? Math.max(pos(a), pos(b)) : 0;
  // labels of points closer than 12 % to a shown one are left out; the base and the target first
  const labelled = new Set<string>();
  const order = [...points].sort((p, q) => Number(q.spec === c.base || q.spec === c.target) - Number(p.spec === c.base || p.spec === c.target));
  for (const p of order) if ([...labelled].every((l) => Math.abs(pos(at(l)!) - pos(p.at)) >= 12)) labelled.add(p.spec);
  const click = (spec: string, shift: boolean) => {
    if (shift) {
      if (spec !== c.target) void (spec === c.base ? swapCompare(tab.id) : setCompareEnds(tab.id, c.base, spec));
    } else if (spec !== c.base) void (spec === c.target ? swapCompare(tab.id) : setCompareEnds(tab.id, spec, c.target));
  };
  return (
    <section className="cmp-timeline" aria-label={t("快照时间轴")}>
      <div className="row-between">
        {/* a list of them: plural in English, one form in Chinese */}
        <strong className="small">{byLang("快照", "Snapshots")}</strong>
        <button className="text-btn small" onClick={() => void runCommand("snapshot.save")} title={t("把当前扫描保存为快照（Ctrl+S）")}>
          <Icon name="snapshot" size={14} /> {t("保存快照")}
        </button>
      </div>
      <div className="cmp-track">
        <span className="cmp-rail" />
        <span className="cmp-span" style={{ left: `${lo}%`, width: `${hi - lo}%` }} />
        {points.map((p) => {
          const isBase = p.spec === c.base;
          const isTarget = p.spec === c.target;
          const name = p.spec === NOW ? t("现在") : isBase ? t("基准快照 {time}", { time: stamp(p.at) }) : t("快照 {time}", { time: stamp(p.at) });
          return (
            <button
              key={p.spec}
              className={`cmp-dot${isBase ? " base" : ""}${isTarget ? " target" : ""}`}
              style={{ left: `${pos(p.at)}%` }}
              aria-label={name}
              aria-pressed={isBase || isTarget}
              title={t("{name}（单击设为基准，Shift+单击设为对比目标）", { name: p.spec === NOW ? t("现在") : stamp(p.at) })}
              onClick={(e) => click(p.spec, e.shiftKey)}
            >
              {labelled.has(p.spec) && (
                <span className={`cmp-dot-label${isBase ? " strong" : ""}${p.spec === NOW && isTarget ? " accent" : ""}`}>
                  {p.spec === NOW ? t("现在") : day(p.at)}
                  {isBase && ` ${t("基准")}`}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </section>
  );
}

// ------------------------------------------------------------------ hover card

const HOVER_DELAY = 250;

/** UI-W6-20…22: both sizes, the change in percent, and the children that changed most. */
export function CompareHoverCard({ hover, bounds }: { hover: HoverInfo | null; bounds: { w: number; h: number } }) {
  const [shown, setShown] = useState<HoverInfo | null>(null);
  const [info, setInfo] = useState<CompareNodeResult | null>(null);
  const target = useApp((s) => {
    const t = s.tabs.find((x) => x.compare?.viewId === hover?.viewId);
    return t?.compare?.info?.target ?? null;
  });
  const keepOpen = useApp((s) => setting(s, "general.hover_keep_open", true));
  const card = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: 0, top: 0 });

  useEffect(() => {
    if (shown === hover) return;
    if (!hover) return setShown(null);
    if ((keepOpen || sameTarget(shown, hover)) && shown) return setShown(hover);
    setShown(null);
    const timer = setTimeout(() => setShown(hover), HOVER_DELAY);
    return () => clearTimeout(timer);
  }, [hover, shown, keepOpen]);

  // the change's details, once per target (not per pointer move)
  const viewId = shown?.viewId ?? null;
  const frame = shown?.frame ?? null;
  const targetHit = shown?.target ?? null;
  useEffect(() => {
    setInfo(null);
    if (!viewId || !frame || targetHit === null) return;
    const { kind, key } = hoverFacts(frame, targetHit, null);
    if (kind === Kind.FREE || kind === Kind.UNKNOWN) return;
    let alive = true;
    void call("compare.node", { view_id: viewId, key, children: 4 })
      .then((r) => alive && setInfo(r))
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [viewId, frame, targetHit]);

  useLayoutEffect(() => {
    if (!shown || !card.current) return;
    const r = cardAvoid(shown);
    const cw = card.current.offsetWidth;
    const ch = card.current.offsetHeight;
    let left = r.x + Math.min(r.w, 24) + 12;
    let top = r.y + Math.min(r.h, 24) + 12;
    if (left + cw > bounds.w) left = Math.max(0, r.x - cw - 12);
    if (top + ch > bounds.h) top = Math.max(0, bounds.h - ch - 8);
    setPos({ left, top });
  }, [shown, info, bounds]);

  if (!shown || !info) return null;
  const it = info.item;
  const tname = target ? sideName(target) : t("目标");
  const pct = it.base > 0 ? (it.delta / it.base) * 100 : null;
  const sep = it.path.includes("\\") ? "\\" : "/";
  const parts = it.path.split(sep);
  const path = parts.length > 3 ? `…${sep}${parts.slice(-2).join(sep)}` : it.path;
  return (
    <div className="hovercard cmp-card" ref={card} style={{ left: pos.left, top: pos.top }} role="tooltip">
      <div className="cmp-card-title">
        <Icon name={KIND_ICON[it.kind]} size={15} />
        <span className="flex cmp-card-name">{it.kind === "group" ? t("{n} 个小项", { n: fmtInt(it.members) }) : it.label}</span>
        <strong className="mono">{fmtDelta(it.delta)}</strong>
      </div>
      <div className="mono small muted cmp-card-path">{path}</div>
      <div className="row-between small">
        <span>
          <span className="muted">{t("基准")}</span> <span className="mono">{it.status === "new" ? "—" : fmtSize(it.base)}</span> <span className="muted">›</span>{" "}
          <span className="muted">{tname}</span> <span className="mono">{it.status === "deleted" ? "—" : fmtSize(it.target)}</span>
        </span>
        {pct != null && it.status !== "deleted" && <span className="mono">{`${pct >= 0 ? "+" : "−"}${Math.abs(pct).toFixed(1)}%`}</span>}
      </div>
      {info.children.length > 0 && (
        <div className="cmp-card-kids">
          {info.children.map((k) => (
            <div key={k.node} className="cmp-card-kid small">
              <Icon name={KIND_ICON[k.kind]} size={13} />
              <span className="flex cmp-card-name">{k.label}</span>
              {k.new_files > 0 && <span className="muted">{t("新增 {n} 项", { n: fmtInt(k.new_files) })}</span>}
              <span className="mono">{fmtDelta(k.delta)}</span>
            </div>
          ))}
        </div>
      )}
      <div className="muted small cmp-card-hint">{t("双击放大 · 右键更多")}</div>
    </div>
  );
}

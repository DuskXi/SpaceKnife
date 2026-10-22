// A report document (docs/plugins.md 「报告文档」) drawn with the app's own pieces: Markdown with raw
// HTML off, key numbers, key–value rows, sortable paged tables, bars, a stacked bar, a ring, a meter,
// a sparkline, node lists, callouts, progress and sections. Plugins send data only; every colour is a
// tone the app resolves, every number is formatted here (lib/format.ts), nodes link to the view.

import { type CSSProperties, type MutableRefObject, useEffect, useMemo, useRef, useState } from "react";

import type {
  BarsBlock,
  CalloutBlock,
  KvBlock,
  LabeledAction,
  MeterBlock,
  NodeRef,
  NodesBlock,
  PluginProgressEvent,
  ProgressBlock,
  Report,
  ReportContext,
  RingBlock,
  SectionBlock,
  SparkBlock,
  StackBlock,
  StatsBlock,
  TableBlock,
  TableRow,
} from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { classHexNow } from "@/lib/classes";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { useApp } from "@/state/store";
import { Meter, Sparkline, StackBar } from "@/ui/charts";
import { Icon } from "@/ui/Icon";

import { type RunContext, runAction } from "../actions";
import { callPlugin } from "../client";
import { toneColor } from "../colors";
import { PluginIcon } from "../icons";
import { baseName, showNode } from "../nodes";
import { type Cell, formatCell, nextSort, numericUnit, pageOf, share, sortRows } from "./format";
import { renderReportMd } from "./md";

export type Block = Report["blocks"][number];

/** Sections nest at most this deep; deeper blocks are left out. */
const MAX_DEPTH = 4;

const tone = (x: string | null | undefined) => toneColor(x, classHexNow);

interface Env {
  /** the plugin it comes from (actions run with its permissions) */
  owner: string;
  ctx: ReportContext;
  depth: number;
  /** the nodes of the rows chosen in a selectable table: the report's actions act on them */
  selected: MutableRefObject<number[]>;
}

export interface ReportViewProps {
  report: Report | null;
  loading: boolean;
  error: string;
  progress?: PluginProgressEvent | null;
  owner: string;
  ctx: ReportContext;
  /** shows 「刷新」 (refresh "manual") */
  manual?: boolean;
  onReload?: () => void;
  /** narrow places (the details column, the start page card): no title, tighter spacing */
  compact?: boolean;
  /** the title is shown by the place (a tab, a dialog) */
  hideTitle?: boolean;
}

export function ReportView(p: ReportViewProps) {
  useApp((s) => s.lang); // the frame's own words follow a language switch
  const selected = useRef<number[]>([]);
  const env: Env = { owner: p.owner, ctx: p.ctx, depth: 0, selected };
  const r = p.report;
  return (
    <div className={`report${p.compact ? " compact" : ""}`} aria-busy={p.loading}>
      {r && !p.hideTitle && (r.title || r.subtitle) && (
        <header className="report-head">
          {r.title && <h2>{r.title}</h2>}
          {r.subtitle && <div className="muted small">{r.subtitle}</div>}
        </header>
      )}
      {(p.manual || (p.error && p.onReload)) && (
        <div className="report-tools">
          {p.loading && <span className="muted small">{t("正在更新…")}</span>}
          <span className="flex" />
          {p.onReload && (
            <button className="ghost tiny" onClick={p.onReload} disabled={p.loading}>
              <Icon name="refresh" size={12} />
              {p.error ? t("重试") : t("刷新")}
            </button>
          )}
        </div>
      )}
      {p.error ? (
        <div className="report-callout danger" role="alert">
          <Icon name="warning" size={14} />
          <span>{t("报告没有生成：{error}", { error: p.error })}</span>
        </div>
      ) : !r && p.loading ? (
        <Skeleton progress={p.progress ?? null} />
      ) : r ? (
        <>
          {p.loading && p.progress && <ProgressLine done={p.progress.done} total={p.progress.total} text={p.progress.text} />}
          {r.blocks.length === 0 ? <div className="muted report-empty">{r.empty || t("没有内容")}</div> : <Blocks blocks={r.blocks} env={env} />}
          {r.actions.length > 0 && <Actions actions={r.actions} env={env} />}
          {r.footnote && <div className="report-foot muted small">{r.footnote}</div>}
        </>
      ) : null}
    </div>
  );
}

function Skeleton({ progress }: { progress: PluginProgressEvent | null }) {
  return (
    <div className="report-skeleton" aria-label={t("正在生成报告")}>
      {progress ? <ProgressLine done={progress.done} total={progress.total} text={progress.text} /> : <span className="muted small">{t("正在生成报告…")}</span>}
      <span className="sk-line w60" />
      <span className="sk-line w90" />
      <span className="sk-line w75" />
    </div>
  );
}

export function Blocks({ blocks, env }: { blocks: readonly Block[]; env: Env }) {
  if (env.depth >= MAX_DEPTH) return null;
  return (
    <>
      {blocks.map((b, i) => (
        <BlockView key={i} block={b} env={env} />
      ))}
    </>
  );
}

function BlockView({ block, env }: { block: Block; env: Env }) {
  switch (block.type) {
    case "md":
      return <div className="report-md" dangerouslySetInnerHTML={{ __html: renderReportMd(block.text ?? "") }} />;
    case "stats":
      return <Stats b={block} />;
    case "kv":
      return <Kv b={block} />;
    case "table":
      return <Table b={block} env={env} />;
    case "bars":
      return <Bars b={block} />;
    case "stack":
      return <Stack b={block} />;
    case "ring":
      return <RingChart b={block} />;
    case "meter":
      return <MeterView b={block} />;
    case "spark":
      return <Spark b={block} />;
    case "nodes":
      return <Nodes b={block} env={env} />;
    case "callout":
      return <Callout b={block} />;
    case "progress":
      return <ProgressView b={block} />;
    case "section":
      return <Section b={block} env={env} />;
    default:
      return <div className="muted small">{t("这个界面不认识这种内容：{type}", { type: String((block as { type?: unknown }).type) })}</div>;
  }
}

/** A node link: shown in its view on click. */
function NodeLink({ node, children }: { node: NodeRef; children: React.ReactNode }) {
  return (
    <button className="report-link" title={node.path || undefined} onClick={() => void showNode(node)}>
      {children}
    </button>
  );
}

function Stats({ b }: { b: StatsBlock }) {
  return (
    <div className="report-stats">
      {(b.items ?? []).map((it, i) => (
        <div key={i} className="report-stat" title={it.hint || undefined}>
          <div className="report-stat-label muted small">{it.label}</div>
          <div className="report-stat-value mono" style={{ color: tone(it.tone) ?? undefined }}>
            {formatCell(it.value, it.unit)}
          </div>
          {it.delta != null && it.delta !== 0 && (
            <div className={`report-stat-delta small ${it.delta > 0 ? "up" : "down"}`}>{deltaText(it.delta, it.unit)}</div>
          )}
          {it.hint && <div className="muted small report-stat-hint">{it.hint}</div>}
        </div>
      ))}
    </div>
  );
}

/** A signed change in the stat's unit: +1.2 GB, −3. */
function deltaText(d: number, unit: Parameters<typeof formatCell>[1]): string {
  const sign = d > 0 ? "+" : "−";
  return `${sign}${formatCell(Math.abs(d), unit === "text" || unit === "path" ? "count" : unit)}`;
}

function Kv({ b }: { b: KvBlock }) {
  return (
    <dl className="report-kv">
      {(b.items ?? []).map((it, i) => (
        <div key={i} className="report-kv-row">
          <dt className="muted">{it.label}</dt>
          <dd className={numericUnit(it.unit) ? "mono" : undefined}>{it.node ? <NodeLink node={it.node}>{formatCell(it.value, it.unit)}</NodeLink> : formatCell(it.value, it.unit)}</dd>
        </div>
      ))}
    </dl>
  );
}

function Table({ b, env }: { b: TableBlock; env: Env }) {
  const [sort, setSort] = useState<string | null>(b.sort ?? null);
  const [page, setPage] = useState(0);
  const [extra, setExtra] = useState<TableRow[]>([]);
  const [more, setMore] = useState(b.more ?? null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [moreError, setMoreError] = useState("");
  const [picked, setPicked] = useState<ReadonlySet<number>>(new Set());
  const rows = useMemo(() => [...(b.rows ?? []), ...extra], [b.rows, extra]);
  const sorted = useMemo(() => sortRows(rows, sort), [rows, sort]);
  const { rows: shown, pages, page: at } = pageOf(sorted, page, b.page_size ?? 50);
  const columns = b.columns ?? [];
  // a row's node links from its name or path (the first text column), else its first column
  const linkColumn = Math.max(0, columns.findIndex((c) => !numericUnit(c.unit)));
  const nodeOf = (r: TableRow) => r.node?.node ?? null;
  const selection = sorted.filter((r) => picked.has(rows.indexOf(r))).map(nodeOf).filter((n): n is number => n != null);
  const selKey = selection.join(",");
  useEffect(() => {
    if (b.select) env.selected.current = selection;
  }, [selKey]); // eslint-disable-line react-hooks/exhaustive-deps

  const loadMore = async () => {
    if (!more) return;
    setLoadingMore(true);
    setMoreError("");
    try {
      const next = (await callPlugin(more.method, { ...env.ctx, cursor: more.cursor })) as Report | null;
      const table = next?.blocks?.find((x): x is TableBlock => x.type === "table");
      setExtra((e) => [...e, ...(table?.rows ?? [])]);
      setMore(table?.more ?? null);
    } catch (err) {
      setMoreError(err instanceof RpcError ? err.message : String(err));
    } finally {
      setLoadingMore(false);
    }
  };

  const grid = { gridTemplateColumns: [b.select ? "28px" : "", ...columns.map((c) => (c.grow ? `minmax(0, ${c.grow}fr)` : numericUnit(c.unit) ? "max-content" : "minmax(0, 1fr)")), rows.some((r) => r.actions?.length) ? "max-content" : ""].filter(Boolean).join(" ") } as CSSProperties;
  return (
    <div className="report-table-wrap">
      <div className="report-table" role="table" style={grid}>
        <div className="report-tr head" role="row">
          {b.select && <span role="columnheader" />}
          {columns.map((c) => {
            const on = sort === c.key ? "asc" : sort === `-${c.key}` ? "desc" : null;
            return (
              <button
                key={c.key}
                role="columnheader"
                aria-sort={on === "asc" ? "ascending" : on === "desc" ? "descending" : "none"}
                className={`report-th${alignClass(c.align, c.unit)}`}
                onClick={() => {
                  setSort(nextSort(sort, c.key, numericUnit(c.unit)));
                  setPage(0);
                }}
              >
                {c.label}
                {on && (
                  <span className={`report-sort ${on}`} aria-hidden="true">
                    <Icon name="chevronDown" size={11} />
                  </span>
                )}
              </button>
            );
          })}
          {rows.some((r) => r.actions?.length) && <span role="columnheader" />}
        </div>
        {shown.map((r) => {
          const index = rows.indexOf(r);
          return (
            <div key={index} className={`report-tr${r.node ? " link" : ""}`} role="row" onDoubleClick={() => r.node && void showNode(r.node)}>
              {b.select && (
                <span role="cell" className="report-td">
                  <input
                    type="checkbox"
                    aria-label={t("选择这一行")}
                    checked={picked.has(index)}
                    onChange={(e) => {
                      const next = new Set(picked);
                      if (e.target.checked) next.add(index);
                      else next.delete(index);
                      setPicked(next);
                    }}
                  />
                </span>
              )}
              {columns.map((c, ci) => {
                const text = formatCell(r.cells?.[c.key] as Cell, c.unit);
                const full = c.unit === "path" ? String(r.cells?.[c.key] ?? "") : text;
                return (
                  <span key={c.key} role="cell" className={`report-td${alignClass(c.align, c.unit)}${numericUnit(c.unit) ? " mono" : ""}`} title={full}>
                    {ci === linkColumn && r.node ? <NodeLink node={r.node}>{text}</NodeLink> : text}
                  </span>
                );
              })}
              {rows.some((x) => x.actions?.length) && (
                <span role="cell" className="report-td report-row-actions">
                  {(r.actions ?? []).map((a, i) => (
                    <ActionButton key={i} action={a} env={env} rc={{ node: r.node?.node ?? null, scanId: r.node?.scan_id ?? env.ctx.scan_id ?? null }} tiny />
                  ))}
                </span>
              )}
            </div>
          );
        })}
      </div>
      {rows.length === 0 && <div className="muted small report-empty">{t("没有条目")}</div>}
      <div className="report-table-foot small">
        <span className="muted">{t("{n} 行", { n: rows.length })}</span>
        {pages > 1 && (
          <span className="report-pager">
            <button className="icon-btn tiny" aria-label={t("上一页")} disabled={at === 0} onClick={() => setPage(at - 1)}>
              <Icon name="back" size={12} />
            </button>
            <span className="mono">
              {at + 1} / {pages}
            </span>
            <button className="icon-btn tiny" aria-label={t("下一页")} disabled={at >= pages - 1} onClick={() => setPage(at + 1)}>
              <Icon name="forward" size={12} />
            </button>
          </span>
        )}
        {more && (
          <button className="ghost tiny" disabled={loadingMore} onClick={() => void loadMore()}>
            {loadingMore ? t("正在读取…") : t("读取更多")}
          </button>
        )}
        {moreError && <span className="danger-text">{moreError}</span>}
        {b.select && selection.length > 0 && <span className="muted">{t("已选 {n} 行，报告的操作只作用于它们", { n: selection.length })}</span>}
      </div>
    </div>
  );
}

const alignClass = (align: string | null | undefined, unit: Parameters<typeof numericUnit>[0]) =>
  align === "center" ? " center" : align === "end" || (!align && numericUnit(unit)) ? " end" : "";

function Bars({ b }: { b: BarsBlock }) {
  const items = b.items ?? [];
  const max = b.max ?? Math.max(0, ...items.map((x) => x.value));
  return (
    <div className="report-bars">
      {items.map((it, i) => (
        <div key={i} className="report-bar">
          <div className="row-between small">
            <span className="report-bar-label">{it.node ? <NodeLink node={it.node}>{it.label}</NodeLink> : it.label}</span>
            <span className="mono">{formatCell(it.value, it.unit)}</span>
          </div>
          <div className="bar">
            <div style={{ width: `${share(it.value, max) * 100}%`, background: tone(it.tone) ?? undefined }} />
          </div>
        </div>
      ))}
    </div>
  );
}

/** Colours of parts without a tone: the accent, then the class colours in turn. */
const PART_COLORS = ["accent", "class:video", "class:image", "class:audio", "class:archive", "class:code", "class:document", "muted"];
const partColor = (toneName: string | null | undefined, i: number) => tone(toneName) ?? tone(PART_COLORS[i % PART_COLORS.length]!)!;

function Stack({ b }: { b: StackBlock }) {
  const parts = b.parts ?? [];
  const total = parts.reduce((a, x) => a + Math.max(0, x.value), 0);
  return (
    <div className="report-stack">
      <StackBar
        height={10}
        title={parts.map((x) => `${x.label} ${formatCell(x.value, b.unit)}`).join(" · ")}
        segments={parts.map((x, i) => ({ key: String(i), share: share(x.value, total), color: partColor(x.tone, i), title: `${x.label} · ${formatCell(x.value, b.unit)}` }))}
      />
      <Legend parts={parts} unit={b.unit} total={total} />
    </div>
  );
}

function Legend({ parts, unit, total }: { parts: StackBlock["parts"]; unit: StackBlock["unit"]; total: number }) {
  return (
    <ul className="report-legend">
      {parts.map((x, i) => (
        <li key={i}>
          <span className="swatch" style={{ background: partColor(x.tone, i) }} />
          <span className="flex">{x.label}</span>
          <span className="mono">{formatCell(x.value, unit)}</span>
          <span className="mono muted">{formatCell(share(x.value, total), "pct")}</span>
        </li>
      ))}
    </ul>
  );
}

/** A ring of several parts, from the top, clockwise; the centre text in the middle. */
function RingChart({ b }: { b: RingBlock }) {
  const parts = b.parts ?? [];
  const total = parts.reduce((a, x) => a + Math.max(0, x.value), 0);
  const size = 96;
  const stroke = 10;
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  let at = 0;
  return (
    <div className="report-ring">
      <span className="ring" style={{ width: size, height: size }} role="img" aria-label={parts.map((x) => `${x.label} ${formatCell(x.value, b.unit)}`).join(" · ")}>
        <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
          <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="var(--sk-color-line-subtle)" strokeWidth={stroke} />
          {parts.map((x, i) => {
            const s = share(x.value, total);
            const start = at;
            at += s;
            if (s <= 0) return null;
            const gap = parts.length > 1 ? Math.min(2, s * c * 0.3) : 0;
            return (
              <circle
                key={i}
                cx={size / 2}
                cy={size / 2}
                r={r}
                fill="none"
                stroke={partColor(x.tone, i)}
                strokeWidth={stroke}
                strokeDasharray={`${Math.max(0.01, s * c - gap)} ${c}`}
                strokeDashoffset={-start * c}
                transform={`rotate(-90 ${size / 2} ${size / 2})`}
              >
                <title>{`${x.label} · ${formatCell(x.value, b.unit)}`}</title>
              </circle>
            );
          })}
        </svg>
        {b.center && <span className="ring-label">{b.center}</span>}
      </span>
      <Legend parts={parts} unit={b.unit} total={total} />
    </div>
  );
}

function MeterView({ b }: { b: MeterBlock }) {
  const s = share(b.value, b.max);
  const warn = b.tone === "warn" || b.tone === "danger";
  return (
    <div className="report-meter">
      <div className="row-between small">
        <span>{b.label}</span>
        <span className="mono">
          {formatCell(b.value, b.unit)} <span className="muted">/ {formatCell(b.max, b.unit)}</span>
        </span>
      </div>
      <Meter share={s} tone={warn ? "warn" : "accent"} title={`${b.label} ${formatCell(s, "pct")}`} />
    </div>
  );
}

function Spark({ b }: { b: SparkBlock }) {
  const values = b.values ?? [];
  const last = values[values.length - 1];
  return (
    <div className="report-spark">
      <Sparkline values={values} width={220} height={40} title={values.map((v, i) => `${b.labels?.[i] ?? i + 1}: ${formatCell(v, b.unit)}`).join(" · ")} />
      {last !== undefined && (
        <span className="mono small">
          {formatCell(last, b.unit)}
          {b.labels?.length ? <span className="muted"> · {b.labels[b.labels.length - 1]}</span> : null}
        </span>
      )}
    </div>
  );
}

function Nodes({ b, env }: { b: NodesBlock; env: Env }) {
  return (
    <div className="report-nodes">
      <ul>
        {(b.items ?? []).map((it, i) => (
          <li key={i}>
            <button className="report-node" onClick={() => void showNode(it.node)} title={it.node.path || undefined}>
              <Icon name="file" size={14} />
              <span className="report-node-text">
                <span className="report-node-name">{it.label || baseName(it.node.path) || `#${it.node.node}`}</span>
                {it.reason && <span className="muted small">{it.reason}</span>}
              </span>
              {it.size != null && <span className="mono">{fmtSize(it.size)}</span>}
            </button>
          </li>
        ))}
      </ul>
      {(b.actions ?? []).length > 0 && <Actions actions={b.actions} env={env} selection={(b.items ?? []).map((x) => x.node.node)} />}
    </div>
  );
}

const CALLOUT_ICON = { info: "info", ok: "check", warn: "warning", danger: "warning" } as const;

function Callout({ b }: { b: CalloutBlock }) {
  const kind = b.tone in CALLOUT_ICON ? b.tone : "info";
  return (
    <div className={`report-callout ${kind}`}>
      <Icon name={CALLOUT_ICON[kind]} size={14} />
      <span>{b.text}</span>
    </div>
  );
}

function ProgressView({ b }: { b: ProgressBlock }) {
  return <ProgressLine done={b.done} total={b.total} text={b.text} />;
}

export function ProgressLine({ done, total, text }: { done: number; total: number; text?: string }) {
  const s = share(done, total);
  return (
    <div className="report-progress" role="progressbar" aria-valuemin={0} aria-valuemax={total || 1} aria-valuenow={done}>
      <div className="row-between small">
        <span className="muted">{text || t("进行中")}</span>
        <span className="mono">{total > 0 ? formatCell(s, "pct") : ""}</span>
      </div>
      <div className="bar">
        <div style={{ width: `${s * 100}%` }} />
      </div>
    </div>
  );
}

function Section({ b, env }: { b: SectionBlock; env: Env }) {
  const [open, setOpen] = useState(!b.collapsed);
  return (
    <section className="report-section">
      <button className="report-section-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon name={open ? "chevronDown" : "chevronRight"} size={12} />
        <h3>{b.title}</h3>
      </button>
      {open && (
        <div className="report-section-body">
          <Blocks blocks={b.blocks ?? []} env={{ ...env, depth: env.depth + 1 }} />
        </div>
      )}
    </section>
  );
}

function Actions({ actions, env, selection }: { actions: readonly LabeledAction[]; env: Env; selection?: number[] }) {
  // a selectable table's chosen rows, else the nodes of the block the actions belong to
  const pick = (): Partial<RunContext> => {
    const chosen = env.selected.current.length ? env.selected.current : selection;
    return chosen?.length ? { selection: chosen, node: chosen[0] } : {};
  };
  return (
    <div className="report-actions">
      {actions.map((a, i) => (
        <ActionButton key={i} action={a} env={env} rc={pick} />
      ))}
    </div>
  );
}

/** A labeled action of a plugin; `rc` says what it acts on (or a function that says so when clicked). */
export function ActionButton({ action, env, rc, tiny }: { action: LabeledAction; env: Pick<Env, "owner" | "ctx">; rc: Partial<RunContext> | (() => Partial<RunContext>); tiny?: boolean }) {
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      const at = typeof rc === "function" ? rc() : rc;
      await runAction(action.action, { owner: env.owner, scanId: env.ctx.scan_id ?? null, viewId: env.ctx.view_id ?? null, node: env.ctx.node ?? null, ...at });
    } finally {
      setBusy(false);
    }
  };
  return (
    <button className={`ghost ${tiny ? "tiny" : "small"}`} disabled={busy} onClick={() => void run()}>
      {action.icon && <PluginIcon plugin={env.owner} name={action.icon} size={tiny ? 12 : 14} />}
      {action.label}
    </button>
  );
}

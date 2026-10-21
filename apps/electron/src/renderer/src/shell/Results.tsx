// W3 「匹配结果」 sidebar (UI-W3-27…34, UI-W3-38): summary, the expression as condition capsules,
// the result list (rows grouped by visible block, served page by page by view.results), key hints.

import { Fragment, useEffect, useMemo, useRef, useState } from "react";

import type { FilterNormal, FilterParse, Range, ResultRow, ViewResults } from "@/protocol/generated";
import { basketAdd, cancellable, clearFilter, viewCall } from "@/state/actions";
import { patchView, set, useApp } from "@/state/store";
import { fmtInt, fmtShare, fmtSize, sizeParts } from "@/lib/format";
import { useThrottled } from "@/lib/throttle";
import { attrLabel } from "@/shell/FilterBox";
import { isRunning } from "@/shell/ScanProgress";
import { Icon, Kbd } from "@/ui/Icon";
import { classHexNow } from "@/lib/classes";
import { t } from "@/lib/i18n";

const ROW_H = 50;
const PAGE = 100;
const OVERSCAN = 6;

type Sort = "size" | "name" | "count";
const SORTS: Sort[] = ["size", "name", "count"];
// labels are looked up when shown: the interface language can change at any time
const sortLabel = (k: Sort) => ({ size: t("按大小"), name: t("按名称"), count: t("按项数") })[k];
const tagColour = (c: string) => (({ red: t("红色"), yellow: t("黄色"), green: t("绿色"), blue: t("蓝色") }) as Record<string, string>)[c] ?? c;
const tagText = (c: string) => (({ red: t("红色标签"), yellow: t("黄色标签"), green: t("绿色标签"), blue: t("蓝色标签") }) as Record<string, string>)[c] ?? c;

/** A translated text with React nodes in its {placeholders}: the word order is the language's. */
function withNodes(text: string, nodes: Record<string, React.ReactNode>): React.ReactNode[] {
  return text.split(/\{(\w+)\}/).map((part, i) => (i % 2 ? <Fragment key={i}>{nodes[part]}</Fragment> : part));
}

export function Results({ viewId }: { viewId: string }) {
  const filter = useApp((s) => s.views[viewId]?.filter ?? null);
  const parse = useApp((s) => s.views[viewId]?.parse ?? null);
  const scanned = useApp((s) => s.views[viewId]?.info?.scanned ?? 0);
  const seq = useApp((s) => s.views[viewId]?.info?.seq ?? 0);
  const rootLabel = useApp((s) => s.views[viewId]?.info?.breadcrumb[0]?.label ?? "");
  const scanId = useApp((s) => s.views[viewId]?.state.scan_id ?? "");
  const storedSort = useApp((s) => s.views[viewId]?.state.results_sort ?? "size");
  const vocab = useApp((s) => s.vocab);
  const [sort, setSort] = useState<Sort>(storedSort);
  const [range, setRange] = useState({ first: 0, count: 20 });
  const [shown, setShown] = useState<{ meta: ViewResults; pages: Map<number, ResultRow[]> } | null>(null);
  const [active, setActive] = useState(-1);
  const list = useRef<HTMLDivElement>(null);
  const cache = useRef({ key: "", pages: new Map<number, ResultRow[]>(), inflight: new Map<number, () => void>() });
  // a new frame invalidates every page; while the scan runs (four frames a second) refetch once a second
  const running = useApp((s) => isRunning(s.scans[scanId]));
  const tick = useThrottled(seq, running ? 1000 : 0);
  const extClass = useMemo(() => {
    const m = new Map<string, string>();
    for (const c of vocab?.classes ?? []) for (const e of c.extensions) if (!m.has(e)) m.set(e, c.id);
    return m;
  }, [vocab]);

  useEffect(() => setSort(storedSort), [viewId, storedSort]);
  // leaving (filter cleared, tab switched) must not leave a hover outline behind on the treemap
  useEffect(() => () => patchView(viewId, { hoverKey: null }), [viewId]);
  // pages still on their way when the panel goes are not needed any more
  useEffect(() => () => cache.current.inflight.forEach((cancel) => cancel()), []);

  // fetch the pages under the visible range; keep showing the previous rows until they arrive
  useEffect(() => {
    if (!filter?.active) return;
    const key = `${viewId}:${tick}:${sort}`;
    const c = cache.current;
    if (c.key !== key) {
      c.inflight.forEach((cancel) => cancel()); // pages of an older frame or order
      c.key = key;
      c.pages = new Map();
      c.inflight = new Map();
    }
    const firstPage = Math.floor(Math.max(0, range.first - OVERSCAN) / PAGE);
    const lastPage = Math.floor((range.first + range.count + OVERSCAN) / PAGE);
    const need: number[] = [];
    for (let p = firstPage; p <= lastPage; p++) need.push(p);
    const publish = (meta: ViewResults) => {
      if (need.every((q) => c.pages.has(q) || q * PAGE >= meta.total)) setShown({ meta, pages: new Map(c.pages) });
    };
    for (const p of need) {
      if (c.pages.has(p) || c.inflight.has(p)) continue;
      const job = cancellable("view.results", { view_id: viewId, sort, offset: p * PAGE, limit: PAGE });
      c.inflight.set(p, job.cancel);
      job.promise
        .then((res) => {
          if (cache.current.key !== key) return;
          c.inflight.delete(p);
          if (!res) return;
          c.pages.set(p, res.rows);
          publish(res);
        })
        .catch(() => c.inflight.delete(p));
    }
  }, [viewId, tick, sort, range.first, range.count, filter?.active]);

  const onScroll = () => {
    const el = list.current;
    if (!el) return;
    const first = Math.floor(el.scrollTop / ROW_H);
    const count = Math.ceil(el.clientHeight / ROW_H) + 1;
    if (first !== range.first || count !== range.count) setRange({ first, count });
  };
  useEffect(() => {
    const el = list.current;
    if (!el) return;
    const ro = new ResizeObserver(onScroll);
    ro.observe(el);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const meta = shown?.meta ?? null;
  const total = meta?.total ?? 0;
  const rowAt = (i: number): ResultRow | undefined => shown?.pages.get(Math.floor(i / PAGE))?.[i % PAGE];

  const changeSort = (s: Sort) => {
    setSort(s);
    const v = useApp.getState().views[viewId];
    if (v) patchView(viewId, { state: { ...v.state, results_sort: s } });
    setActive(-1);
    list.current?.scrollTo({ top: 0 });
  };

  const hover = (key: number | null) => patchView(viewId, { hoverKey: key });
  const locate = (r: ResultRow) => r.kind !== "rest" && void viewCall(viewId, "view.reveal", { key: r.key });
  // the unified context menu for a result: its block is located first, so the menu has it (UI-W5-01)
  const menuFor = async (r: ResultRow, x: number, y: number) => {
    if (r.kind === "rest") return;
    await viewCall(viewId, "view.reveal", { key: r.key });
    set({ menu: { viewId, scanId, key: r.key, node: r.kind === "group" ? null : r.node, x, y } });
  };
  const zoom = (r: ResultRow) => r.kind !== "rest" && void viewCall(viewId, "view.zoom", { key: r.key });
  const toBasket = (r: ResultRow) => {
    if (r.kind === "file" || r.kind === "folder") basketAdd(scanId, r.node);
  };

  const moveTo = (i: number) => {
    const k = Math.max(0, Math.min(total - 1, i));
    setActive(k);
    const el = list.current;
    if (el) {
      if (k * ROW_H < el.scrollTop) el.scrollTop = k * ROW_H;
      else if ((k + 1) * ROW_H > el.scrollTop + el.clientHeight) el.scrollTop = (k + 1) * ROW_H - el.clientHeight;
    }
    const r = rowAt(k);
    if (r) hover(r.kind === "rest" ? null : r.key);
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return; // Alt+↑, Ctrl+Home… are view shortcuts
    const r = active >= 0 ? rowAt(active) : undefined;
    let handled = true;
    if (e.key === "ArrowDown") moveTo(active + 1);
    else if (e.key === "ArrowUp") moveTo(active - 1);
    else if (e.key === "Home") moveTo(0);
    else if (e.key === "End") moveTo(total - 1);
    else if (e.key === "PageDown") moveTo(active + Math.max(1, range.count - 1));
    else if (e.key === "PageUp") moveTo(active - Math.max(1, range.count - 1));
    else if (e.key === "Enter" && r) zoom(r);
    else if (e.key === "Delete" && r) toBasket(r);
    else if (e.key === " " && r) locate(r);
    else handled = false;
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  };

  const files = filter?.files ?? 0;
  const alloc = filter?.alloc ?? 0;
  const [num, unit] = sizeParts(alloc);
  const first = Math.max(0, range.first - OVERSCAN);
  const last = Math.min(total, range.first + range.count + OVERSCAN);
  const rows: React.ReactNode[] = [];
  for (let i = first; i < last; i++) {
    const r = rowAt(i);
    rows.push(
      <ResultLine
        key={i}
        id={`${viewId}-res-${i}`}
        index={i}
        row={r}
        max={meta?.max_alloc ?? 0}
        active={i === active}
        rootLabel={rootLabel}
        extClass={extClass}
        onHover={() => r && hover(r.kind === "rest" ? null : r.key)}
        onClick={() => {
          setActive(i);
          list.current?.focus({ preventScroll: true });
          if (r) locate(r);
        }}
        onMenu={(x, y) => {
          setActive(i);
          if (r) void menuFor(r, x, y);
        }}
        onDouble={() => r && zoom(r)}
      />,
    );
  }

  return (
    <aside className="inspector results" aria-label={t("匹配结果")}>
      <div className="res-head">
        <h2>{t("匹配结果")}</h2>
        <span className="flex" />
        <button className="ghost small res-exit" title={t("清除过滤，回到详情（Esc）")} onClick={() => clearFilter(viewId)}>
          <Icon name="close" size={12} />
          {t("清除过滤")}
        </button>
        <SortMenu sort={sort} onChange={changeSort} />
        <button className="icon-btn small" aria-label={t("隐藏侧栏")} title={t("隐藏侧栏")} onClick={() => set({ inspectorHidden: true })}>
          <Icon name="sidebar" />
        </button>
      </div>
      <div className="res-top">
        <div className="res-summary">
          <div className="big-num">
            <span className="num">{num}</span>
            <span className="unit">{unit}</span>
            <span className="res-count mono">{t("{n} 项", { n: fmtInt(files) })}</span>
          </div>
          <div className="bar" role="img" aria-label={t("占已扫描的 {share}", { share: fmtShare(alloc, scanned) })}>
            <div style={{ width: `${Math.min(100, scanned ? (alloc / scanned) * 100 : 0)}%` }} />
          </div>
          <div className="small muted">
            {withNodes(t("占已扫描 {size} 的 {share}"), {
              size: <span className="mono strong">{fmtSize(scanned)}</span>,
              share: <span className="mono strong">{fmtShare(alloc, scanned)}</span>,
            })}
          </div>
        </div>
        <Conditions parse={parse} />
      </div>
      <div className="res-cols" aria-hidden="true">
        <span>{t("名称")}</span>
        <span>{t("大小")}</span>
      </div>
      <div
        ref={list}
        className="res-list"
        role="listbox"
        aria-label={t("匹配结果列表")}
        tabIndex={0}
        aria-activedescendant={active >= 0 ? `${viewId}-res-${active}` : undefined}
        onScroll={onScroll}
        onKeyDown={onKey}
        onMouseLeave={() => hover(null)}
        onBlur={() => hover(null)}
      >
        <div style={{ height: total * ROW_H }} className="res-spacer">
          {rows}
        </div>
        {meta && total === 0 && (
          <div className="res-empty">
            <Icon name="search" size={20} />
            <div>{t("当前位置下没有匹配项")}</div>
            <div className="small muted">{t("放宽条件，或回到上一级（Alt+↑）在更大的范围里找")}</div>
          </div>
        )}
      </div>
      <div className="res-hints small muted">
        <span>{t("单击定位")}</span>
        <span>
          <Kbd>Enter</Kbd> {t("放大进入")}
        </span>
        <span>
          <Kbd>Delete</Kbd> {t("加入删除篮")}
        </span>
      </div>
    </aside>
  );
}

function ResultLine(props: {
  id: string;
  index: number;
  row: ResultRow | undefined;
  max: number;
  active: boolean;
  rootLabel: string;
  extClass: Map<string, string>;
  onHover: () => void;
  onClick: () => void;
  onDouble: () => void;
  onMenu: (x: number, y: number) => void;
}) {
  const { row: r, index } = props;
  const style = { top: index * ROW_H };
  if (!r) return <div className="res-row loading" style={style} aria-hidden="true" />;
  const ext = r.kind === "file" ? (r.name.includes(".") ? r.name.slice(r.name.lastIndexOf(".") + 1).toLowerCase() : "") : "";
  const cls = r.kind === "file" ? (props.extClass.get(ext) ?? "other") : null;
  const name =
    r.kind === "group"
      ? r.name
        ? r.name
        : t("{n} 个小项", { n: fmtInt(r.count) })
      : r.kind === "rest"
        ? t("其余 {n} 项", { n: fmtInt(r.count) })
        : r.name.replace(/[\\/]+$/, "") || props.rootLabel;
  const path = r.path || props.rootLabel;
  const tag = r.tag ? (r.tag_inherited ? t("{tag}（继承自文件夹）", { tag: tagText(r.tag) }) : tagText(r.tag)) : "";
  const spoken = [name, fmtSize(r.alloc)];
  if (r.kind !== "file") spoken.push(t("{n} 项", { n: fmtInt(r.count) }));
  if (tag) spoken.push(tag);
  spoken.push(t("位于 {path}", { path }));
  return (
    <div
      id={props.id}
      className={`res-row${props.active ? " on" : ""}${r.kind === "rest" ? " rest" : ""}`}
      style={style}
      role="option"
      aria-selected={props.active}
      aria-label={spoken.join(t("，"))}
      onMouseEnter={props.onHover}
      onClick={props.onClick}
      onDoubleClick={props.onDouble}
      onContextMenu={(e) => {
        e.preventDefault();
        props.onMenu(e.clientX, e.clientY);
      }}
    >
      <span className={`res-icon ${r.kind}`} style={cls ? { background: classHexNow(cls) } : undefined}>
        <Icon name={r.kind === "file" ? "file" : r.kind === "folder" ? "folder" : "layers"} size={15} />
      </span>
      <span className="res-text">
        <span className="res-name">
          <span className="res-label" title={name}>
            {name}
          </span>
          {r.tag && <span className={`tag-mini ${r.tag}${r.tag_inherited ? " inherited" : ""}`} role="img" aria-label={tag} title={tag} />}
          {r.kind === "folder" && <span className="res-n mono">{t("{n} 项", { n: fmtInt(r.count) })}</span>}
          {r.kind === "group" && r.name && r.count > 1 && <span className="res-n mono">{t("{n} 项", { n: fmtInt(r.count) })}</span>}
        </span>
        <span className="res-path mono" title={path}>
          <span>{path}</span>
        </span>
      </span>
      <span className="res-size">
        <span className="mono">{fmtSize(r.alloc)}</span>
        <span className="res-bar">
          <span style={{ width: `${props.max ? Math.max(2, (r.alloc / props.max) * 100) : 0}%` }} />
        </span>
      </span>
    </div>
  );
}

function SortMenu({ sort, onChange }: { sort: Sort; onChange: (s: Sort) => void }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !box.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  return (
    <div className="res-sort" ref={box}>
      <button className="ghost small" aria-haspopup="menu" aria-expanded={open} aria-label={t("排序：{sort}", { sort: sortLabel(sort) })} onClick={() => setOpen(!open)}>
        {sortLabel(sort)}
        <Icon name="chevronDown" size={12} />
      </button>
      {open && (
        <div
          className="menu"
          role="menu"
          aria-label={t("排序方式")}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              setOpen(false);
            }
          }}
        >
          {SORTS.map((s) => (
            <button
              key={s}
              role="menuitemradio"
              aria-checked={s === sort}
              className="menu-item"
              autoFocus={s === sort}
              onClick={() => {
                setOpen(false);
                if (s !== sort) onChange(s);
              }}
            >
              <span className="menu-check">{s === sort && <Icon name="check" size={14} />}</span>
              {sortLabel(s)}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ conditions (UI-W3-30)

const pendingLabel = (category: string): string =>
  (({ name: t("名称"), class: t("类别"), folder: t("文件夹"), tag: t("标签"), size: t("大小"), age: t("时间"), attr: t("属性") }) as Record<string, string>)[category] ?? category;
/** 「7 天」: a number with its time unit. */
function timeSpan(n: string, unit: string): string {
  const fields = { n };
  if (unit === "s") return t("{n} 秒", fields);
  if (unit === "min") return t("{n} 分钟", fields);
  if (unit === "h") return t("{n} 小时", fields);
  if (unit === "d") return t("{n} 天", fields);
  if (unit === "w") return t("{n} 周", fields);
  if (unit === "month") return t("{n} 个月", fields);
  if (unit === "y") return t("{n} 年", fields);
  return `${n} ${unit}`;
}
const ageLabel = (field: string): string =>
  (({ modify: t("修改时间"), create: t("创建时间"), access: t("访问时间") }) as Record<string, string>)[field] ?? t("{field}时间", { field });

interface Capsule {
  label: string;
  values: { text: string; cls: string }[];
  joiner: string;
}

function Conditions({ parse }: { parse: FilterParse | null }) {
  const lastValid = useRef<FilterNormal | null>(null);
  if (parse?.normal) lastValid.current = parse.normal;
  const vocab = useApp((s) => s.vocab);
  const normal = parse?.normal ?? lastValid.current;
  if (!normal) return null;
  const className = (id: string) => vocab?.classes.find((c) => c.id === id)?.name ?? id;
  const caps = capsules(normal, className);
  const pending = parse?.pending?.category ?? null;
  if (!caps.length && !pending) return null;
  return (
    <section className="res-conds" aria-label={t("条件")}>
      <h3>{t("条件")}</h3>
      <div className="conds">
        {caps.map((c, i) => (
          <span key={i} className="cond">
            <span className="cond-label">{c.label}</span>
            {c.values.map((v, k) => (
              <span key={k}>
                {k > 0 && <span className="cond-join">{c.joiner}</span>}
                <code className={v.cls}>{v.text}</code>
              </span>
            ))}
          </span>
        ))}
        {pending && (
          <span className="cond pending">
            <span className="cond-label">{pendingLabel(pending)}</span>
            <span className="muted">{t("选择中…")}</span>
          </span>
        )}
      </div>
    </section>
  );
}

function capsules(n: FilterNormal, className: (id: string) => string): Capsule[] {
  const out: Capsule[] = [];
  const or = t("或");
  const and = t("且");
  const list = (label: string, items: string[], cls: string, joiner = or) =>
    items.length && out.push({ label, joiner, values: items.map((text) => ({ text, cls })) });
  const not = (a: string) => t("非{attr}", { attr: attrLabel(a) });
  list(t("名称"), n.masks, "t-mask");
  list(t("排除名称"), n.not_masks, "t-mask");
  list(t("文件夹"), n.folders.map((f) => `\\${f}`), "t-folder");
  list(t("排除文件夹"), n.not_folders.map((f) => `\\${f}`), "t-folder");
  list(t("类别"), n.classes.map(className), "t-value");
  list(t("排除类别"), n.not_classes.map(className), "t-value");
  for (const r of n.sizes) out.push({ label: r.field === "logical" ? t("逻辑大小") : t("大小"), joiner: and, values: sizeRange(r) });
  for (const r of n.ages) out.push({ label: ageLabel(r.field), joiner: and, values: ageRange(r) });
  if (n.tags !== null) list(t("标签"), n.tags.length === 4 ? [t("任意")] : n.tags.map(tagColour), "t-keyword");
  list(t("排除标签"), n.not_tags.map(tagColour), "t-keyword");
  if (n.attrs) list(t("属性"), [...n.attrs.required.map(attrLabel), ...n.attrs.forbidden.map(not)], "t-value", and);
  for (const a of n.not_attrs) list(t("排除属性"), [...a.required.map(attrLabel), ...a.forbidden.map(not)], "t-value", and);
  return out;
}

function sizeRange(r: Range) {
  const side = (b: NonNullable<Range["lower"]>) => ({ text: `${b.op} ${b.number} ${b.unit === "b" ? "B" : b.unit.toUpperCase()}`, cls: "t-compare" });
  return [r.lower && side(r.lower), r.upper && side(r.upper)].filter((x): x is { text: string; cls: string } => !!x);
}

function ageRange(r: Range) {
  const out: { text: string; cls: string }[] = [];
  if (r.upper) {
    const fields = { span: timeSpan(r.upper.number, r.upper.unit) };
    out.push({ text: r.upper.op === "<=" ? t("{span}以内（含）", fields) : t("{span}以内", fields), cls: "t-compare" });
  }
  if (r.lower) {
    const fields = { span: timeSpan(r.lower.number, r.lower.unit) };
    out.push({ text: r.lower.op === ">=" ? t("早于 {span}前（含）", fields) : t("早于 {span}前", fields), cls: "t-compare" });
  }
  return out;
}

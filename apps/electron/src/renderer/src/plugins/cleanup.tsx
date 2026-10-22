// A plugin's cleanup (docs/plugins.md 「cleanup」): its candidates first come here for review, each
// with why it was proposed; only what the user leaves checked goes to the delete basket, marked with
// the cleanup it came from. Deleting is still the basket's own confirmed step: a plugin can never
// delete anything itself.

import { useEffect, useMemo, useRef, useState } from "react";

import type { CleanupItem, CleanupResult, ReportContext } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { basketAddMany } from "@/state/actions";
import { get, set, useApp } from "@/state/store";
import { fmtInt, fmtSize, middlePath } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { callPlugin } from "./client";
import { showNode } from "./nodes";
import { ProgressLine } from "./report/ReportView";
import { pageOf } from "./report/format";
import { pluginName } from "./store";

/** Rows drawn at a time; the rest is a page away. */
const PAGE = 200;

let running = 0;

/** Run a cleanup: its method with where the user is; the dialog shows progress, then the review. */
export async function startCleanup(method: string, plugin: string, ctx: ReportContext): Promise<void> {
  get().cleanup?.abort?.();
  const ac = new AbortController();
  const run = ++running;
  set({ cleanup: { plugin, scanId: ctx.scan_id ?? "", result: null, loading: true, error: "", abort: () => ac.abort(), request: null } });
  const mine = () => run === running && !ac.signal.aborted && !!get().cleanup;
  try {
    const value = await callPlugin(method, ctx, { signal: ac.signal, onId: (id) => set((s) => (s.cleanup ? { cleanup: { ...s.cleanup, request: id } } : s)) });
    if (!mine()) return;
    const result = value as CleanupResult | null;
    if (!result || result.type !== "cleanup" || !Array.isArray(result.items)) throw new Error(t("插件返回的不是清理候选"));
    reviewCleanup(plugin, ctx.scan_id ?? "", result);
  } catch (err) {
    if (!mine() || (err instanceof RpcError && err.code === -32800)) return;
    set((s) => (s.cleanup ? { cleanup: { ...s.cleanup, loading: false, error: err instanceof Error ? err.message : String(err) } } : s));
  }
}

/** Show candidates for review (a cleanup's answer, or a call action that answered with them). */
export function reviewCleanup(plugin: string, scanId: string, result: CleanupResult): void {
  set({ cleanup: { plugin, scanId, result, loading: false, error: "", abort: null, request: null } });
}

function close(): void {
  get().cleanup?.abort?.();
  set({ cleanup: null });
}

/** What the checked candidates free, how many there are. */
export function checkedTotals(items: readonly Pick<CleanupItem, "alloc">[], checked: ReadonlySet<number>): { n: number; bytes: number } {
  let bytes = 0;
  for (const i of checked) bytes += items[i]?.alloc ?? 0;
  return { n: checked.size, bytes };
}

export function CleanupDialog() {
  const c = useApp((s) => s.cleanup);
  const progress = useApp((s) => (s.cleanup?.request != null ? (s.pluginProgress[String(s.cleanup.request)] ?? null) : null));
  const items = c?.result?.items ?? EMPTY;
  const [checked, setChecked] = useState<ReadonlySet<number>>(new Set());
  const [page, setPage] = useState(0);
  const [adding, setAdding] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    setChecked(new Set(items.flatMap((it, i) => (it.checked ? [i] : []))));
    setPage(0);
  }, [items]);
  useEffect(() => {
    if (!c) return;
    box.current?.focus();
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !get().confirm) {
        e.stopPropagation();
        close();
      }
    };
    window.addEventListener("keydown", key, true);
    return () => window.removeEventListener("keydown", key, true);
  }, [c]);
  const totals = useMemo(() => checkedTotals(items, checked), [items, checked]);
  if (!c) return null;
  const r = c.result;
  const shown = pageOf(items, page, PAGE);
  const all = checked.size === items.length && items.length > 0;
  const toggle = (i: number) => {
    const next = new Set(checked);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    setChecked(next);
  };
  const add = async () => {
    if (!r || !checked.size) return;
    setAdding(true);
    const pick = [...checked].sort((a, b) => a - b).map((i) => items[i]!);
    // the basket takes up to 100 000 nodes a call
    for (let at = 0; at < pick.length; at += 100_000) {
      const part = pick.slice(at, at + 100_000);
      await basketAddMany(c.scanId || part[0]!.node.scan_id, part.map((x) => x.node.node), r.origin, part.map((x) => x.reason));
    }
    setAdding(false);
    set({ cleanup: null, basketOpen: true });
  };
  const title = r?.title || t("清理建议");
  return (
    <div className="modal-back" role="presentation" onMouseDown={close}>
      <div ref={box} tabIndex={-1} className="modal cleanup-modal" role="dialog" aria-modal="true" aria-labelledby="sk-cleanup-title" onMouseDown={(e) => e.stopPropagation()}>
        <div className="plugin-dialog-head">
          <h3 id="sk-cleanup-title">{title}</h3>
          <span className="muted small">{pluginName(c.plugin)}</span>
          <span className="flex" />
          <button className="icon-btn small" aria-label={t("关闭")} title={t("关闭")} onClick={close}>
            <Icon name="close" />
          </button>
        </div>
        {c.loading ? (
          <div className="cleanup-busy">
            {progress ? <ProgressLine done={progress.done} total={progress.total} text={progress.text} /> : <span className="muted">{t("正在找出可以清理的项…")}</span>}
          </div>
        ) : c.error ? (
          <div className="report-callout danger" role="alert">
            <Icon name="warning" size={14} />
            <span>{t("清理没有完成：{error}", { error: c.error })}</span>
          </div>
        ) : r && items.length === 0 ? (
          <div className="muted cleanup-empty">{t("没有找到可以清理的项")}</div>
        ) : r ? (
          <>
            <div className="cleanup-summary">
              <div>
                <div className="muted small">{t("勾选的项可释放")}</div>
                <div className="cleanup-big mono">{fmtSize(totals.bytes)}</div>
              </div>
              <div className="muted small">
                {t("已勾选 {n} / {total} 项", { n: fmtInt(totals.n), total: fmtInt(items.length) })}
                {r.dropped > 0 && <div>{t("另有 {n} 项已不在扫描里或不在磁盘上，没有列出", { n: fmtInt(r.dropped) })}</div>}
              </div>
            </div>
            <div className="cleanup-list" role="table" aria-label={title}>
              <div className="cleanup-row head" role="row">
                <span role="columnheader">
                  <input type="checkbox" aria-label={t("全选")} checked={all} onChange={() => setChecked(all ? new Set() : new Set(items.map((_, i) => i)))} />
                </span>
                <span role="columnheader">{t("名称")}</span>
                <span role="columnheader" className="end">
                  {t("大小")}
                </span>
                <span role="columnheader">{t("原因")}</span>
              </div>
              {shown.rows.map((it) => {
                const i = items.indexOf(it);
                return (
                  <div key={i} className={`cleanup-row${checked.has(i) ? "" : " off"}`} role="row">
                    <span role="cell">
                      <input type="checkbox" aria-label={t("选择 {name}", { name: it.name })} checked={checked.has(i)} onChange={() => toggle(i)} />
                    </span>
                    <span role="cell" className="cleanup-name">
                      <button className="report-link" title={it.path} onClick={() => void showNode(it.node)}>
                        {it.name}
                      </button>
                      <span className="mono muted small">{middlePath(it.path, 64)}</span>
                    </span>
                    <span role="cell" className="mono end">
                      {fmtSize(it.alloc)}
                    </span>
                    <span role="cell" className="cleanup-reason muted small" title={it.reason}>
                      {it.reason}
                    </span>
                  </div>
                );
              })}
            </div>
            {shown.pages > 1 && (
              <div className="report-pager small">
                <button className="icon-btn tiny" aria-label={t("上一页")} disabled={shown.page === 0} onClick={() => setPage(shown.page - 1)}>
                  <Icon name="back" size={12} />
                </button>
                <span className="mono">
                  {shown.page + 1} / {shown.pages}
                </span>
                <button className="icon-btn tiny" aria-label={t("下一页")} disabled={shown.page >= shown.pages - 1} onClick={() => setPage(shown.page + 1)}>
                  <Icon name="forward" size={12} />
                </button>
              </div>
            )}
            <p className="muted small cleanup-note">
              <Icon name="info" size={12} /> {t("加入删除篮不会删除任何文件；在删除篮里确认后才会移到回收站或永久删除。")}
            </p>
          </>
        ) : null}
        <div className="modal-buttons">
          <button className="ghost" onClick={close}>
            {t("取消")}
          </button>
          <button className="primary" disabled={!r || !checked.size || adding} onClick={() => void add()}>
            <Icon name="basket" size={14} /> {t("加入删除篮")}
          </button>
        </div>
      </div>
    </div>
  );
}

const EMPTY: CleanupItem[] = [];

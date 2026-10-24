// The run page and the cleanup history (docs/dupes.md §16.13.9, §16.10.6): a run's phase, counts
// (done, skipped, failed and put back, needing you), the item in progress, pause and 「做完当前这一项
// 就停」; afterwards what was not done and why. The history lists every run: what is still in the
// recovery area, what can be undone, what can be emptied; items that need the user get two buttons,
// neither of which deletes anything.

import { useEffect } from "react";

import type { CleanupRunDetail, CleanupRunSummary } from "@/protocol/generated";
import {
  actionLabel,
  exportPlan,
  loadRun,
  pauseTask,
  requestPurge,
  requestUndo,
  resolveItem,
  runLine,
  setCleanup,
  setView,
  stopTask,
  taskRunning,
  useCleanup,
} from "@/state/cleanup";
import { fitPath, fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

/** A time the backend wrote (ISO, UTC) in this computer's time zone. */
export const localTime = (iso: string): string => {
  const d = new Date(iso);
  if (!iso || Number.isNaN(d.getTime())) return iso.replace("T", " ").slice(0, 19);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
};

const itemState = (s: string): string =>
  ({
    pending: t("没做完"),
    done: t("完成"),
    skipped: t("跳过"),
    failed: t("出错"),
    attention: t("要你看一下"),
    undone: t("已撤销"),
    purged: t("已清除"),
    cancelled: t("没有开始"),
  })[s] ?? s;

/** Item states whose text is a reason worth showing (an undone item keeps its old result: not shown). */
const REASONED = new Set(["skipped", "failed", "attention", "cancelled", "pending"]);

const stateLabel = (s: string): string =>
  ({
    waiting: t("等待中"),
    preflight: t("预检中"),
    running: t("执行中"),
    paused: t("已暂停"),
    done: t("完成"),
    partial: t("部分完成"),
    stopped: t("停下了"),
    failed: t("出错停下"),
    undone: t("已撤销"),
    recovered: t("中断后已收尾"),
  })[s] ?? s;

export function RunView() {
  const task = useCleanup((s) => s.task);
  const run = useCleanup((s) => s.run);
  const running = taskRunning(task);
  const runId = task?.run_id ?? "";
  // once it ends (or pauses): every item and what became of it
  useEffect(() => {
    if (runId && (!running || task?.state === "paused")) void loadRun(runId, "");
  }, [runId, running, task?.state]);
  if (!task) return <div className="muted small cl-empty">{t("还没有执行")}</div>;
  const mine = run && run.run.run_id === runId ? run : null;
  if (task.needs_confirm) return <HeldRun />;
  const pct = task.total ? Math.round((100 * task.done) / task.total) : 0;
  return (
    <div className="cl-runview">
      <h3>
        {stateLabel(task.state)} <span className="muted small">{task.run_id}</span>
      </h3>
      {task.kind !== "execute" && <div className="small muted">{{ preflight: t("只预检：不写任何东西"), undo: t("撤销这次清理"), purge: t("清除恢复区"), facts: "", recover: t("中断后收尾") }[task.kind]}</div>}
      <div className="cl-phases small" aria-label={t("阶段")}>
        {["preflight", "execute", "done"].map((p) => (
          <span key={p} className={`cl-phase${task.phase === p || (p === "done" && !running) ? " on" : ""}`}>
            {{ preflight: t("预检"), execute: t("逐项处理"), done: t("收尾") }[p]}
          </span>
        ))}
      </div>
      <div className="cl-bar" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
        <span style={{ width: `${pct}%` }} />
      </div>
      <div className="small">
        {t("{done} / {total} 项", { done: fmtInt(task.done), total: fmtInt(task.total) })}
        {task.message && <span className="muted"> · {task.message}</span>}
      </div>
      {running && task.state !== "paused" && (
        <div className="mono small muted cl-current" title={task.current}>
          {task.current ? t("正在处理：{path}", { path: fitPath(task.current, 90) }) : t("准备中…")}
        </div>
      )}
      <div className="cl-counts small">
        {Object.entries(task.counts).map(([k, v]) => (
          <span key={k} className={`cl-count ${k.split("_")[0]}`}>
            {codeLabel(k)} {fmtInt(v)}
          </span>
        ))}
      </div>
      {running && (
        <div className="modal-buttons">
          {task.state === "paused" ? (
            <button className="ghost" onClick={() => void pauseTask(true)}>
              <Icon name="play" size={13} /> {t("继续")}
            </button>
          ) : (
            <button className="ghost" onClick={() => void pauseTask(false)}>
              <Icon name="pause" size={13} /> {t("暂停")}
            </button>
          )}
          <button className="ghost" onClick={() => void stopTask()}>
            <Icon name="stop" size={13} /> {t("做完当前这一项就停")}
          </button>
        </div>
      )}
      {!running && (
        <>
          <div className="small strong">{runLine(task)}</div>
          {task.skipped.map((s) => (
            <details key={s.code} className="small">
              <summary>
                {s.text} · {fmtInt(s.count)}
              </summary>
              {s.examples.map((e) => (
                <div key={e} className="mono muted">
                  {e}
                </div>
              ))}
            </details>
          ))}
          {task.run_id && (
            <div className="modal-buttons">
              <button className="ghost" onClick={() => void exportPlan("json", task.run_id)}>
                {t("导出报告")}
              </button>
              <button className="ghost" disabled={!!mine && !mine.run.can_undo} onClick={() => void requestUndo(task.run_id)}>
                {t("撤销这次清理")}
              </button>
              <button className="ghost" disabled={!!mine && !mine.run.can_purge} title={mine && !mine.run.can_purge ? t("恢复区里没有这次的原件") : undefined} onClick={() => void requestPurge(task.run_id)}>
                {t("清除恢复区…")}
              </button>
            </div>
          )}
        </>
      )}
      {mine && mine.items.length > 0 && (
        <section className="cl-run-items">
          <div className="small muted">{t("逐项结果")}</div>
          <RunItems run={mine} />
        </section>
      )}
    </div>
  );
}

/** A run the preflight held: some steps can no longer be done; nothing was changed. */
function HeldRun() {
  const task = useCleanup((s) => s.task)!;
  const dropped = task.skipped.reduce((n, s) => n + s.count, 0);
  return (
    <div className="cl-runview">
      <h3>{t("等你确认")}</h3>
      <div className="small">
        {t("预检时有 {n} 项不能做（核对之后变了），其余 {m} 项还没有开始，没有改动任何文件", { n: fmtInt(dropped), m: fmtInt(Math.max(0, task.total - dropped)) })}
      </div>
      {task.skipped.map((s) => (
        <div key={s.code} className="small muted">
          · {s.text} · {fmtInt(s.count)}
        </div>
      ))}
      <div className="modal-buttons">
        <button className="ghost" onClick={() => void setView("review")}>
          {t("回到审阅")}
        </button>
        <button className="primary" onClick={() => setCleanup({ dialog: { kind: "drops", task } })}>
          {t("看清单，执行其余的…")}
        </button>
      </div>
    </div>
  );
}

/** Every step of a run: what it was, on which file, what became of it; the two buttons of an item
 * that needs the user (neither deletes anything). */
function RunItems({ run }: { run: CleanupRunDetail }) {
  return (
    <div className="cl-items" role="list">
      {run.items.map((it) => (
        <div key={it.seq} className={`cl-item ${it.state}`} role="listitem">
          <span className="mono muted">{it.seq}</span>
          <span className="small">{actionLabel(it.action)}</span>
          <span className="mono small cl-item-path" title={it.target ? `${it.path} → ${it.target}` : it.path}>
            {fitPath(it.path, 60)}
          </span>
          <span className="small">
            <span className={`cl-state ${it.state}`}>{itemState(it.state)}</span>
            {it.text && it.text !== itemState(it.state) && (REASONED.has(it.state) || (it.state === "done" && it.code !== "done")) && <span className="muted"> · {it.text}</span>}
          </span>
          {it.state === "attention" && (
            <span className="cl-resolve">
              <button className="ghost tiny" onClick={() => void resolveItem(run.run.run_id, it.seq, "put_back")}>
                {t("把原件放回原处")}
              </button>
              <button className="ghost tiny" onClick={() => void resolveItem(run.run.run_id, it.seq, "keep_as_is")}>
                {t("保留现状")}
              </button>
            </span>
          )}
        </div>
      ))}
      {run.total > run.items.length && <div className="muted small cl-empty">{t("只列出前 {n} 项；导出报告看全部", { n: fmtInt(run.items.length) })}</div>}
    </div>
  );
}

export const codeLabel = (code: string): string =>
  ({
    done: t("完成"),
    done_partial: t("部分共用"),
    would_do: t("可以做"),
    cancelled: t("没有开始"),
    failed_rolled_back: t("出错已回到原样"),
    failed_attention: t("要你看一下"),
    skipped_changed: t("变了"),
    skipped_keeper_changed: t("保留的那份变了"),
    skipped_keeper_lost: t("保留的那份不在了"),
    skipped_keeper_busy: t("保留的那份正被打开"),
    skipped_locked: t("正被打开"),
    skipped_denied: t("没有权限"),
    skipped_missing: t("已经不在"),
    skipped_unavailable: t("做不了"),
    skipped_in_basket: t("在删除篮里"),
    skipped_name_taken: t("名字被占"),
    skipped_raced: t("别的程序动了它"),
    skipped_trash_full: t("回收站放不下"),
    skipped_error: t("出错没动"),
    undone: t("已撤销"),
    undo_taken: t("原处已有东西，没有撤销"),
    undo_changed: t("清理之后改过，没有撤销"),
    undo_beside: t("原件还原到了旁边"),
    undo_nothing: t("块克隆不用撤销"),
    purged: t("已清除"),
    refused: t("没有清除"),
  })[code] ?? code;

/** What the last undo or emptying of this run did (while it runs, and after). */
function TaskLine({ runId }: { runId: string }) {
  const task = useCleanup((s) => s.task);
  if (!task || task.run_id !== runId || (task.kind !== "undo" && task.kind !== "purge")) return null;
  const what = task.kind === "undo" ? t("撤销") : t("清除恢复区");
  const running = taskRunning(task);
  return (
    <div className="cl-taskline small" role="status">
      <span className="strong">{what}</span>
      {task.state === "waiting" ? (
        <span className="muted">{t("几秒后开始，这期间可以取消")}</span>
      ) : running ? (
        <span className="muted">{t("{done} / {total} 项", { done: fmtInt(task.done), total: fmtInt(task.total) })}</span>
      ) : (
        <span>
          {Object.entries(task.counts)
            .map(([k, v]) => `${codeLabel(k)} ${fmtInt(v)}`)
            .join(" · ") || t("没有要做的")}
        </span>
      )}
      {!running &&
        task.skipped.map((s) => (
          <span key={s.code} className="warn">
            {s.text} · {fmtInt(s.count)}
          </span>
        ))}
    </div>
  );
}

export function History() {
  const runs = useCleanup((s) => s.runs);
  const run = useCleanup((s) => s.run);
  const filter = useCleanup((s) => s.runFilter);
  return (
    <div className="cl-history">
      <div className="cl-runs" role="list" aria-label={t("清理记录")}>
        {!runs.length && <div className="muted small cl-empty">{t("还没有清理过重复文件")}</div>}
        {runs.map((r) => (
          <RunRow key={r.run_id} r={r} on={run?.run.run_id === r.run_id} />
        ))}
      </div>
      {run && (
        <div className="cl-run-detail">
          <div className="cl-run-head">
            <span className="strong">{run.run.run_id}</span>
            <span className="muted small">{stateLabel(run.run.state)}</span>
            <span className="flex" />
            <select aria-label={t("按结果筛选")} value={filter} onChange={(e) => void loadRun(run.run.run_id, e.target.value)}>
              <option value="">{t("全部")}</option>
              {["done", "skipped", "failed", "attention", "undone", "purged", "cancelled"].map((f) => (
                <option key={f} value={f}>
                  {({ done: t("完成"), skipped: t("跳过"), failed: t("出错"), attention: t("要你看一下"), undone: t("已撤销"), purged: t("已清除"), cancelled: t("没有开始") } as Record<string, string>)[f]}
                </option>
              ))}
            </select>
            <button className="ghost tiny" onClick={() => void exportPlan("csv", run.run.run_id)}>
              {t("导出 CSV")}
            </button>
          </div>
          <TaskLine runId={run.run.run_id} />
          <RunItems run={run} />
        </div>
      )}
    </div>
  );
}

function RunRow({ r, on }: { r: CleanupRunSummary; on: boolean }) {
  const acts = Object.entries(r.by_action)
    .map(([a, n]) => `${actionLabel(a)} ${fmtInt(n)}`)
    .join(" · ");
  return (
    <div className={`cl-runrow${on ? " on" : ""}`} role="listitem">
      <button className="cl-runrow-main" onClick={() => void loadRun(r.run_id)}>
        <span className="strong small">{localTime(r.started)}</span>
        <span className="small muted">{stateLabel(r.state)}</span>
        <span className="small">{acts || t("没有改动任何文件")}</span>
        <span className="small muted" title={r.roots.join("\n")}>
          {r.roots.map((x) => fitPath(x, 48)).join(" · ")}
        </span>
      </button>
      <div className="small">
        {r.held > 0 && <span>{t("恢复区里有 {size}", { size: fmtSize(r.held) })} </span>}
        {r.in_trash > 0 && <span className="muted">{t("回收站里 {n} 份要清空回收站才腾出（SpaceKnife 不替你清空回收站）", { n: fmtInt(r.in_trash) })} </span>}
        {r.attention > 0 && <span className="warn">{t("{n} 项要你看一下", { n: fmtInt(r.attention) })} </span>}
        {r.purge_reminder && <span className="warn">{t("原件放在恢复区已经好几天了：确认不再需要时可以清除")}</span>}
      </div>
      <div className="cl-runrow-buttons">
        <button className="ghost tiny" disabled={!r.can_undo} onClick={() => void requestUndo(r.run_id)}>
          {t("撤销")}
        </button>
        <button className="ghost tiny" disabled={!r.can_purge} onClick={() => void requestPurge(r.run_id)}>
          {t("清除恢复区…")}
        </button>
      </div>
    </div>
  );
}

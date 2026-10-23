// The review page (docs/dupes.md §16.7.2): per action, per volume (what is freed at once, and once
// the recycle bin or the recovery area is emptied), how contents are checked before running, what
// happens when an item fails, the risks by kind (high ones acknowledged for the copies listed),
// what is not done and why, the gates still closed, a spot check, exports; then approve, preflight,
// and 「执行…」 (a confirmation follows). This page is the only place that runs a plan.

import type { CleanupBucket } from "@/protocol/generated";
import {
  ack,
  actionLabel,
  approve,
  edit,
  exportPlan,
  inspect,
  openRisk,
  preflightOnly,
  requestExecute,
  setFilter,
  setOptions,
  setView,
  taskRunning,
  unack,
  unackedBuckets,
  useCleanup,
} from "@/state/cleanup";
import { call } from "@/state/actions";
import { fmtInt, fmtSize, plural } from "@/lib/format";
import { t } from "@/lib/i18n";

import { Sev } from "./parts";

export function ReviewPage() {
  const st = useCleanup((s) => s.state);
  const task = useCleanup((s) => s.task);
  const riskOpen = useCleanup((s) => s.riskOpen);
  if (!st) return null;
  const sm = st.summary;
  const high = sm.buckets.filter((b) => b.severity === "high");
  const medium = sm.buckets.filter((b) => b.severity === "medium");
  const rest = sm.buckets.filter((b) => b.severity === "low" || b.severity === "info");
  const approved = !!st.approval && st.approval.version === st.version;
  const gates = sm.gates;
  const blocked = gates.length > 0 || unackedBuckets(st).length > 0;
  const running = taskRunning(task);
  return (
    <div className="cl-review">
      <h3>
        {t("审阅清理方案 · 第 {v} 版", { v: fmtInt(st.version) })}
        <span className="muted small">
          {" "}
          {st.facts_done < st.facts_total ? t("风险核查 {pct}%", { pct: Math.floor((100 * st.facts_done) / Math.max(1, st.facts_total)) }) : t("风险核查完成")}
        </span>
      </h3>
      <table className="cl-sum">
        <thead>
          <tr>
            <th>{t("处理")}</th>
            <th>{t("份")}</th>
            <th>{t("组")}</th>
            <th>{t("马上腾出")}</th>
            <th>{t("清空或清除之后")}</th>
          </tr>
        </thead>
        <tbody>
          {sm.by_action.map((a) => (
            <tr key={a.action}>
              <td>{actionLabel(a.action)}</td>
              <td className="mono">{fmtInt(a.copies)}</td>
              <td className="mono">{fmtInt(a.groups)}</td>
              <td className="mono">{a.freed_now ? fmtSize(a.freed_now) : "—"}</td>
              <td className="mono">
                {a.freed_later ? fmtSize(a.freed_later) : "—"}
                {a.freed_later > 0 && <span className="muted small"> {a.action === "trash" ? t("（清空回收站后）") : t("（原件在恢复区，清除后）")}</span>}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="small cl-vols">
        {sm.by_volume.map((v) => (
          <div key={v.volume}>
            <span className="strong">{v.volume}</span> {v.fs.toUpperCase()} ·{" "}
            {v.freed_now ? t("马上 {size}", { size: fmtSize(v.freed_now) }) : ""} {v.freed_later ? t("之后 +{size}", { size: fmtSize(v.freed_later) }) : ""}
            {v.recovery && <span className="muted"> · {t("恢复区 {path}", { path: v.recovery })}</span>}
            {v.trash && <span className="warn"> · {v.trash}</span>}
            {v.snapshots_note && <span className="muted"> · {v.snapshots_note}</span>}
          </div>
        ))}
      </div>
      <div className="cl-opts small">
        <div role="radiogroup" aria-label={t("执行前核对内容")}>
          <span className="muted">{t("执行前核对内容：")}</span>
          {(["auto", "content", "metadata"] as const).map((v) => (
            <label key={v}>
              <input type="radio" name="verify" checked={st.options.verify === v} onChange={() => void setOptions({ verify: v })} />
              {{ auto: t("自动（时间戳证明不了没变过时重读）"), content: t("全部重读"), metadata: t("只核对文件信息（▲ 高风险）") }[v]}
            </label>
          ))}
          {sm.compare_bytes > 0 && <span className="muted"> · {t("块克隆前逐段比较，要读 {size}", { size: fmtSize(sm.compare_bytes) })}</span>}
        </div>
        <div role="radiogroup" aria-label={t("出错时")}>
          <span className="muted">{t("出错时：")}</span>
          {(["continue", "stop"] as const).map((v) => (
            <label key={v}>
              <input type="radio" name="onfail" checked={st.options.on_failure === v} onChange={() => void setOptions({ on_failure: v })} />
              {v === "continue" ? t("这一项回到原样，继续下一项") : t("整次停下")}
            </label>
          ))}
        </div>
      </div>
      {high.length > 0 && (
        <section className="cl-bucket-sec">
          <h4>{t("要你确认（高）")}</h4>
          {high.map((b) => (
            <Bucket key={b.id} b={b} open={riskOpen?.risk.id === b.id} needsAck />
          ))}
        </section>
      )}
      {medium.length > 0 && (
        <section className="cl-bucket-sec">
          <h4>{t("值得看一眼（中）")}</h4>
          {medium.map((b) => (
            <Bucket key={b.id} b={b} open={riskOpen?.risk.id === b.id} />
          ))}
        </section>
      )}
      {rest.length > 0 && (
        <details className="cl-bucket-sec">
          <summary>
            <h4>{t("低和提示：{n} 份", { n: fmtInt(rest.reduce((n, b) => n + b.copies, 0)) })}</h4>
          </summary>
          {rest.map((b) => (
            <Bucket key={b.id} b={b} open={riskOpen?.risk.id === b.id} />
          ))}
        </details>
      )}
      {sm.not_done.length > 0 && (
        <section className="cl-bucket-sec">
          <h4>{t("不做的和兜底的")}</h4>
          <div className="small">
            {sm.not_done.map((x) => (
              <span key={x.reason} className="cl-notdone">
                {x.text} {plural(x.count, x.unit === "groups" ? t("{n} 组", { n: fmtInt(x.count) }) : t("{n} 份", { n: fmtInt(x.count) }), x.unit === "groups" ? "1 group" : "1 copy")}
              </span>
            ))}
          </div>
        </section>
      )}
      <div className="cl-spot small">
        <button className="ghost tiny" onClick={() => void spotCheck()}>
          {t("抽查：每类随机 10 组")}
        </button>
        <span className="muted">{t("已过目 {n} / {total} 组", { n: fmtInt(sm.reviewed), total: fmtInt(sm.in_plan) })}</span>
        <span className="flex" />
        <button className="ghost tiny" onClick={() => void exportPlan("json")}>
          {t("导出 JSON")}
        </button>
        <button className="ghost tiny" onClick={() => void exportPlan("csv")}>
          {t("导出 CSV")}
        </button>
      </div>
      <div className="muted small">{t("不提供「导出成脚本」：脚本没有执行前的核对，导出去就没有了零误报和原子性的保证")}</div>
      {gates.length > 0 && (
        <div className="cl-gates warn small" role="status">
          {gates.map((g) => (
            <div key={g}>· {g}</div>
          ))}
        </div>
      )}
      <div className="cl-run-buttons">
        <span className="muted small">
          {approved ? t("已批准：{n} 步（之后再改方案，批准就失效）", { n: fmtInt(st.approval!.steps) }) : t("还没有批准")}
        </span>
        <span className="flex" />
        <button className="ghost" disabled={blocked || running} onClick={() => void preflightOnly()}>
          {t("只预检")}
        </button>
        <button className="ghost" disabled={blocked || approved || running} onClick={() => void approve()}>
          {t("批准")}
        </button>
        <button className="primary cl-exec" disabled={blocked || running} onClick={() => requestExecute()}>
          {t("执行…")}
        </button>
      </div>
      {task && task.kind === "preflight" && (
        <div className="small cl-preflight" role="status">
          {taskRunning(task) ? t("预检中…") : t("预检：{n} 项可以做", { n: fmtInt(task.counts.would_do ?? 0) })}
          {task.skipped.map((s) => (
            <div key={s.code} className="muted">
              · {s.text} {fmtInt(s.count)}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function Bucket({ b, open, needsAck }: { b: CleanupBucket; open: boolean; needsAck?: boolean }) {
  const copies = useCleanup((s) => (open ? s.riskOpen : null));
  return (
    <div className={`cl-bucket ${b.severity}`}>
      <div className="cl-bucket-head">
        <Sev s={b.severity} />
        <span className="strong">{b.title}</span>
        <span className="small muted">
          {t("{n} 份 · {g} 组 · {size}", { n: fmtInt(b.copies), g: fmtInt(b.groups), size: fmtSize(b.bytes) })}
        </span>
        <span className="flex" />
        <button className="ghost tiny" onClick={() => void openRisk(open ? null : b.id)}>
          {open ? t("收起") : t("看这些")}
        </button>
        <button className="ghost tiny" onClick={() => void showInTable(b.id)}>
          {t("到组表里看")}
        </button>
        <button className="ghost tiny" onClick={() => void edit({ op: "action_set", action: "leave", scope: { filter: `risk:${b.id}` } }, t("这些改成不动"))}>
          {t("改成不动")}
        </button>
      </div>
      <div className="small">{b.example}</div>
      {open && copies && (
        <div className="cl-bucket-list small">
          {copies.items.map((c) => (
            <div key={c.row} className="cl-bucket-item">
              <span className="mono" title={c.path}>
                {c.path}
              </span>
              <span className="muted"> — {c.risks.find((r) => r.id === b.id)?.text ?? ""}</span>
            </div>
          ))}
          {copies.total > copies.items.length && <div className="muted">{t("另有 {n} 份（在组表里筛 risk:{id} 看全部）", { n: fmtInt(copies.total - copies.items.length), id: b.id })}</div>}
          {needsAck && (
            <label className="cl-ack">
              <input type="checkbox" checked={b.acked} onChange={(e) => void (e.target.checked ? ack(b.id) : unack(b.id))} />
              {t("我知道了，照这样做（{n} 份）", { n: fmtInt(b.copies) })}
            </label>
          )}
        </div>
      )}
      {needsAck && !open && (
        <div className="small muted">
          {b.acked ? t("已确认") : b.unacked < b.copies && b.unacked > 0 ? t("又有 {n} 份落进这类风险，要再确认", { n: fmtInt(b.unacked) }) : t("点「看这些」，看过之后才能确认")}
        </div>
      )}
    </div>
  );
}

async function showInTable(risk: string): Promise<void> {
  setFilter(`risk:${risk}`);
  await setView("groups");
}

async function spotCheck(): Promise<void> {
  const id = useCleanup.getState().planId;
  if (!id) return;
  const s = await call("dupes.cleanup_sample", { plan_id: id, per_bucket: 10 });
  if (!s.groups.length) return;
  await setView("groups");
  await inspect(s.groups[0]!);
}

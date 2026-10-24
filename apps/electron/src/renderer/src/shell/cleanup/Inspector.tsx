// The inspector of cleanup mode (docs/dupes.md §16.13.7): the group's head line; per copy the
// facts beside the keeper's (the cells that differ marked), what can be done and why not, the risks
// with their evidence, and the full reason chain of its decision.

import { useEffect, useRef } from "react";

import type { CleanupCopyDetail } from "@/protocol/generated";
import { actionLabel, ACTIONS, setAction, useCleanup } from "@/state/cleanup";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";

import { Sev } from "./parts";

export function CleanupInspector() {
  const d = useCleanup((s) => s.detail);
  const ref = useRef<HTMLElement>(null);
  const gid = d?.group.id ?? null;
  // another group starts at its top
  useEffect(() => {
    if (ref.current) ref.current.scrollTop = 0;
  }, [gid]);
  if (!d) return <aside className="cl-inspector muted small">{t("选一组看它的每一份")}</aside>;
  const g = d.group;
  return (
    <aside ref={ref} className="cl-inspector" aria-label={t("检查器")} tabIndex={-1}>
      <div className="cl-ins-head">
        <div className="strong">{g.name}</div>
        <div className="muted small">
          {t("{n} 份 · 每份 {size}", { n: g.copies, size: fmtSize(g.size) })} · <span className="mono">{g.digest16}</span> · {t("内容相同（已读完全文核对）")}
        </div>
        <div className="small">{g.keep_line}</div>
        {d.out_reason && <div className="warn small">{d.out_reason}</div>}
      </div>
      {d.copies.map((c) => (
        <CopyCard key={c.item.row} c={c} group={g.id} live={g.state === "in"} />
      ))}
    </aside>
  );
}

function CopyCard({ c, group, live }: { c: CleanupCopyDetail; group: number; live: boolean }) {
  const m = c.item;
  const kept = m.keep !== "none";
  return (
    <section className={`cl-card${kept ? " kept" : ""}`}>
      <div className="cl-card-head">
        <span className="strong">{kept ? (m.keep === "locked" ? t("必须保留") : t("保留")) : actionLabel(m.action ?? "leave")}</span>
        <Sev s={m.severity} />
        <span className="flex" />
        <span className="muted small">{m.volume}</span>
      </div>
      <div className="mono small cl-card-path" title={m.path}>
        {m.path}
      </div>
      {m.view_of && <div className="muted small">{t("和 {path} 是同一个文件", { path: m.view_of })}</div>}
      {c.facts.length > 0 && (
        <table className="cl-facts">
          <thead>
            <tr>
              <th />
              <th>{t("这份")}</th>
              <th>{t("保留那份")}</th>
            </tr>
          </thead>
          <tbody>
            {c.facts.map((f) => (
              <tr key={f.label} className={f.differs ? "differs" : ""}>
                <th>{f.label}</th>
                <td className="mono" title={f.this}>
                  {f.this}
                </td>
                <td className="mono" title={f.keeper}>
                  {f.keeper}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {!kept && !m.view_of && (
        <div className="cl-avail">
          <div className="small muted">{t("能做的处理")}</div>
          {ACTIONS.map((a) => {
            const why = m.available[a] ?? "";
            return (
              <label key={a} className={`cl-avail-row${why ? " no" : ""}`}>
                <input type="radio" name={`act-${m.row}`} checked={(m.action ?? "leave") === a} disabled={!live || !!why} onChange={() => void setAction(group, m.path, a)} />
                <span className="cl-avail-text">
                  <span>{actionLabel(a)}</span>
                  {why && <span className="cl-pick-why">{why}</span>}
                </span>
              </label>
            );
          })}
        </div>
      )}
      {m.risks.length > 0 && (
        <div className="cl-risks">
          {m.risks.map((r) => (
            <div key={r.id} className={`cl-risk-line ${r.severity}`}>
              <Sev s={r.severity} /> <span className="strong">{r.title}</span>
              {r.acked && <span className="cl-tag">{t("已确认")}</span>}
              <div className="small">{r.text}</div>
            </div>
          ))}
        </div>
      )}
      <details className="cl-chain">
        <summary className="small">{t("为什么这样决定")}</summary>
        <ol className="small">
          {[...c.keep_chain, ...c.action_chain].map((x, i) => (
            <li key={i} className={x.decided ? "decided" : ""}>
              <span className="muted">{t("{layer}：", { layer: x.layer })}</span>
              {x.text}
            </li>
          ))}
        </ol>
      </details>
    </section>
  );
}

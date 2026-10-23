// Small pieces of the cleanup mode shared by its pages: severity marks, risk chips, the action
// picker (only what can be done here; the rest greyed with the reason), sizes.

import type { CleanupCopy, CleanupRisk } from "@/protocol/generated";
import { ACTIONS, actionLabel, type CleanupActionName, sevLabel, sevMark } from "@/state/cleanup";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";

export function Sev({ s, n }: { s: string; n?: number }) {
  if (!s || s === "none") return null;
  return (
    <span className={`cl-sev ${s}`} title={sevLabel(s)} aria-label={sevLabel(s)}>
      {sevMark(s)}
      {n !== undefined && n > 0 ? <span className="mono"> {n}</span> : null}
    </span>
  );
}

export function RiskChip({ r }: { r: CleanupRisk }) {
  return (
    <span className={`cl-risk ${r.severity}${r.acked ? " acked" : ""}`} title={`${sevLabel(r.severity)} · ${r.text}`}>
      {sevMark(r.severity)} {r.title}
    </span>
  );
}

/** The action of a copy not kept: a select whose impossible choices are disabled, their reason in
 * the option's text (a select's options take no tooltip in every browser). */
export function ActionPicker({ copy, onPick, disabled }: { copy: CleanupCopy; onPick: (a: CleanupActionName) => void; disabled?: boolean }) {
  const cur = copy.action ?? "leave";
  return (
    <select
      className="cl-act"
      aria-label={t("处理")}
      value={cur}
      disabled={disabled}
      onChange={(e) => onPick(e.target.value as CleanupActionName)}
      title={copy.source}
    >
      {ACTIONS.map((a) => {
        const why = copy.available[a] ?? "";
        return (
          <option key={a} value={a} disabled={!!why && a !== cur}>
            {actionLabel(a)}
            {why ? ` — ${why}` : ""}
          </option>
        );
      })}
    </select>
  );
}

export function Freed({ now, later }: { now: number; later: number }) {
  if (!now && !later) return <span className="muted">—</span>;
  return (
    <span className="mono" title={t("马上 {now} · 清空回收站或清除恢复区之后 {later}", { now: fmtSize(now), later: fmtSize(later) })}>
      {fmtSize(now + later)}
    </span>
  );
}

/** A dialog shell (the app's .modal look); the first button gets the focus. */
export function Modal({ title, children, wide }: { title: string; children: React.ReactNode; wide?: boolean }) {
  return (
    <div className="modal-back" role="presentation">
      <div className={`modal cl-modal${wide ? " wide" : ""}`} role="dialog" aria-modal="true" aria-label={title}>
        <h3>{title}</h3>
        {children}
      </div>
    </div>
  );
}

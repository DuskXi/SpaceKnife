// Small pieces of the cleanup mode shared by its pages: severity marks, risk chips, the action
// picker (only what can be done here; the rest greyed with the reason), sizes.

import { useEffect, useLayoutEffect, useRef, useState } from "react";

import type { CleanupCopy, CleanupRisk } from "@/protocol/generated";
import { ACTIONS, actionLabel, type CleanupActionName, sevLabel, sevMark } from "@/state/cleanup";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

/** A severity: its mark (never colour alone), the word when `word` is set, a count. */
export function Sev({ s, n, word }: { s: string; n?: number; word?: boolean }) {
  if (!s || s === "none") return null;
  return (
    <span className={`cl-sev ${s}`} title={sevLabel(s)} aria-label={n !== undefined ? t("{sev} {n} 份", { sev: sevLabel(s), n }) : sevLabel(s)}>
      {sevMark(s)}
      {word && <span> {sevLabel(s)}</span>}
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

/** A copy's risks in a row: the first few as chips, the rest counted (all of them in the tooltip). */
export function RiskChips({ risks, max = 2 }: { risks: CleanupRisk[]; max?: number }) {
  const shown = risks.filter((r) => r.severity !== "info");
  const rest = shown.slice(max);
  return (
    <>
      {shown.slice(0, max).map((r) => (
        <RiskChip key={r.id} r={r} />
      ))}
      {rest.length > 0 && (
        <span className="cl-risk more" title={rest.map((r) => `${sevLabel(r.severity)} · ${r.title}`).join("\n")}>
          +{rest.length}
        </span>
      )}
    </>
  );
}

export interface PickerChoice {
  action: CleanupActionName;
  label: string;
  /** why it cannot be done here ("" when it can) */
  why: string;
  disabled: boolean;
}

/** The choices of a copy's action: every action, the impossible ones with their reason. */
export function pickerChoices(copy: CleanupCopy): PickerChoice[] {
  const cur = copy.action ?? "leave";
  return ACTIONS.map((a) => {
    const why = copy.available[a] ?? "";
    return { action: a, label: actionLabel(a), why, disabled: !!why && a !== cur };
  });
}

/** The action of a copy not kept: a button that opens a list of the five actions; what cannot be
 * done here stays in the list, greyed, with the reason under it. Keyboard: ↑ ↓ Home End move,
 * Enter or Space picks, Esc closes. */
export function ActionPicker({ copy, onPick, disabled, initialOpen = false }: { copy: CleanupCopy; onPick: (a: CleanupActionName) => void; disabled?: boolean; initialOpen?: boolean }) {
  const cur = copy.action ?? "leave";
  const [open, setOpen] = useState(initialOpen);
  const btn = useRef<HTMLButtonElement>(null);
  const choices = pickerChoices(copy);
  const close = (focus = true) => {
    setOpen(false);
    if (focus) btn.current?.focus();
  };
  return (
    <span className="cl-picker">
      <button
        ref={btn}
        type="button"
        className="cl-act"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={t("处理：{act}", { act: actionLabel(cur) })}
        title={copy.source}
        disabled={disabled}
        onClick={(e) => {
          e.stopPropagation();
          setOpen(!open);
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            e.stopPropagation();
            setOpen(true);
          }
        }}
      >
        <span className="cl-act-label">{actionLabel(cur)}</span>
        <Icon name="chevronDown" size={12} />
      </button>
      {open && (
        <PickerMenu
          anchor={btn}
          choices={choices}
          current={cur}
          onClose={close}
          onPick={(a) => {
            close();
            if (a !== cur) onPick(a);
          }}
        />
      )}
    </span>
  );
}

function PickerMenu(props: { anchor: React.RefObject<HTMLButtonElement | null>; choices: PickerChoice[]; current: string; onClose: (focus?: boolean) => void; onPick: (a: CleanupActionName) => void }) {
  const { anchor, choices, current, onClose, onPick } = props;
  const menu = useRef<HTMLDivElement>(null);
  const enabled = choices.filter((c) => !c.disabled).map((c) => c.action);
  const [active, setActive] = useState<string>(enabled.includes(current as CleanupActionName) ? current : (enabled[0] ?? current));
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  // placed under the button (above it when there is no room), inside the window
  useLayoutEffect(() => {
    const b = anchor.current?.getBoundingClientRect();
    const m = menu.current?.getBoundingClientRect();
    if (!b || !m) return;
    const below = b.bottom + 4 + m.height <= window.innerHeight - 8;
    setPos({ left: Math.max(8, Math.min(b.left, window.innerWidth - m.width - 8)), top: below ? b.bottom + 4 : Math.max(8, b.top - 4 - m.height) });
    menu.current?.focus();
  }, [anchor]);
  // a click elsewhere, a scroll or the window losing the focus closes it
  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (!menu.current?.contains(e.target as Node) && !anchor.current?.contains(e.target as Node)) onClose(false);
    };
    const scroll = (e: Event) => {
      if (!menu.current?.contains(e.target as Node)) onClose(false);
    };
    const blur = () => onClose(false);
    document.addEventListener("mousedown", down, true);
    window.addEventListener("scroll", scroll, true);
    window.addEventListener("blur", blur);
    return () => {
      document.removeEventListener("mousedown", down, true);
      window.removeEventListener("scroll", scroll, true);
      window.removeEventListener("blur", blur);
    };
  }, [anchor, onClose]);
  const move = (d: number | "first" | "last") => {
    if (!enabled.length) return;
    const i = enabled.indexOf(active as CleanupActionName);
    const j = d === "first" ? 0 : d === "last" ? enabled.length - 1 : (i + d + enabled.length) % enabled.length;
    setActive(enabled[j]!);
  };
  return (
    <div
      ref={menu}
      className="cl-picker-menu"
      role="listbox"
      tabIndex={-1}
      aria-label={t("处理")}
      aria-activedescendant={`cl-pick-${active}`}
      style={pos ? { left: pos.left, top: pos.top } : { visibility: "hidden" }}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "ArrowDown") move(1);
        else if (e.key === "ArrowUp") move(-1);
        else if (e.key === "Home") move("first");
        else if (e.key === "End") move("last");
        else if (e.key === "Enter" || e.key === " ") onPick(active as CleanupActionName);
        else if (e.key === "Escape") onClose();
        else if (e.key === "Tab") onClose(false);
        else return;
        e.preventDefault();
      }}
    >
      {choices.map((c) => (
        <div
          key={c.action}
          id={`cl-pick-${c.action}`}
          role="option"
          aria-selected={c.action === current}
          aria-disabled={c.disabled || undefined}
          className={`cl-pick${c.disabled ? " no" : ""}${c.action === active ? " active" : ""}`}
          onMouseEnter={() => !c.disabled && setActive(c.action)}
          onClick={() => !c.disabled && onPick(c.action)}
        >
          <span className="cl-pick-check">{c.action === current ? <Icon name="check" size={12} /> : null}</span>
          <span className="cl-pick-text">
            <span className="cl-pick-label">{c.label}</span>
            {c.why && <span className="cl-pick-why">{c.why}</span>}
          </span>
        </div>
      ))}
    </div>
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

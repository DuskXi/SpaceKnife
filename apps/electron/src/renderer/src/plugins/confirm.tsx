// A question with two answers, shown over everything (opening a plugin's web page, uninstalling a
// plugin): `await ask({...})` is true when the user chose the first button.

import { useEffect, useRef } from "react";

import { type ConfirmAsk, get, set, useApp } from "@/state/store";
import { t } from "@/lib/i18n";

/** Ask; a second question while one is open answers the first with no. */
export function ask(q: Omit<ConfirmAsk, "answer">): Promise<boolean> {
  return new Promise((resolve) => {
    get().confirm?.answer(false);
    set({
      confirm: {
        ...q,
        answer: (yes) => {
          set({ confirm: null });
          resolve(yes);
        },
      },
    });
  });
}

export function ConfirmDialog() {
  const q = useApp((s) => s.confirm);
  const okButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!q) return;
    okButton.current?.focus();
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        q.answer(false);
      }
    };
    window.addEventListener("keydown", key, true);
    return () => window.removeEventListener("keydown", key, true);
  }, [q]);
  if (!q) return null;
  return (
    <div className="modal-back" role="presentation" onMouseDown={() => q.answer(false)}>
      <div className="modal confirm-modal" role="alertdialog" aria-modal="true" aria-labelledby="sk-confirm-title" onMouseDown={(e) => e.stopPropagation()}>
        <h3 id="sk-confirm-title">{q.title}</h3>
        {q.body.map((line, i) => (
          <p key={i} className={i ? "mono confirm-detail" : undefined}>
            {line}
          </p>
        ))}
        <div className="modal-buttons">
          <button className="ghost" onClick={() => q.answer(false)}>
            {t("取消")}
          </button>
          <button ref={okButton} className={q.danger ? "danger-btn" : "primary"} onClick={() => q.answer(true)}>
            {q.ok}
          </button>
        </div>
      </div>
    </div>
  );
}

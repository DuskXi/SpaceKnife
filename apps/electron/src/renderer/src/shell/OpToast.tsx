// The undo toast of a file operation (UI-W5-42…47): progress while it runs, then 5 seconds to undo
// (put trashed items back, or call off a permanent deletion that is still waiting).

import { useEffect, useState } from "react";

import { opCancel, opUndo } from "@/state/actions";
import { set, useApp } from "@/state/store";
import { fmtInt, fmtSize, plural } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

const UNDO_MS = 5000;

export function OpToast() {
  const op = useApp((s) => s.op);
  const [since, setSince] = useState(0);
  const [now, setNow] = useState(0);

  // the undo window starts when the operation is done (trash) or the moment it waits (delete)
  const undoable = !!op && op.can_undo && (op.phase === "done" || op.phase === "waiting");
  useEffect(() => {
    if (!undoable) return;
    const t0 = performance.now();
    setSince(t0);
    setNow(t0);
    const timer = setInterval(() => setNow(performance.now()), 100);
    return () => clearInterval(timer);
  }, [undoable, op?.task_id, op?.phase]);
  useEffect(() => {
    if (op && (op.phase === "done" || op.phase === "undone" || op.phase === "cancelled") && (!undoable || now - since > UNDO_MS)) {
      const timer = setTimeout(() => set({ op: null }), undoable ? 0 : 2500);
      return () => clearTimeout(timer);
    }
  }, [op, undoable, now, since]);
  if (!op) return null;

  const left = Math.max(0, UNDO_MS - (now - since));
  const ok = op.results.filter((r) => r.ok).length;
  const failed = op.results.length - ok;
  let text: string;
  const progress = { done: fmtInt(op.done), total: fmtInt(op.total) };
  if (op.phase === "waiting") text = plural(op.total, t("{n} 项将被永久删除", { n: fmtInt(op.total) }), "1 item will be deleted permanently");
  else if (op.phase === "running") text = op.mode === "trash" ? t("正在移到回收站 {done} / {total}…", progress) : t("正在删除 {done} / {total}…", progress);
  else if (op.phase === "undone") text = t("已撤销");
  else if (op.phase === "cancelled") text = t("已停止");
  else if (op.mode === "trash") {
    const parts = {
      n: fmtInt(ok),
      freed: op.freed ? t("（{size}）", { size: fmtSize(op.freed) }) : "",
      failed: failed ? t("，{n} 项未移动（见日志）", { n: fmtInt(failed) }) : "",
    };
    text = plural(ok, t("已移到回收站 {n} 项{freed}{failed}", parts), "Moved 1 item to trash{freed}{failed}", parts);
  } else {
    const parts = {
      n: fmtInt(ok),
      freed: op.freed ? t("，释放 {size}", { size: fmtSize(op.freed) }) : "",
      failed: failed ? t("，{n} 项失败（见日志）", { n: fmtInt(failed) }) : "",
    };
    text = plural(ok, t("已永久删除 {n} 项{freed}{failed}", parts), "Deleted 1 item permanently{freed}{failed}", parts);
  }

  return (
    <div className="op-toast" role="status" aria-live="polite">
      <Icon name={op.phase === "running" || op.phase === "waiting" ? "clock" : "check"} size={16} />
      <span className="flex">{text}</span>
      {undoable && left > 0 && (
        <button className="ghost" onClick={() => void opUndo(op.task_id)}>
          {t("撤销（{n} 秒）", { n: Math.ceil(left / 1000) })}
        </button>
      )}
      {op.phase === "running" && (
        <button className="ghost" onClick={() => void opCancel(op.task_id)}>
          {t("停止")}
        </button>
      )}
      <button className="icon-btn small" aria-label={t("关闭提示")} onClick={() => set({ op: null })}>
        <Icon name="close" size={14} />
      </button>
      {undoable && <span className="op-countdown" style={{ width: `${(left / UNDO_MS) * 100}%` }} />}
    </div>
  );
}

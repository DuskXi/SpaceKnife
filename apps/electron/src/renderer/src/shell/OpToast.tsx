// The undo toast of a file operation (UI-W5-42…47): progress while it runs, then 5 seconds to undo
// (put trashed items back, or call off a permanent deletion that is still waiting).

import { useEffect, useState } from "react";

import { opCancel, opUndo } from "@/state/actions";
import { set, useApp } from "@/state/store";
import { fmtInt, fmtSize } from "@/lib/format";
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
  if (op.phase === "waiting") text = `${fmtInt(op.total)} 项将被永久删除`;
  else if (op.phase === "running") text = `${op.mode === "trash" ? "正在移到回收站" : "正在删除"} ${fmtInt(op.done)} / ${fmtInt(op.total)}…`;
  else if (op.phase === "undone") text = "已撤销";
  else if (op.phase === "cancelled") text = "已停止";
  else if (op.mode === "trash")
    text = `已移到回收站 ${fmtInt(ok)} 项${op.freed ? `（${fmtSize(op.freed)}）` : ""}${failed ? `，${fmtInt(failed)} 项未移动（见日志）` : ""}`;
  else text = `已永久删除 ${fmtInt(ok)} 项${op.freed ? `，释放 ${fmtSize(op.freed)}` : ""}${failed ? `，${fmtInt(failed)} 项失败（见日志）` : ""}`;

  return (
    <div className="op-toast" role="status" aria-live="polite">
      <Icon name={op.phase === "running" || op.phase === "waiting" ? "clock" : "check"} size={16} />
      <span className="flex">{text}</span>
      {undoable && left > 0 && (
        <button className="ghost" onClick={() => void opUndo(op.task_id)}>
          撤销（{Math.ceil(left / 1000)} 秒）
        </button>
      )}
      {op.phase === "running" && (
        <button className="ghost" onClick={() => void opCancel(op.task_id)}>
          停止
        </button>
      )}
      <button className="icon-btn small" aria-label="关闭提示" onClick={() => set({ op: null })}>
        <Icon name="close" size={14} />
      </button>
      {undoable && <span className="op-countdown" style={{ width: `${(left / UNDO_MS) * 100}%` }} />}
    </div>
  );
}

// The status bar's entry for plugins' background tasks: a spinner, the task (or how many) and how far
// they are; a click opens the list — each task's title, plugin, progress, what it is doing, a cancel
// button — and the ones that ended lately with how they ended.

import { useEffect, useRef, useState } from "react";

import type { PluginTaskInfo } from "@/protocol/generated";
import { useApp } from "@/state/store";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { pluginName } from "./store";
import { cancelTask, overallShare, running, shownTasks, taskShare } from "./tasks";

export function TasksStatusItem() {
  const all = useApp((s) => s.pluginTasks);
  const [open, setOpen] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const box = useRef<HTMLSpanElement>(null);
  const list = shownTasks(all, now);
  const live = list.filter(running);
  // ended tasks leave the bar after a while: look again now and then while any is shown
  useEffect(() => {
    if (!list.length || live.length === list.length) return;
    const id = window.setTimeout(() => setNow(Date.now()), 5000);
    return () => window.clearTimeout(id);
  }, [list.length, live.length, now]);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !box.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  if (!list.length) return null;
  const share = overallShare(live);
  const failed = list.filter((x) => x.state === "failed").length;
  let text: string;
  if (live.length === 1) text = live[0].title;
  else if (live.length > 1) text = t("{n} 个插件任务", { n: live.length });
  else text = failed ? t("插件任务失败") : t("插件任务已结束");
  return (
    <span className="menu-anchor" ref={box}>
      <button
        className="status-item plugin-tasks-item"
        aria-haspopup="menu"
        aria-expanded={open}
        title={t("插件的后台任务")}
        onClick={() => setOpen(!open)}
      >
        {live.length ? <span className="spinner small" aria-hidden="true" /> : <Icon name={failed ? "warning" : "check"} size={14} />}
        <span className={failed && !live.length ? "warn-text" : undefined}>{text}</span>
        {share != null && <span className="strong">{`${Math.round(share * 100)}%`}</span>}
      </button>
      {open && (
        <div
          className="menu plugin-tasks-menu"
          role="menu"
          aria-label={t("插件的后台任务")}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              setOpen(false);
            }
          }}
        >
          <div className="menu-label">{t("插件的后台任务")}</div>
          {list.map((task) => (
            <TaskRow key={task.id} task={task} />
          ))}
        </div>
      )}
    </span>
  );
}

function stateText(task: PluginTaskInfo): string {
  switch (task.state) {
    case "restarting":
      return t("插件崩溃了，正在重新开始（第 {n} 次）", { n: task.attempt });
    case "done":
      return t("已完成");
    case "cancelled":
      return t("已取消");
    case "failed":
      return task.error ? t("失败：{why}", { why: task.error }) : t("失败");
    default:
      return task.text;
  }
}

export function TaskRow({ task }: { task: PluginTaskInfo }) {
  const share = taskShare(task);
  const live = running(task);
  const said = stateText(task);
  const detail = live && share != null ? (said ? `${said} · ${Math.round(share * 100)}%` : `${Math.round(share * 100)}%`) : said;
  return (
    <div className={`plugin-task${live ? "" : " ended"}`} role="menuitem">
      <div className="plugin-task-head">
        <span className="plugin-task-title" title={task.title}>
          {task.title}
        </span>
        <span className="muted small">{pluginName(task.plugin)}</span>
        {live && (
          <button className="icon-btn plugin-task-cancel" title={t("取消这个任务")} aria-label={t("取消这个任务")} onClick={() => void cancelTask(task.id)}>
            <Icon name="close" size={12} />
          </button>
        )}
      </div>
      {live && (
        <span className={`plugin-task-bar${share == null ? " indeterminate" : ""}`}>
          <span style={share == null ? undefined : { width: `${share * 100}%` }} />
        </span>
      )}
      {detail && (
        <span className={`small plugin-task-text${task.state === "failed" ? " warn-text" : " muted"}`} title={detail}>
          {detail}
        </span>
      )}
    </div>
  );
}

// Plugins' background tasks (@plugin.background, docs/plugin-dev.md 「后台任务」): the backend keeps a
// record per task (plugin.tasks) and sends each change (plugin.task: started, moved on, ended). The
// status bar shows the running ones like the app's other long work, with a list to cancel them.

import type { PluginTaskInfo } from "@/protocol/generated";
import { call } from "@/state/actions";
import { get, set } from "@/state/store";

/** How long a task that ended stays in the status bar's list (failed ones until seen in the list). */
export const ENDED_SHOWN_MS = 60_000;

/** One task changed: put it in place (a new one at the end). */
export function putTask(list: readonly PluginTaskInfo[], ev: PluginTaskInfo): PluginTaskInfo[] {
  const i = list.findIndex((t) => t.id === ev.id);
  if (i < 0) return [...list, ev].slice(-64);
  const next = list.slice();
  next[i] = ev;
  return next;
}

export function onTask(ev: PluginTaskInfo): void {
  set((s) => ({ pluginTasks: putTask(s.pluginTasks, ev) }));
}

export async function refreshTasks(): Promise<void> {
  try {
    set({ pluginTasks: (await call("plugin.tasks", {})).tasks });
  } catch {
    /* not connected, or a backend without background tasks */
  }
}

export async function cancelTask(id: string): Promise<void> {
  try {
    onTask(await call("plugin.task_cancel", { id }));
  } catch {
    /* gone meanwhile: the next plugin.task says how it ended */
  }
}

export const running = (t: PluginTaskInfo): boolean => t.state === "running" || t.state === "restarting";

/** What the list shows: running tasks first (oldest first), then the ones that ended lately. */
export function shownTasks(list: readonly PluginTaskInfo[], now: number = Date.now()): PluginTaskInfo[] {
  const live = list.filter(running);
  const ended = list.filter((t) => !running(t) && t.ended != null && now - t.ended * 1000 < ENDED_SHOWN_MS).reverse();
  return [...live, ...ended];
}

/** 0–1 when the task says how far it is, else null (an indeterminate bar). */
export function taskShare(t: PluginTaskInfo): number | null {
  if (!(t.total > 0)) return null;
  return Math.max(0, Math.min(1, t.done / t.total));
}

/** The running tasks' overall share for the status bar (null when none of them can say). */
export function overallShare(list: readonly PluginTaskInfo[]): number | null {
  const shares = list.filter(running).map(taskShare);
  if (!shares.length || shares.some((x) => x == null)) return null;
  return (shares as number[]).reduce((a, b) => a + b, 0) / shares.length;
}

/** Whether anything is worth a place in the status bar right now. */
export function hasShownTasks(now: number = Date.now()): boolean {
  return shownTasks(get().pluginTasks, now).length > 0;
}

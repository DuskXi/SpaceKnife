// Plugins' notices (ctx.notify, the notify permission; docs/plugin-dev.md 「通知」): the backend checks
// and bounds them (at most one per plugin every 5 s) and sends plugin.notice. The window shows them
// in a stack at the bottom right until closed, or for NOTICE_MS of the window being looked at; the
// button runs one of the plugin's own commands with the notice's args. While the window is
// minimized or hidden, the main process shows a system notification as well (window.sk.window.notify);
// clicking it brings the window back and runs the button's command.

import type { PluginNoticeEvent } from "@/protocol/generated";
import { get, set } from "@/state/store";
import { t } from "@/lib/i18n";

/** How long a notice stays while the window has the focus (and the pointer is not on it). */
export const NOTICE_MS = 12_000;
/** Notices on screen at once (the oldest goes). */
export const MAX_NOTICES = 3;

/** The text a system notification shows: the plugin's name with the title. */
export function systemTitle(n: PluginNoticeEvent, name: string): string {
  return name ? t("{plugin}：{text}", { plugin: name, text: n.title }) : n.title;
}

export function onNotice(ev: PluginNoticeEvent, name = ""): void {
  set((s) => ({ notices: [...s.notices.filter((n) => n.id !== ev.id), ev].slice(-MAX_NOTICES) }));
  window.sk.window?.notify?.({ id: ev.id, title: systemTitle(ev, name), body: ev.body });
}

export function dismissNotice(id: string): void {
  set((s) => ({ notices: s.notices.filter((n) => n.id !== id) }));
}

/** The button's text: the notice's own, else its command's title. */
export function noticeButton(n: PluginNoticeEvent): string {
  if (!n.command) return "";
  return n.button || get().commands.find((c) => c.id === n.command)?.name || t("打开");
}

/** The button (or the system notification) was clicked: its command runs with the notice's args
 * (only one of the plugin's own commands: the backend let no other through). */
export async function runNotice(id: string): Promise<void> {
  const n = get().notices.find((x) => x.id === id);
  if (!n) return;
  dismissNotice(id);
  if (!n.command || !n.command.startsWith(`${n.plugin}.`)) return;
  const { COMMANDS } = await import("./actions"); // (plugins/store.ts loads this module: no cycle)
  await COMMANDS.runShortcut(n.command, Object.keys(n.args).length ? { args: n.args } : {});
}

/** Each notice's time on screen: it only runs while the window has the focus and the pointer is
 * not on the stack. `tick` adds `ms` to the shown ones and returns those whose time is up. */
export function tickNotices(shown: Map<string, number>, ids: readonly string[], ms: number): string[] {
  for (const id of [...shown.keys()]) if (!ids.includes(id)) shown.delete(id);
  const done: string[] = [];
  for (const id of ids) {
    const age = (shown.get(id) ?? 0) + ms;
    shown.set(id, age);
    if (age >= NOTICE_MS) done.push(id);
  }
  return done;
}

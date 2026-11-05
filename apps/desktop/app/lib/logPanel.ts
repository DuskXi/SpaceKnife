// The log panel's texts (components/shell/LogPanel.vue): an event's source and level in the current
// language, and its message with the path in monospace, cut from the left when long.

import { t } from "@/lib/i18n";
import type { LogEntry } from "@/state/store";

/** One row's height in px: the list is virtualised on it. */
export const LOG_ROW_H = 20;

/** An event's source in the current language (read at render time, M7.5); unknown ones as they are. */
export function sourceText(source: string): string {
  switch (source) {
    case "scan":
      return t("扫描");
    case "perm":
      return t("权限");
    case "monitor":
      return t("监控");
    case "delete":
      return t("删除");
    case "snapshot":
      return t("快照");
    case "export":
      return t("导出");
    case "system":
      return t("系统");
    default:
      return source;
  }
}

export const logLevelText = (level: LogEntry["level"]): string => ({ INFO: t("信息"), WARNING: t("警告"), ERROR: t("错误") })[level];

/** A long path as "…\\a\\b\\c" (its last three parts); short ones as they are (UI-W8-25). */
export function shortPath(path: string): string {
  if (path.length <= 48) return path;
  const sep = path.includes("\\") ? "\\" : "/";
  const parts = path.split(sep).filter(Boolean);
  return `…${sep}${parts.slice(-3).join(sep)}`;
}

/** A message split around its path: the path part (`mono`) is shown short, in monospace. */
export function messageParts(message: string, path: string): { text: string; mono: boolean }[] {
  const at = path ? message.indexOf(path) : -1;
  if (at < 0) return [{ text: message, mono: false }];
  return [
    { text: message.slice(0, at), mono: false },
    { text: shortPath(path), mono: true },
    { text: message.slice(at + path.length), mono: false },
  ];
}

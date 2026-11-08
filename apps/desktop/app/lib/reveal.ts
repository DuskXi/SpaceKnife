// Labels of the 「在资源管理器中显示」 action (shell/Inspector), per platform.

import { t } from "@/lib/i18n";
import type { PathForms } from "@/protocol/generated";

export const revealLabel = () =>
  window.sk.platform === "darwin" ? t("在访达中显示") : window.sk.platform === "win32" ? t("在资源管理器中显示") : t("在文件管理器中显示");

/** Whether path.forms' text names the item. A name that is not valid Unicode (a lone surrogate on
 *  Windows, bytes that are not UTF-8 on POSIX) is written escaped there, as 0.3.4 wrote it
 *  (`\udc80`, `\xff`): text that names nothing on disk, or another item. Only the URI keeps such
 *  a name's bytes, and those are not UTF-8 then. */
export function textNamesIt(forms: Pick<PathForms, "uri">): boolean {
  try {
    decodeURIComponent(forms.uri);
    return true;
  } catch {
    return false;
  }
}

/** Whether the system can be handed `path` as text to open it, show it in its folder or start a
 *  terminal there: not on Windows when a name ends in a dot or a space (Win32 drops them:
 *  `setup.` would open the `setup` beside it). The text must name it too ([`textNamesIt`]). */
export function openableByPath(path: string, windows: boolean): boolean {
  if (!windows) return true;
  const names = path.replace(/^[A-Za-z]:/, "").split(/[\\/]/);
  return !names.some((n) => n !== "" && n !== "." && n !== ".." && /[. ]$/.test(n));
}

/** [`openableByPath`] on the platform the files are on. */
export const handable = (path: string): boolean => openableByPath(path, window.sk.platform === "win32");

/** The text the system is handed for these forms, or null when it would name another item or
 *  nothing (`notOpenable` says why). */
export const handOff = (forms: Pick<PathForms, "raw" | "uri">): string | null => (textNamesIt(forms) && handable(forms.raw) ? forms.raw : null);

/** The folder of an item (itself when it is one): where a terminal opens. */
export const folderOf = (path: string, isDir: boolean): string => (isDir ? path : path.replace(/[\\/][^\\/]*$/, ""));

/** Why it cannot (the menu shows it on the disabled items). */
export const notOpenable = () => t("它或上级文件夹的名字按路径交给系统会变成另一个（名字里有无法显示的字符，或以点或空格结尾），不能在这里打开");

/** Items under the MFT pseudo folders have no folder of their own to open or delete from. */
export const notOnDisk = () => t("NTFS 元数据与孤立文件只在 MFT 快速扫描里可见，没有可以打开或删除的对应位置");

// 「在画面中隐藏这个文件夹」 / 「不再隐藏这个文件夹」 (UI-W5-52): a folder of the picture in or out of
// the setting treemap.hidden_folders, from the context menu (ContextMenu.vue) and the command
// palette (item.hide_folder / item.unhide_folder). The backend says which applies
// (view.hidden_folder: the list's entries naming it, the path to add, or why neither) and makes the
// change (view.hide_folder), so the menu, the palette and the views judge a path alike.

import { t } from "@/lib/i18n";
import type { HiddenFolderInfo } from "@/protocol/generated";

import { applySettings, call, setViewSettings } from "./actions";
import { type AppState, get, toast } from "./store";

export const HIDDEN_FOLDERS = "treemap.hidden_folders";

/** Why the setting can't be changed now, as the settings page judges its rows (SettingRow): no
 * backend to save it, or the setting is off here. In a browser tab of headless mode it can be
 * changed like any setting (they belong to the computer the files are on). */
export function settingLocked(s: Pick<AppState, "conn" | "settings"> = get()): string | undefined {
  if (s.conn !== "ready") return t("与后端的连接已断开，设置暂不可修改");
  const meta = s.settings?.meta.find((m) => m.key === HIDDEN_FOLDERS);
  if (meta && !meta.enabled) return meta.reason || t("这个设置现在不能修改");
  return undefined;
}

/** The menu's row for a folder: hide it or show it again, and why it is greyed (undefined: it
 * isn't). `info` null: the answer is on its way, the row offers hiding (the backend decides). */
export function hideRow(info: HiddenFolderInfo | null, locked: string | undefined): { hide: boolean; disabled?: string } {
  const hide = !info || info.listed.length === 0;
  return { hide, disabled: info?.reason || locked };
}

/** The view is zoomed into `node` or a folder inside it: hidden, it is still drawn there. */
function zoomedIn(viewId: string, node: number): boolean {
  return get().views[viewId]?.frame?.info.breadcrumb.some((c) => c.key === node) ?? false;
}

/** Put the folder `node` of the view's scan in the list (`hide`) or take it out; the views redraw
 * as the setting changes. Hiding turns the view's 「隐藏文件夹」 on when it was off: the folder is
 * meant to go. `name`: what the toast calls it (else its name in the scan). */
export async function hideFolder(viewId: string, node: number, hide: boolean, name?: string): Promise<void> {
  const view = get().views[viewId];
  if (!view) return;
  const locked = settingLocked();
  if (locked) return toast(locked);
  try {
    applySettings(await call("view.hide_folder", { view_id: viewId, node, hide }));
  } catch (err) {
    return toast(err instanceof Error ? err.message : String(err));
  }
  if (hide && get().views[viewId]?.state.hide_folders === false) await setViewSettings(viewId, { hide_folders: true });
  const shown =
    name ||
    (await call("tree.node", { scan_id: view.state.scan_id, node })
      .then((r) => r.node.name)
      .catch(() => ""));
  if (!hide) return toast(t("「{name}」不再隐藏", { name: shown }), 3000);
  toast(
    zoomedIn(viewId, node)
      ? t("已隐藏「{name}」：放大在它里面时照常显示，回到外面就不占画面了", { name: shown })
      : t("已隐藏「{name}」：工具栏的「隐藏文件夹」（H）可以让它回来", { name: shown }),
    3000,
  );
}

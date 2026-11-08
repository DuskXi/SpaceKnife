// The pure parts of the delete basket drawer (shell/BasketDrawer in the Electron renderer).

import type { BasketItem, BasketVolume, OpProgressEvent } from "@/protocol/generated";
import { fmtInt, plural } from "@/lib/format";
import { t } from "@/lib/i18n";

export type BasketSort = "size" | "name" | "path";
export const BASKET_SORTS: readonly BasketSort[] = ["size", "name", "path"];
export const basketSortLabel = (k: BasketSort): string => ({ size: t("按大小"), name: t("按名称"), path: t("按位置") })[k];

/** 「3 项」 / "3 items", "1 item". */
export const itemCount = (n: number): string => plural(n, t("{n} 项", { n: fmtInt(n) }), "1 item");

/** Why an item cannot be deleted now, when the backend gave no note. */
export const statusText = (status: BasketItem["status"]): string =>
  ({ ready: "", locked: t("正被其他程序使用"), missing: t("已经不在磁盘上了"), protected: t("受保护的系统位置，不能删除") })[status];

/** The volume most of the basket frees space on (its capacity bar is shown). */
export function mainVolume(vols: readonly BasketVolume[]): BasketVolume | null {
  return vols.reduce<BasketVolume | null>((a, v) => (a == null || v.freeable > a.freeable ? v : a), null);
}

/** The items in the drawer's order (a copy). */
export function sortedItems(items: readonly BasketItem[], sort: BasketSort): BasketItem[] {
  const out = [...items];
  if (sort === "size") out.sort((a, b) => b.alloc - a.alloc);
  else if (sort === "name") out.sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));
  else out.sort((a, b) => a.path.localeCompare(b.path, "zh-CN"));
  return out;
}

/** What a confirmation was given for: each item as the drawer showed it then. */
export type Confirmed = ReadonlyArray<{ id: string; members: number }>;

/** The items the user is asked to confirm (the basket is shared: another session, a browser in
 *  headless mode, can add to it while the dialog is open). */
export const confirmedOf = (items: readonly BasketItem[]): Confirmed => items.map((i) => ({ id: i.id, members: i.members }));

/** The confirmed items still in the basket now (what executing the confirmed ids runs). */
export function stillConfirmed(confirmed: Confirmed, now: readonly BasketItem[]): BasketItem[] {
  const ids = new Set(confirmed.map((c) => c.id));
  return now.filter((i) => ids.has(i.id));
}

/** Whether a confirmed item now holds more than it did (a group of small items another session
 *  added to keeps its id): executing the confirmed ids would delete what was not confirmed. */
export function grewSince(confirmed: Confirmed, now: readonly BasketItem[]): boolean {
  const byId = new Map(now.map((i) => [i.id, i]));
  return confirmed.some((c) => (byId.get(c.id)?.members ?? 0) > c.members);
}

/** Whether the toast of a file operation offers undo: a deletion still waiting, a move to the
 *  trash done or stopped midway (what went to the trash comes back), as the backend says. */
export const opUndoable = (op: Pick<OpProgressEvent, "can_undo" | "phase">): boolean =>
  op.can_undo && (op.phase === "done" || op.phase === "waiting" || op.phase === "cancelled");

/** The folder an item lies in (its path without the last name). */
export const parentOf = (path: string): string => path.replace(/[\\/][^\\/]*[\\/]?$/, "");

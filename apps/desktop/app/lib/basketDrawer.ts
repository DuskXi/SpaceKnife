// The pure parts of the delete basket drawer (shell/BasketDrawer in the Electron renderer).

import type { BasketItem, BasketVolume } from "@/protocol/generated";
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

/** The folder an item lies in (its path without the last name). */
export const parentOf = (path: string): string => path.replace(/[\\/][^\\/]*[\\/]?$/, "");

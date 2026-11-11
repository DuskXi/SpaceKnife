// The toolbar's words and switches (components/shell/Toolbar*.vue): colour modes, the free / unknown
// space and recycle bin switches, and the breadcrumb's labels and full path.

import type { ViewState } from "@/protocol/generated";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import type { COLOR_MODES } from "@/state/visuals";

/** The breadcrumb keeps this much of itself (or all, when shorter) before groups fold away. */
export const CRUMBS_MIN = 160;

export const colorLabel = (m: (typeof COLOR_MODES)[number]): string => ({ class: t("类别"), flat: t("扁平"), time: t("时间"), branch: t("分支") })[m];

/** The recycle bin's name on this system: macOS calls it the Trash. */
export const binName = (): string => (window.sk.platform === "darwin" ? t("废纸篓") : t("回收站"));

export interface SpaceSwitch {
  id: "free" | "unknown" | "trash" | "folders";
  key: string;
  /** drawn instead of the colour swatch (a switch that stands for no colour of its own) */
  icon?: IconName;
  label: () => string;
  title: (st: Pick<ViewState, "hidden_folders">) => string;
  /** pressed: the space is drawn; for 「隐藏文件夹」, the folders are hidden */
  on: (st: ViewState) => boolean;
  flip: (st: ViewState) => Partial<Pick<ViewState, "show_free" | "show_unknown" | "show_trash" | "hide_folders">>;
}

/** The free / unknown space and recycle bin switches (UI-MAIN-21, 22; the bin's when the scan holds
 * one), and 「隐藏文件夹」 when the scan holds any of the folders of the setting 「要隐藏的文件夹」. */
export const SPACE_SWITCHES: readonly SpaceSwitch[] = [
  { id: "free", key: "F", label: () => t("可用空间"), title: () => t("可用空间（F）"), on: (st) => st.show_free, flip: (st) => ({ show_free: !st.show_free }) },
  { id: "unknown", key: "U", label: () => t("未知空间"), title: () => t("未知空间（U）"), on: (st) => st.show_unknown, flip: (st) => ({ show_unknown: !st.show_unknown }) },
  { id: "trash", key: "R", label: () => binName(), title: () => t("回收站（R）"), on: (st) => st.show_trash, flip: (st) => ({ show_trash: !st.show_trash }) },
  {
    id: "folders",
    key: "H",
    icon: "eyeOff",
    label: () => t("隐藏文件夹"),
    title: (st) => t("隐藏设置里列出的 {n} 个文件夹（H）", { n: st.hidden_folders }),
    on: (st) => st.hide_folders,
    flip: (st) => ({ hide_folders: !st.hide_folders }),
  },
];

/** The switches a view shows: the recycle bin's only when its scan holds one, 「隐藏文件夹」 only when
 * it holds a folder to hide. */
export const spaceSwitches = (st: Pick<ViewState, "has_trash" | "hidden_folders">): readonly SpaceSwitch[] =>
  SPACE_SWITCHES.filter((s) => (s.id !== "trash" || st.has_trash) && (s.id !== "folders" || st.hidden_folders > 0));

export interface Crumb {
  key: number;
  label: string;
}

/** A breadcrumb segment's label: aggregate blocks by their count, the root of a whole-volume scan by
 * the tab's title, anything else by its last path part. */
export function crumbLabel(c: Crumb, i: number, whole: boolean, title: string): string {
  if (c.label.startsWith("small:")) return t("{n} 个小项", { n: Number(c.label.slice(6)).toLocaleString("en-US") });
  if (c.label.startsWith("more:")) return t("+{n} 项", { n: Number(c.label.slice(5)).toLocaleString("en-US") });
  if (i === 0 && whole) return title;
  return c.label.split(/[\\/]/).filter(Boolean).pop() ?? c.label;
}

/** The full path the breadcrumb stands for (the path editor's starting text); `fallback` without one. */
export function fullPath(crumbs: readonly Crumb[], fallback: string, platform: string = window.sk.platform): string {
  if (!crumbs.length) return fallback;
  const sep = platform === "win32" ? "\\" : "/";
  const parts = crumbs.filter((c) => !c.label.startsWith("small:") && !c.label.startsWith("more:")).map((c) => c.label);
  const [first, ...rest] = parts;
  return [first!.replace(/[\\/]+$/, ""), ...rest].join(sep) + (rest.length ? "" : sep);
}

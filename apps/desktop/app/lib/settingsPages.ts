// The settings tab's pages and its search (pages/SettingsPage.vue, the toolbar's breadcrumb): the
// pages in their nav groups, rows grouped by their backend group, when a row's dependency holds,
// and the search over names, descriptions, synonyms and options (UI-W7-05).

import type { SettingMeta } from "@/protocol/generated";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import type { SettingsPageId } from "@/state/store";

export type SettingValue = boolean | number | string;

// name and note translate each time they are read (getters), so a language switch applies (M7.5)
export const PAGES: { id: SettingsPageId; readonly name: string; icon: IconName; readonly note: string }[] = [
  {
    id: "general",
    get name() {
      return t("常规");
    },
    icon: "settings",
    get note() {
      return t("语言、悬停提示、窗口提醒与日志");
    },
  },
  {
    id: "appearance",
    get name() {
      return t("外观");
    },
    icon: "palette",
    get note() {
      return t("主题、强调色、动画与可视化外观；更改立即生效");
    },
  },
  {
    // the category id stays "treemap" (settings keys treemap.*); the page covers every visualization
    id: "treemap",
    get name() {
      return t("可视化");
    },
    icon: "layers",
    get note() {
      return t("几何、火焰图方向与动画；修改后已打开的视图立即重排，「新视图…」类只影响之后新开的视图");
    },
  },
  {
    id: "scan",
    get name() {
      return t("扫描与性能");
    },
    icon: "gauge",
    get note() {
      return t("扫描方式、扫描范围与硬链接计算从下一次扫描起生效，其余更改立即生效");
    },
  },
  {
    id: "classes",
    get name() {
      return t("文件类别");
    },
    icon: "file",
    get note() {
      return t("按扩展名归类，用于树图着色、:class: 过滤和类别统计");
    },
  },
  {
    id: "tags",
    get name() {
      return t("标签");
    },
    icon: "tag",
    get note() {
      return t("四个固定标签：红▲、黄■、绿●、蓝◆，颜色与形状不可修改");
    },
  },
  {
    id: "keys",
    get name() {
      return t("快捷键");
    },
    icon: "command",
    get note() {
      return t("全部命令与当前快捷键，与命令面板同一份命令清单");
    },
  },
  {
    id: "advanced",
    get name() {
      return t("高级");
    },
    icon: "terminal",
    get note() {
      return t("设置文件、日志、缓存与快照");
    },
  },
  {
    id: "about",
    get name() {
      return t("关于与更新");
    },
    icon: "info",
    get note() {
      return t("版本信息、更新与发布时间线");
    },
  },
];
export const GROUPS: SettingsPageId[][] = [
  ["general", "appearance", "treemap"],
  ["scan", "classes", "tags"],
  ["keys", "advanced", "about"],
];
export const pageName = (id: string): string => PAGES.find((p) => p.id === id)?.name ?? id;
export const pageOf = (id: string) => PAGES.find((x) => x.id === id) ?? PAGES[0]!;

/** The rows by their group, in the order the groups first appear. */
export function groupBy(rows: SettingMeta[]): [string, SettingMeta[]][] {
  const out = new Map<string, SettingMeta[]>();
  for (const r of rows) out.set(r.group, [...(out.get(r.group) ?? []), r]);
  return [...out.entries()];
}

/** Whether a row's `depends` ("key" or "key=value") holds for these values. */
export function dependsOk(depends: string | null | undefined, values: Record<string, SettingValue> | null | undefined): boolean {
  if (!depends || !values) return true;
  const [k, want] = depends.split("=");
  const v = values[k!];
  return want === undefined ? Boolean(v) : String(v) === want;
}

/** The settings matching a query (UI-W7-05): name, description, group, page, synonyms, options. */
export function searchSettings(meta: SettingMeta[], query: string): SettingMeta[] {
  const q = query.toLowerCase();
  return meta.filter((m) =>
    [m.name, m.description, m.group, pageName(m.category), ...m.synonyms, ...m.options.map((o) => `${o.label} ${o.value}`)].some((x) => x.toLowerCase().includes(q)),
  );
}

/** A name with the first match of the query marked: [before, match, after] ("" match when none). */
export function markOnce(text: string, query?: string): [string, string, string] {
  if (!query) return [text, "", ""];
  const at = text.toLowerCase().indexOf(query.toLowerCase());
  if (at < 0) return [text, "", ""];
  return [text.slice(0, at), text.slice(at, at + query.length), text.slice(at + query.length)];
}

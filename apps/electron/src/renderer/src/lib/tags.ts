// Tag display names (W7 「标签」 · 显示名, UI-W7-34): 「红色标签 · 保留」 when the user named it.

import type { SettingsState } from "@/protocol/generated";

import { t } from "./i18n";

/** 「红色标签」 in the current language (a table built when shown: the language can change). */
function colorLabel(color: string): string {
  const labels: Record<string, string> = { red: t("红色标签"), yellow: t("黄色标签"), green: t("绿色标签"), blue: t("蓝色标签") };
  return labels[color] ?? t("{color}色标签", { color });
}

/** The user's name for a tag colour ("" when none). */
export function tagName(settings: SettingsState | null, color: string): string {
  const v = settings?.values[`tags.name_${color}`];
  return typeof v === "string" ? v.trim() : "";
}

/** 「红色标签」, or 「红色标签 · 保留」. */
export function tagLabel(settings: SettingsState | null, color: string): string {
  const name = tagName(settings, color);
  const label = colorLabel(color);
  return name ? `${label} · ${name}` : label;
}

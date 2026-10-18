// Tag display names (W7 「标签」 · 显示名, UI-W7-34): 「红色标签 · 保留」 when the user named it.

import type { SettingsState } from "@/protocol/generated";

const ZH: Record<string, string> = { red: "红", yellow: "黄", green: "绿", blue: "蓝" };

/** The user's name for a tag colour ("" when none). */
export function tagName(settings: SettingsState | null, color: string): string {
  const v = settings?.values[`tags.name_${color}`];
  return typeof v === "string" ? v.trim() : "";
}

/** 「红色标签」, or 「红色标签 · 保留」. */
export function tagLabel(settings: SettingsState | null, color: string): string {
  const name = tagName(settings, color);
  return `${ZH[color] ?? color}色标签${name ? ` · ${name}` : ""}`;
}

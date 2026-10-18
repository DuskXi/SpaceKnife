// The few texts of the main process (M7.5): dialog titles and backend start-up errors. The
// language follows the renderer (sk:setLanguage) once it knows the setting; before that the OS's.

import { app } from "electron";

export type Lang = "zh-CN" | "en";

const TEXT = {
  pickFolders: { "zh-CN": "选择要扫描的文件夹", en: "Choose folders to scan" },
  openSnapshot: { "zh-CN": "打开快照文件", en: "Open snapshot file" },
  snapshotFilter: { "zh-CN": "SpaceKnife 快照", en: "SpaceKnife snapshot" },
  backendTimeout: { "zh-CN": "后端 30 秒内没有就绪", en: "The backend was not ready within 30 seconds" },
  backendBadLine: { "zh-CN": "后端就绪行无法解析：{line}（{error}）", en: "Cannot read the backend's ready line: {line} ({error})" },
  backendExited: { "zh-CN": "后端启动失败（退出码 {code}）", en: "The backend failed to start (exit code {code})" },
} as const;

let lang: Lang | null = null;

function systemLang(): Lang {
  const tag = (app.getPreferredSystemLanguages()[0] ?? app.getLocale() ?? "").toLowerCase();
  return tag.startsWith("zh") ? "zh-CN" : "en";
}

export function setLang(value: Lang): void {
  lang = value === "en" ? "en" : "zh-CN";
}

export function mt(key: keyof typeof TEXT, fields: Record<string, string | number> = {}): string {
  const text: string = TEXT[key][lang ?? systemLang()];
  return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in fields ? String(fields[k]) : m));
}

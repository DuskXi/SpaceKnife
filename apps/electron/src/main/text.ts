// The few texts of the main process (M7.5): dialog titles, backend start-up errors and why a
// downloaded update can't be started. The language follows the renderer (sk:setLanguage) once it
// knows the setting; before that the OS's.

import { app } from "electron";

export type Lang = "zh-CN" | "en";

const TEXT = {
  pickFolders: { "zh-CN": "选择要扫描的文件夹", en: "Choose folders to scan" },
  openSnapshot: { "zh-CN": "打开快照文件", en: "Open snapshot file" },
  snapshotFilter: { "zh-CN": "SpaceKnife 快照", en: "SpaceKnife snapshot" },
  backendTimeout: { "zh-CN": "后端 30 秒内没有就绪", en: "The backend wasn't ready within 30 seconds." },
  backendBadLine: { "zh-CN": "后端就绪行无法解析：{line}（{error}）", en: "Can't read the backend's ready line: {line} ({error})" },
  backendExited: { "zh-CN": "后端启动失败（退出码 {code}）", en: "The backend failed to start (exit code {code})." },
  updateNoInstall: { "zh-CN": "这份 SpaceKnife 的安装方式没有安装步骤", en: "This copy of SpaceKnife has no install step for updates." },
  updateMissing: { "zh-CN": "找不到下载的更新文件", en: "The downloaded update file wasn't found." },
  updateNotInUpdates: { "zh-CN": "更新文件不在 SpaceKnife 的更新文件夹里", en: "The update file isn't in SpaceKnife's updates folder." },
  updateNotAnUpdate: { "zh-CN": "这不是这种安装方式的 SpaceKnife 更新文件", en: "This isn't a SpaceKnife update for this kind of install." },
  updateStartFailed: { "zh-CN": "无法启动更新：{error}", en: "Can't start the update: {error}" },
  updateUnpackFailed: { "zh-CN": "无法解压更新：{error}", en: "Can't unpack the update: {error}" },
  updateReplaceFailed: { "zh-CN": "无法替换旧版本：{error}", en: "Can't replace the old version: {error}" },
} as const;

export type TextKey = keyof typeof TEXT;

let lang: Lang | null = null;

function systemLang(): Lang {
  const tag = (app.getPreferredSystemLanguages()[0] ?? app.getLocale() ?? "").toLowerCase();
  return tag.startsWith("zh") ? "zh-CN" : "en";
}

export function setLang(value: Lang): void {
  lang = value === "en" ? "en" : "zh-CN";
}

export function mt(key: TextKey, fields: Record<string, string | number> = {}): string {
  const text: string = TEXT[key][lang ?? systemLang()];
  return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in fields ? String(fields[k]) : m));
}

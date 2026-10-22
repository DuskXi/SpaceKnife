// Scan sources of plugins (docs/plugins.md 「scan_source」): what the user picks for one (a file, a
// folder or typed text), starting the scan (scan.start {source, uri}), and which scan came from
// which source (rescanning everything runs the source again; `source:` conditions).

import type { SourceInfo } from "@/protocol/generated";
import { startScan } from "@/state/actions";
import { t } from "@/lib/i18n";

const bySource = new Map<string, { source: string; uri: string }>();

/** The source a scan of this session came from, or null (a disk scan, a snapshot). */
export const scanSourceOf = (scanId: string): string | null => bySource.get(scanId)?.source ?? null;

/** What to run again for a scan of a source: its source and URI. */
export const sourceScanOf = (scanId: string): { source: string; uri: string } | null => bySource.get(scanId) ?? null;

/** Extensions as the file dialog takes them: ".zip" → "zip". */
export const dialogExtensions = (src: Pick<SourceInfo, "extensions">): string[] => src.extensions.map((x) => x.replace(/^\*?\./, "")).filter(Boolean);

/** Ask for what the source lists: a file, a folder; null when cancelled. A `text` source has its
 * own input on the start page (its placeholder says what to type). */
export async function pickUri(src: SourceInfo): Promise<string | null> {
  if (src.pick === "file") {
    const exts = dialogExtensions(src);
    return window.sk.dialog.openFile({ title: src.placeholder || src.title, filters: exts.length ? [{ name: src.title, extensions: exts }] : [] });
  }
  if (src.pick === "folder") return (await window.sk.dialog.openFolders())[0] ?? null;
  return null;
}

/** Start a scan of `uri` from the source; resolves with the scan id (null when it did not start). */
export async function startSourceScan(src: Pick<SourceInfo, "id">, uri: string, replaceTab?: string): Promise<string | null> {
  const text = uri.trim();
  if (!text) return null;
  // not a disk: the macOS consent prompts of protected places don't apply
  const id = await startScan({ source: src.id, uri: text, skip_protected: false }, replaceTab);
  if (id) bySource.set(id, { source: src.id, uri: text });
  return id;
}

/** The start page's line under a source: what it can and can't do. */
export function sourceNote(src: Pick<SourceInfo, "on_disk" | "ops" | "watch">): string {
  if (!src.on_disk) return t("内容不在磁盘上：不能打开、定位或删除");
  if (!src.ops) return t("可以打开和定位，不能删除");
  return src.watch ? t("可以打开、定位和删除，变化时自动更新") : t("可以打开、定位和删除");
}

// The command palette's rows (components/shell/Palette*.vue, W4): the backend ranks everything
// (palette.query / palette.paths); this lays the answer out as one list of runnable lines in groups,
// says which run where, and splits names around their matched characters.

import type { PaletteCommand, PaletteDirect, PalettePath, PalettePaths, PaletteRecent, PaletteResult, Span } from "@/protocol/generated";

export type PaletteRow =
  | { kind: "filter"; d: PaletteDirect }
  | { kind: "goto"; d: PaletteDirect }
  | { kind: "recent"; r: PaletteRecent }
  | { kind: "command"; c: PaletteCommand }
  | { kind: "path"; p: PalettePath };

export type PaletteGroup = "direct" | "recent" | "command" | "path";

export interface PaletteLine {
  row: PaletteRow;
  group: PaletteGroup;
  enabled: boolean;
}

/** Whether a direct row runs: a valid filter in a scan tab; a path to jump to (in this scan's tab)
 * or to scan (outside it). */
export const directEnabled = (d: PaletteDirect, scanTab: boolean): boolean =>
  d.kind === "filter" ? d.valid && scanTab : d.state === "outside" || (d.state === "in_scan" && scanTab);

const directRow = (d: PaletteDirect): PaletteRow => (d.kind === "filter" ? { kind: "filter", d } : { kind: "goto", d });

/** The answer as lines, in group order: direct, recent, commands, paths (when asked for). */
export function paletteLines(res: PaletteResult | null, paths: PalettePaths | null, scanTab: boolean): PaletteLine[] {
  if (!res) return [];
  const out: PaletteLine[] = [];
  for (const d of res.direct) out.push({ row: directRow(d), group: "direct", enabled: directEnabled(d, scanTab) });
  for (const r of res.recent) out.push({ row: { kind: "recent", r }, group: "recent", enabled: scanTab && r.exists });
  for (const c of res.commands) out.push({ row: { kind: "command", c }, group: "command", enabled: c.enabled });
  if (res.paths && paths) for (const p of paths.items) out.push({ row: { kind: "path", p }, group: "path", enabled: true });
  return out;
}

/** What Enter pressed before the answer arrived runs: the first runnable direct row, else recent
 * entry, else command (never a path: those come later). */
export function firstRunnable(r: PaletteResult, scanTab: boolean): PaletteLine | null {
  const direct = r.direct.find((d) => directEnabled(d, scanTab));
  if (direct) return { row: directRow(direct), group: "direct", enabled: true };
  const rec = r.recent.find((x) => scanTab && x.exists);
  if (rec) return { row: { kind: "recent", r: rec }, group: "recent", enabled: true };
  const cmd = r.commands.find((c) => c.enabled);
  if (cmd) return { row: { kind: "command", c: cmd }, group: "command", enabled: true };
  return null;
}

/** Text split at the matched characters (UTF-16 spans from the backend); `hit` parts are bold. */
export function hitParts(text: string, hits: readonly Span[]): { text: string; hit: boolean }[] {
  if (!hits.length) return [{ text, hit: false }];
  const out: { text: string; hit: boolean }[] = [];
  let at = 0;
  for (const h of hits) {
    if (h.start > at) out.push({ text: text.slice(at, h.start), hit: false });
    out.push({ text: text.slice(h.start, h.end), hit: true });
    at = h.end;
  }
  if (at < text.length) out.push({ text: text.slice(at), hit: false });
  return out;
}

/** A folder's path with its trailing separator (Tab on a recent folder goes into it). */
export function withSep(path: string): string {
  const sep = path.includes("\\") || /^[A-Za-z]:/.test(path) ? "\\" : "/";
  return path.endsWith(sep) ? path : path + sep;
}

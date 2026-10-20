// Update checks (updates.v1): which surface says that a newer release exists, the download of
// it, and a plain-text summary of its release notes. Pure: the backend's UpdateStatus goes in,
// nothing here reads the store or talks to the backend (state/actions.ts does).
//
// How loudly depends on how far behind the app is: a patch release (0.3.0 → 0.3.1) only gets the
// badge in the title bar; a minor one (0.3.x → 0.4.0) also a card hanging at the top left; a
// major one (0.x → 1.0.0) also a dialog. The badge stays for every level; clicking it opens the
// card under it. Closing the card or the dialog (update.dismiss) keeps them closed for that version.

import type { UpdateDownload, UpdateInfo, UpdateStatus } from "@/protocol/generated";

export type InstallKind = UpdateStatus["install_kind"];

/** Where the update card shows: under the badge (opened by hand), at the top left by itself (minor),
 * as a dialog by itself (major), or nowhere. */
export type UpdateSurface = "none" | "anchored" | "corner" | "dialog";

/** A newer release is known: the title bar shows the badge. */
export function hasUpdate(st: UpdateStatus | null | undefined): st is UpdateStatus & { latest: UpdateInfo } {
  return !!st && st.level !== "none" && !!st.latest;
}

/** What opens by itself for a status: the top-left card for a minor release, the dialog for a major
 * one, nothing for a patch release or once this version's card was closed. */
export function autoSurface(st: UpdateStatus | null | undefined): Exclude<UpdateSurface, "anchored"> {
  if (!hasUpdate(st) || st.dismissed) return "none";
  return st.level === "major" ? "dialog" : st.level === "minor" ? "corner" : "none";
}

/** The surface on screen: the card opened from the badge replaces the one that opened by itself. */
export function visibleSurface(st: UpdateStatus | null | undefined, open: boolean): UpdateSurface {
  if (!hasUpdate(st)) return "none";
  return open ? "anchored" : autoSurface(st);
}

const IDLE: UpdateDownload = { state: "idle", version: null, received: 0, total: 0, path: null, verified: null, error: null };

/** The download of the version shown; an idle one when the backend's is of another (older) version. */
export function downloadOf(st: UpdateStatus | null | undefined): UpdateDownload {
  if (!st?.latest) return IDLE;
  const d = st.download;
  return d.version !== null && d.version !== st.latest.version ? IDLE : d;
}

/** Whole percent of a running download; null when the size is unknown or nothing runs. */
export function downloadPct(d: UpdateDownload): number | null {
  if (d.state === "verifying") return 100;
  if (d.state !== "downloading" || d.total <= 0) return null;
  return Math.min(100, Math.floor((d.received / d.total) * 100));
}

/** The download runs (checking the file included): the badge fills up, the card shows the progress. */
export const downloading = (d: UpdateDownload): boolean => d.state === "downloading" || d.state === "verifying";

/** After the download: start it (an installer, a disk image, a package; the app quits) or show it in
 * its folder (a portable archive or an AppImage the user puts in place of the old one). */
export function installAction(kind: InstallKind): "install" | "reveal" {
  return kind === "nsis" || kind === "dmg" || kind === "deb" || kind === "rpm" ? "install" : "reveal";
}

/** YYYY-MM-DD of a release in local time; "" when unknown. */
export function releaseDate(iso: string | null | undefined): string {
  const d = iso ? new Date(iso) : null;
  if (!d || Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

// ------------------------------------------------------------------ release notes

export interface NoteLine {
  kind: "heading" | "item" | "text";
  text: string;
  /** list nesting: 0, or 1 for anything deeper */
  depth: number;
}

export interface NotesSummary {
  lines: NoteLine[];
  /** the release page says more than the summary */
  cut: boolean;
}

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };

/** Markdown inline syntax to its text: links and images keep their text, code its content,
 * emphasis and HTML tags go; a backslash escape keeps the character as written. */
export function plainInline(s: string): string {
  // escaped characters are parked in the private use area so no rule below sees them
  let out = s.replace(/\\([!-/:-@[-`{-~])/g, (_, c: string) => String.fromCharCode(0xe000 + c.charCodeAt(0)));
  out = out
    .replace(/(`+)([^`]|[^`].*?[^`])\1(?!`)/g, (_, _q: string, code: string) => code.trim())
    .replace(/!\[([^\]]*)\]\((?:[^()]|\([^()]*\))*\)/g, "$1") // a target may hold one level of (…)
    .replace(/\[([^\]]+)\]\((?:[^()]|\([^()]*\))*\)/g, "$1")
    .replace(/\[([^\]]+)\]\[[^\]]*\]/g, "$1")
    .replace(/<((?:https?|mailto):[^>\s]+)>/g, "$1")
    .replace(/<br\s*\/?>/gi, " ")
    .replace(/<\/?[a-zA-Z][^>]*>/g, "")
    .replace(/(\*\*|__)(?=\S)(.+?)(?<=\S)\1/g, "$2")
    .replace(/(^|[^*\w])\*(?=\S)(.+?)(?<=\S)\*(?![*\w])/g, "$1$2")
    .replace(/(^|[^_\w])_(?=\S)(.+?)(?<=\S)_(?![_\w])/g, "$1$2")
    .replace(/~~(?=\S)(.+?)(?<=\S)~~/g, "$1")
    .replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (m, e: string) => {
      if (e[0] !== "#") return ENTITIES[e.toLowerCase()] ?? m;
      const code = e[1] === "x" || e[1] === "X" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
      return code > 0 && code < 0x110000 ? String.fromCodePoint(code) : m;
    });
  return out.replace(/[-]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xe000)).replace(/\s+/g, " ").trim();
}

/** Every line of the notes that says something, as plain text. Code blocks, rules, HTML comments,
 * link definitions and table separators say nothing here; `skipped` tells whether any of them did
 * hold something (the summary then leaves out part of the notes). */
function noteLines(md: string): { lines: NoteLine[]; skipped: boolean } {
  const lines: NoteLine[] = [];
  let skipped = false;
  let fence: string | null = null;
  let prevText = false; // the previous source line became a text line (a setext heading follows it)
  for (const raw of md.replace(/<!--[\s\S]*?(-->|$)/g, "").split(/\r\n?|\n/)) {
    const wasText = prevText;
    prevText = false;
    const open = /^ {0,3}(`{3,}|~{3,})/.exec(raw);
    if (fence !== null) {
      if (open && open[1]![0] === fence[0] && open[1]!.length >= fence.length && !raw.trim().slice(open[1]!.length)) fence = null;
      else if (raw.trim()) skipped = true;
      continue;
    }
    if (open) {
      fence = open[1]!;
      continue;
    }
    if (!raw.trim()) continue;
    if (/^ {0,3}(=+|-+)\s*$/.test(raw) && wasText) {
      lines[lines.length - 1]!.kind = "heading"; // setext: the line above is a heading
      continue;
    }
    if (/^ {0,3}([-*_])(\s*\1){2,}\s*$/.test(raw)) continue; // a rule
    if (/^ {0,3}\[[^\]]+\]:\s*\S/.test(raw)) continue; // a link definition
    if (/^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)+\|?\s*$/.test(raw)) continue; // a table's separator row
    let line = raw.replace(/^\s*(>\s?)+/, ""); // a quote reads like the text it quotes
    let kind: NoteLine["kind"] = "text";
    let depth = 0;
    const heading = /^ {0,3}#{1,6}\s+(.*?)(\s+#+)?\s*$/.exec(line);
    const item = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/.exec(line);
    if (heading) {
      kind = "heading";
      line = heading[1]!;
    } else if (item) {
      kind = "item";
      depth = item[1]!.replace(/\t/g, "    ").length >= 2 ? 1 : 0;
      line = item[3]!.replace(/^\[[ xX]\]\s+/, "");
    } else if (/^\s*\|.*\|\s*$/.test(line)) {
      line = line.trim().slice(1, -1).split("|").map((c) => c.trim()).filter(Boolean).join(" · ");
    }
    const text = plainInline(line);
    if (!text) continue; // an image without alt text, an empty item
    lines.push({ kind, text, depth });
    prevText = kind === "text";
  }
  return { lines, skipped };
}

/** The start of the release notes for the card: at most `maxLines` lines and about `maxChars`
 * characters of plain text. A heading left with nothing under it at the end is dropped. */
export function summarizeNotes(md: string, maxLines = 8, maxChars = 600): NotesSummary {
  const all = noteLines(md);
  const lines: NoteLine[] = [];
  let used = 0;
  let cut = all.skipped;
  for (const line of all.lines) {
    if (lines.length >= maxLines) {
      cut = true;
      break;
    }
    const room = maxChars - used;
    if (line.text.length > room) {
      cut = true;
      // a few words are worth keeping; a stub of one is not
      if (room >= 40) lines.push({ ...line, text: clip(line.text, room) });
      break;
    }
    lines.push(line);
    used += line.text.length;
  }
  if (cut) while (lines.length && lines[lines.length - 1]!.kind === "heading") lines.pop();
  return { lines, cut };
}

/** `text` cut to at most `room` characters at a word boundary when there is one nearby, with "…". */
function clip(text: string, room: number): string {
  const head = text.slice(0, room - 1);
  const space = head.lastIndexOf(" ");
  return `${(space > room * 0.6 ? head.slice(0, space) : head).trimEnd()}…`;
}

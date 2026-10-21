import { describe, expect, it } from "vitest";

import type { UpdateDownload, UpdateInfo, UpdateStatus } from "@/protocol/generated";

import { autoSurface, downloadOf, downloadPct, finishAction, hasUpdate, type InstallKind, notesFor, plainInline, releaseDate, summarizeNotes, visibleSurface } from "./updates";

const info = (patch: Partial<UpdateInfo> = {}): UpdateInfo => ({
  version: "0.4.0",
  tag: "v0.4.0",
  name: "SpaceKnife 0.4.0",
  published_at: "2026-10-18T09:30:00Z",
  prerelease: false,
  url: "https://github.com/DuskXi/SpaceKnife/releases/tag/v0.4.0",
  notes: "",
  asset: { name: "SpaceKnife-0.4.0-Setup-x64.exe", size: 100_000_000, url: "https://github.com/x", sha256: null },
  ...patch,
});

const dl = (patch: Partial<UpdateDownload> = {}): UpdateDownload => ({
  state: "idle",
  version: null,
  received: 0,
  total: 0,
  path: null,
  verified: null,
  error: null,
  ...patch,
});

const status = (patch: Partial<UpdateStatus> = {}): UpdateStatus => ({
  next_check_at: null,
  current: "0.3.0",
  install_kind: "nsis",
  enabled: true,
  channel: "preview",
  checking: false,
  checked_at: "2026-10-19T08:00:00Z",
  error: null,
  level: "minor",
  latest: info(),
  dismissed: false,
  download: dl(),
  ...patch,
});

describe("surfaces", () => {
  it("shows the badge for every level and nothing without a newer release", () => {
    expect(hasUpdate(null)).toBe(false);
    expect(hasUpdate(status({ level: "none", latest: null }))).toBe(false);
    expect(hasUpdate(status({ level: "minor", latest: null }))).toBe(false); // no release to show
    for (const level of ["patch", "minor", "major"] as const) expect(hasUpdate(status({ level }))).toBe(true);
  });

  it("opens the top-left card for a minor release, the dialog for a major one, nothing for a patch", () => {
    expect(autoSurface(status({ level: "patch" }))).toBe("none");
    expect(autoSurface(status({ level: "minor" }))).toBe("corner");
    expect(autoSurface(status({ level: "major" }))).toBe("dialog");
    expect(autoSurface(status({ level: "none", latest: null }))).toBe("none");
    expect(autoSurface(undefined)).toBe("none");
  });

  it("keeps a closed version closed; the card from the badge still opens", () => {
    expect(autoSurface(status({ level: "minor", dismissed: true }))).toBe("none");
    expect(autoSurface(status({ level: "major", dismissed: true }))).toBe("none");
    expect(visibleSurface(status({ level: "major", dismissed: true }), true)).toBe("anchored");
  });

  it("puts the card opened from the badge in place of the one that opened by itself", () => {
    expect(visibleSurface(status({ level: "minor" }), true)).toBe("anchored");
    expect(visibleSurface(status({ level: "major" }), false)).toBe("dialog");
    expect(visibleSurface(status({ level: "patch" }), false)).toBe("none");
    expect(visibleSurface(status({ level: "none", latest: null }), true)).toBe("none");
  });
});

describe("download", () => {
  it("is the download of the version shown, else idle", () => {
    const running = dl({ state: "downloading", version: "0.4.0", received: 25, total: 100 });
    expect(downloadOf(status({ download: running }))).toBe(running);
    expect(downloadOf(status({ download: dl({ state: "done", version: "0.3.5", path: "x" }) })).state).toBe("idle");
    expect(downloadOf(status({ download: dl({ state: "failed", error: "offline" }) })).state).toBe("failed"); // no version yet
    expect(downloadOf(null).state).toBe("idle");
  });

  it("gives whole percents of a running download only", () => {
    expect(downloadPct(dl({ state: "downloading", received: 999, total: 1000 }))).toBe(99);
    expect(downloadPct(dl({ state: "downloading", received: 1200, total: 1000 }))).toBe(100);
    expect(downloadPct(dl({ state: "downloading", received: 10, total: 0 }))).toBeNull(); // size unknown
    expect(downloadPct(dl({ state: "verifying", received: 1000, total: 1000 }))).toBe(100);
    expect(downloadPct(dl({ state: "done", received: 1000, total: 1000 }))).toBeNull();
  });

  it("starts installers, disk images and packages; shows portable copies in their folder", () => {
    expect(["nsis", "portable", "appimage"].map((k) => finishAction(k as InstallKind))).toEqual(["restart", "restart", "restart"]);
    expect(["dmg", "deb", "rpm"].map((k) => finishAction(k as InstallKind))).toEqual(["install", "install", "install"]);
    expect(["dev", "unknown"].map((k) => finishAction(k as InstallKind))).toEqual(["page", "page"]);
  });
});

describe("releaseDate", () => {
  it("gives the local date, or nothing when unknown", () => {
    const d = new Date(2026, 9, 18, 12, 0, 0);
    expect(releaseDate(d.toISOString())).toBe("2026-10-18");
    expect(releaseDate(null)).toBe("");
    expect(releaseDate("not a date")).toBe("");
  });
});

describe("plainInline", () => {
  it("keeps the text of links, images and code", () => {
    expect(plainInline("See [the docs](https://example.com/a_(b)) and ![a chart](x.png)")).toBe("See the docs and a chart");
    expect(plainInline("Use `view.set` or ``a ` b``")).toBe("Use view.set or a ` b");
    expect(plainInline("[ref link][1] <https://github.com/DuskXi/SpaceKnife>")).toBe("ref link https://github.com/DuskXi/SpaceKnife");
  });

  it("drops emphasis and HTML but not underscores inside words", () => {
    expect(plainInline("**Bold** and *italic* and __strong__ and _em_ and ~~old~~")).toBe("Bold and italic and strong and em and old");
    expect(plainInline("snake_case_name and 2 * 3 * 4")).toBe("snake_case_name and 2 * 3 * 4");
    expect(plainInline("line<br/>break <b>tag</b> &amp; &lt;x&gt; &#x4E2D;&#25991;")).toBe("line break tag & <x> 中文");
  });

  it("keeps escaped characters as written", () => {
    expect(plainInline("\\*not italic\\* and \\[not a link\\](x)")).toBe("*not italic* and [not a link](x)");
  });
});

describe("summarizeNotes", () => {
  it("turns headings, lists, quotes and tables into plain lines", () => {
    const md = [
      "## What's new",
      "",
      "- **Flame graph** view ([#12](https://github.com/x/y/pull/12))",
      "  - nested item",
      "1. numbered",
      "- [x] task done",
      "> quoted text",
      "| a | b |",
      "|---|:-:|",
      "| 1 | 2 |",
      "Setext heading",
      "---",
      "***",
      "[1]: https://example.com",
    ].join("\n");
    const { lines, cut } = summarizeNotes(md, 20, 1000);
    expect(cut).toBe(false);
    expect(lines).toEqual([
      { kind: "heading", text: "What's new", depth: 0 },
      { kind: "item", text: "Flame graph view (#12)", depth: 0 },
      { kind: "item", text: "nested item", depth: 1 },
      { kind: "item", text: "numbered", depth: 0 },
      { kind: "item", text: "task done", depth: 0 },
      { kind: "text", text: "quoted text", depth: 0 },
      { kind: "text", text: "a · b", depth: 0 },
      { kind: "text", text: "1 · 2", depth: 0 },
      { kind: "heading", text: "Setext heading", depth: 0 },
    ]);
  });

  it("leaves out code blocks and HTML comments, and says so for code", () => {
    const withCode = summarizeNotes("Intro\n```sh\nuv run spaceknife\n```\nAfter");
    expect(withCode.lines.map((l) => l.text)).toEqual(["Intro", "After"]);
    expect(withCode.cut).toBe(true);
    const comment = summarizeNotes("<!-- release template\nfill this in -->\nReal text");
    expect(comment.lines.map((l) => l.text)).toEqual(["Real text"]);
    expect(comment.cut).toBe(false);
    expect(summarizeNotes("Text\n~~~\nopen fence never closed").lines.map((l) => l.text)).toEqual(["Text"]);
  });

  it("keeps the first lines and reports the cut", () => {
    const md = Array.from({ length: 12 }, (_, i) => `- change ${i + 1}`).join("\n");
    const s = summarizeNotes(md, 8, 600);
    expect(s.lines).toHaveLength(8);
    expect(s.lines[7]!.text).toBe("change 8");
    expect(s.cut).toBe(true);
    expect(summarizeNotes(md, 12, 600).cut).toBe(false);
  });

  it("clips a long line at a word and stops at the character budget", () => {
    const long = "word ".repeat(200).trim();
    const s = summarizeNotes(`First line\n${long}\nThird`, 8, 100);
    expect(s.cut).toBe(true);
    expect(s.lines).toHaveLength(2);
    expect(s.lines[1]!.text.endsWith("word…")).toBe(true);
    expect(s.lines[1]!.text.length).toBeLessThanOrEqual(100 - "First line".length);
    // no room for a meaningful piece: the line is left out instead of clipped to a stub
    const tight = summarizeNotes(`${"x".repeat(80)}\n${long}`, 8, 100);
    expect(tight.lines.map((l) => l.text.length)).toEqual([80]);
    expect(tight.cut).toBe(true);
  });

  it("drops a heading left with nothing under it", () => {
    const s = summarizeNotes("## Added\n- a\n- b\n## Fixed\n- c", 3, 600);
    expect(s.lines.map((l) => l.text)).toEqual(["Added", "a", "b"]);
    expect(s.cut).toBe(true);
  });

  it("handles empty notes and Windows line ends", () => {
    expect(summarizeNotes("")).toEqual({ lines: [], cut: false });
    expect(summarizeNotes("# Title\r\n\r\n* one\r\n").lines).toEqual([
      { kind: "heading", text: "Title", depth: 0 },
      { kind: "item", text: "one", depth: 0 },
    ]);
  });
});

describe("notesFor", () => {
  const notes = [
    "**English** · [简体中文](#简体中文) · [Website](https://spaceknife.kaltsit.dev/)",
    "",
    "## SpaceKnife 0.4.0 (pre-release)",
    "",
    "### What's new",
    "",
    "- Help inside the app",
    "",
    "---",
    "",
    "## 简体中文",
    "",
    "### 新功能",
    "",
    "- 软件内帮助",
  ].join("\n");
  it("keeps the part in the interface language, without the language switch", () => {
    expect(notesFor(notes, "en")).toBe("## SpaceKnife 0.4.0 (pre-release)\n\n### What's new\n\n- Help inside the app");
    expect(notesFor(notes, "zh-CN")).toBe("### 新功能\n\n- 软件内帮助");
  });
  it("shows notes of one language whole", () => {
    expect(notesFor("- Fixed a crash\r\n- Faster", "zh-CN")).toBe("- Fixed a crash\n- Faster");
    expect(notesFor("", "en")).toBe("");
  });
});


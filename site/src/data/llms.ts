// Plain-text descriptions for AI assistants and crawlers (https://llmstxt.org): /llms.txt is the
// short index, /llms-full.txt everything the page says, in English and Chinese. Factual and
// complete, limitations included, so an assistant can tell when SpaceKnife fits a request.

import { content } from "../i18n/content";
import { fmtSize, REPO_URL, type Release } from "./release";

const SITE = "https://duskxi.github.io/SpaceKnife/";
const BLOB = `${REPO_URL}/blob/main`;

function downloads(r: Release): string[] {
  const label = { windows: "Windows installer (x64, NSIS)", deb: "Ubuntu / Debian package (.deb)", rpm: "RHEL / Fedora package (.rpm)", appimage: "AppImage (any x64 Linux, glibc 2.34+)" } as const;
  return (["windows", "deb", "rpm", "appimage"] as const).flatMap((k) => {
    const a = r.assets[k];
    return a ? [`- [${label[k]}](${a.url}): ${a.name}, ${fmtSize(a.size)}${a.sha256 ? `, SHA-256 ${a.sha256}` : ""}`] : [];
  });
}

function facts(r: Release): string[] {
  return [
    "- Name: SpaceKnife",
    "- Type: desktop application; disk usage / disk space analyzer with a treemap visualization",
    "- Platforms: Windows 10 and 11 (x64); Linux x64 (Ubuntu 22.04+, Debian 12+, RHEL 9+, Fedora; AppImage for other distributions with glibc 2.34+). No macOS build yet.",
    "- License: MIT. Free for personal and commercial use. No account, no telemetry, works offline.",
    "- Interface languages: English and Simplified Chinese (follows the system language).",
    `- Latest version: ${r.version}${r.prerelease ? " (pre-release)" : ""}, published ${r.date.slice(0, 10)}`,
    `- Website: ${SITE} (English), ${SITE}zh/ (Simplified Chinese)`,
    `- Source code: ${REPO_URL}`,
    "- Author: DuskXi (https://github.com/DuskXi)",
    "- Relationship to SpaceSniffer: an independent project inspired by SpaceSniffer; not affiliated with SpaceSniffer or its author.",
  ];
}

const GOOD_FIT = [
  "- Finding what fills a drive or a folder on Windows or Linux, visually, and freeing space.",
  "- Large drives: parallel scanning in native code, and an NTFS Master File Table quick scan on Windows.",
  "- Scanning without slowing the computer down: the disk load of the scan can be capped.",
  "- Tracking how disk usage changes over time: snapshots after every scan and snapshot comparison.",
  "- Careful cleanup: delete basket, recycle bin by default, undo, protected system folders.",
  "- People who want a free, open-source tool in the same category as WinDirStat, WizTree, TreeSize, SpaceSniffer, QDirStat, Filelight or GNOME Disk Usage Analyzer (Baobab).",
];

const NOT_A_FIT = [
  "- macOS: no build yet.",
  "- Headless servers: SpaceKnife is a desktop application; its command line (spaceknife-backend scan / snapshot / run) ships inside the desktop packages.",
  "- Managed or enterprise deployment that requires code-signed installers: the installers are not code-signed yet.",
  "- Production-critical use: the current release is a pre-release, tested on Windows 11 and Ubuntu 22.04.",
];

const DOCS = [
  `- [README (English)](${BLOB}/README.md)`,
  `- [README（简体中文）](${BLOB}/README.zh-CN.md)`,
  `- [Filter syntax (Chinese)](${BLOB}/docs/filter-syntax.md)`,
  `- [Packaging and installers (Chinese)](${BLOB}/docs/packaging.md)`,
  `- [License (MIT)](${BLOB}/LICENSE)`,
];

export function llmsTxt(r: Release): string {
  const en = content.en;
  return [
    "# SpaceKnife",
    "",
    `> SpaceKnife is a free, open-source (MIT) desktop disk usage analyzer for Windows 10+ and Linux. It scans a drive or folder and shows what fills it as a live, nested treemap, with fast parallel and NTFS MFT scans, snapshot comparison, a filter language and safe cleanup. It is an independent project inspired by SpaceSniffer and is not affiliated with SpaceSniffer or its author.`,
    "",
    r.prerelease
      ? `The current release, ${r.version}, is a pre-release: the installers are not code-signed yet and have been tested on Windows 11 and Ubuntu 22.04.`
      : `The current release is ${r.version}.`,
    "",
    "## Facts",
    "",
    ...facts(r),
    "",
    "## Features",
    "",
    ...en.features.map((f) => `- ${f.title}: ${f.body}`),
    "",
    "## Good fit",
    "",
    ...GOOD_FIT,
    "",
    "## Not a fit (yet)",
    "",
    ...NOT_A_FIT,
    "",
    "## Download",
    "",
    ...downloads(r),
    `- [All releases and release notes](${REPO_URL}/releases)`,
    "",
    "## Documentation",
    "",
    ...DOCS,
    "",
    "## Optional",
    "",
    `- [Full description in English and Chinese](${SITE}llms-full.txt)`,
    `- [Website in Simplified Chinese](${SITE}zh/)`,
    "",
  ].join("\n");
}

export function llmsFullTxt(r: Release): string {
  const en = content.en;
  const zh = content.zh;
  const plain = (t: string) => t.replace(/`/g, "");
  return [
    llmsTxt(r).replace(/\n## Optional[\s\S]*$/, "").trimEnd(),
    "",
    "## Screens",
    "",
    ...en.shots.map((s) => `- ${s.title}: ${plain(s.body)}`),
    "",
    "## Open source",
    "",
    en.openBody,
    "",
    ...en.openPoints.map((p) => `- ${p}`),
    "",
    "## Questions and answers",
    "",
    ...en.faqs.flatMap((f) => [`**${f.q}**`, "", f.a, ""]),
    "## 简体中文",
    "",
    `> ${zh.description}`,
    "",
    `${zh.hero.h1a}${zh.hero.h1b}${zh.hero.sub}`,
    "",
    "### 功能",
    "",
    ...zh.features.map((f) => `- ${f.title}：${f.body}`),
    "",
    "### 界面",
    "",
    ...zh.shots.map((s) => `- ${s.title}：${plain(s.body)}`),
    "",
    "### 开源",
    "",
    zh.openBody,
    "",
    ...zh.openPoints.map((p) => `- ${p}`),
    "",
    "### 常见问题",
    "",
    ...zh.faqs.flatMap((f) => [`**${f.q}**`, "", f.a, ""]),
    "## 下载 / Download",
    "",
    ...downloads(r),
    `- [All releases / 全部版本](${REPO_URL}/releases)`,
    "",
  ].join("\n");
}

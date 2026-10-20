// All text of the site, in English and Simplified Chinese. The page, the structured data (JSON-LD)
// and llms.txt read the same entries, so what people and machines are told cannot drift apart.

export type Lang = "en" | "zh";

export interface Feature {
  icon: string; // design/build/icons name
  title: string;
  body: string;
}

export interface Shot {
  id: "treemap" | "filter" | "compare" | "palette";
  title: string;
  body: string;
  alt: string;
}

export interface Faq {
  q: string;
  a: string;
}

export interface Content {
  lang: Lang;
  htmlLang: string;
  ogLocale: string;
  title: string; // <title>
  description: string; // meta description
  nav: { features: string; screenshots: string; download: string; faq: string; docs: string; github: string; switchTo: string };
  hero: {
    badge: string;
    h1a: string;
    h1b: string;
    sub: string;
    ctaWindows: string;
    ctaLinux: string;
    ctaMac: string;
    ctaOther: string;
    ctaGithub: string;
    meta: string;
    shotAlt: string;
  };
  highlights: { value: string; label: string }[];
  featuresTitle: string;
  featuresSub: string;
  features: Feature[];
  shotsTitle: string;
  shotsSub: string;
  shots: Shot[];
  mockNote: string;
  openTitle: string;
  openBody: string;
  openPoints: string[];
  openCta: string;
  downloadTitle: string;
  downloadSub: string;
  download: {
    windows: { title: string; note: string };
    portableWin: { title: string; note: string };
    macArm64: { title: string; note: string };
    macX64: { title: string; note: string };
    deb: { title: string; note: string };
    rpm: { title: string; note: string };
    appimage: { title: string; note: string };
    portableLinux: { title: string; note: string };
    button: string;
    sha: string;
    copy: string;
    copied: string;
    allReleases: string;
    checksums: string;
    prerelease: string;
    mac: string; // once a release has the dmgs
    macNone: string;
  };
  faqTitle: string;
  faqs: Faq[];
  footer: { tagline: string; disclaimer: string; license: string; agents: string; source: string };
  appCategory: string;
  featureList: string[];
}

export const content: Record<Lang, Content> = {
  en: {
    lang: "en",
    htmlLang: "en",
    ogLocale: "en_US",
    title: "SpaceKnife: free, open-source disk usage analyzer for Windows and Linux",
    description:
      "SpaceKnife shows what fills your drives as a live treemap. Fast parallel and NTFS MFT scans, snapshot comparison, filters and safe cleanup. Free and open source (MIT) for Windows 10+ and Linux.",
    nav: { features: "Features", screenshots: "Screenshots", download: "Download", faq: "FAQ", docs: "Docs", github: "GitHub", switchTo: "中文" },
    hero: {
      badge: "pre-release · free & open source",
      h1a: "See what fills",
      h1b: "your drives.",
      sub: "SpaceKnife draws every folder and file as a block sized by the space it takes, live while it scans. Find the space hogs, see what grew since last month, and clean up safely.",
      ctaWindows: "Download for Windows",
      ctaLinux: "Download for Linux",
      ctaMac: "Download for macOS",
      ctaOther: "Download",
      ctaGithub: "Star on GitHub",
      meta: "Windows 10+ · macOS 14+ · Ubuntu, Debian, RHEL, Fedora · MIT license",
      shotAlt: "SpaceKnife showing a 2 TB drive as a nested treemap, with a hover card over a large video file",
    },
    highlights: [
      { value: "MFT", label: "quick scan on NTFS" },
      { value: "Rust", label: "parallel native scanner" },
      { value: "≤ X %", label: "disk load you allow" },
      { value: "MIT", label: "free and open source" },
    ],
    featuresTitle: "Everything you need to reclaim space",
    featuresSub: "Built from first principles for today's huge drives: fast to scan, calm on your disk, safe to clean.",
    features: [
      { icon: "layers", title: "Three live visualizations", body: "A nested treemap, a flame graph and a polar flame graph, rendered with WebGL and filled in while the scan runs; switch at any time. Zoom animation, hover details, detail levels, and free and unknown space drawn too." },
      { icon: "bolt", title: "Fast scans", body: "A native Rust extension walks folders in parallel; on Windows a quick scan reads the NTFS Master File Table directly." },
      { icon: "gauge", title: "Calm on your disk", body: "Cap the disk load the scan causes (max, balanced, quiet, idle or custom); it backs off when other programs need the disk." },
      { icon: "pulse", title: "Live monitoring", body: "After the scan, changes keep flowing in and changed blocks flash, so the picture never goes stale." },
      { icon: "filter", title: "A real filter language", body: "Filter by name, type, size, dates, attributes and tags, and combine them; compatible with SpaceSniffer-style filters." },
      { icon: "diff", title: "Snapshots and comparison", body: "Every scan is saved; compare any two, or a snapshot with now, and see exactly what grew, shrank, appeared or vanished." },
      { icon: "basket", title: "Safe cleanup", body: "Collect items in a delete basket; they go to the recycle bin by default and can be restored. System folders are protected." },
      { icon: "search", title: "Command palette", body: "Ctrl+K searches the whole scan by name and runs any command, without leaving the keyboard." },
      { icon: "terminal", title: "Export and command line", body: "Export reports as CSV, JSON, HTML and more, or scan, compare and export from a terminal." },
    ],
    shotsTitle: "A closer look",
    shotsSub: "Real screens of SpaceKnife 0.3, showing a made-up 2 TB drive.",
    shots: [
      { id: "treemap", title: "The whole drive at a glance", body: "Block area is disk usage, nested folder by folder. Hover any block for its size, type and dates; the side panel lists the largest items.", alt: "Treemap of a 2 TB drive with videos, photos, games and backups as nested blocks" },
      { id: "filter", title: "Find it with a filter", body: "`:class:Video;>1gb` keeps every video over 1 GB lit and dims the rest. The matches are listed beside the treemap, sorted by size.", alt: "Filter for videos over 1 GB: matching blocks lit, others dimmed, matches listed on the right" },
      { id: "compare", title: "What changed since last month?", body: "Compare two snapshots: orange grew, blue shrank, new and deleted items are marked, and SpaceKnife estimates when free space runs out.", alt: "Snapshot comparison five weeks apart: growth in orange, shrinkage in blue" },
      { id: "palette", title: "Keyboard first", body: "The command palette searches every file and folder of the scan by name and runs any command.", alt: "Command palette searching the scan for 'mountain'" },
    ],
    mockNote: "Screenshots use mock data, not a real disk.",
    openTitle: "Open source, built in the open",
    openBody: "SpaceKnife is MIT-licensed. The scanner, the treemap layout and the filter engine run in a Python backend with a native Rust extension; the Electron interface talks to it over a documented WebSocket protocol.",
    openPoints: [
      "No account, no telemetry: everything runs locally on your computer.",
      "Python 3.12 backend, Rust (PyO3) native scanner, Electron + React + WebGL interface.",
      "More than a thousand automated tests on Windows and Linux.",
    ],
    openCta: "Read the source",
    downloadTitle: "Download SpaceKnife",
    downloadSub: "Free for personal and commercial use. Each release lists SHA-256 checksums.",
    download: {
      windows: { title: "Windows", note: "Windows 10 and 11, x64. Install for yourself or for all users." },
      portableWin: { title: "Windows · portable", note: "No installation: unzip anywhere and run. Everything stays in the data folder next to it." },
      macArm64: { title: "macOS · Apple Silicon", note: "macOS 14+ on Macs with M-series chips. Preview: not notarized yet." },
      macX64: { title: "macOS · Intel", note: "macOS 14+ on Macs with Intel chips. Preview: not notarized yet." },
      deb: { title: "Ubuntu / Debian", note: "Ubuntu 22.04+, Debian 12+ and derivatives." },
      rpm: { title: "RHEL / Fedora", note: "RHEL 9+, Rocky, Alma, Fedora and derivatives." },
      appimage: { title: "AppImage", note: "Any x64 Linux with glibc 2.34+. Needs FUSE 2." },
      portableLinux: { title: "Linux · portable", note: "x64, glibc 2.34+. Extract anywhere and run, no FUSE needed; data stays next to it." },
      button: "Download",
      sha: "SHA-256",
      copy: "Copy",
      copied: "Copied",
      allReleases: "All releases",
      checksums: "SHA256SUMS.txt",
      prerelease: "This is a pre-release: the installers are not code-signed yet, so Windows SmartScreen asks once (More info › Run anyway).",
      mac: "macOS: not notarized yet, so allow it once at the first launch (System Settings › Privacy & Security › Open Anyway).",
      macNone: "macOS is not supported yet.",
    },
    faqTitle: "Questions",
    faqs: [
      { q: "Is SpaceKnife free?", a: "Yes. SpaceKnife is free and open source under the MIT license, for personal and commercial use." },
      { q: "Is it related to SpaceSniffer?", a: "No. SpaceKnife is an independent project inspired by SpaceSniffer; it is not affiliated with SpaceSniffer or its author." },
      { q: "Which systems does it run on?", a: "Windows 10 and 11 (x64), macOS 14+ (Apple Silicon and Intel), and x64 Linux: Ubuntu 22.04+, Debian, RHEL 9+, Fedora, or any distribution through the AppImage." },
      { q: "Why does macOS block it the first time?", a: "The macOS build is not notarized by Apple yet. Open SpaceKnife once, then go to System Settings › Privacy & Security and click Open Anyway; from then on it opens normally. A notarized build is on the way." },
      { q: "Does it send my data anywhere?", a: "No. SpaceKnife has no account and no telemetry; scans, snapshots and settings stay on your computer. The only request it makes is an update check against this project's GitHub releases, at most once a day and sending nothing about you; you can turn it off in the settings." },
      { q: "Can it delete the wrong file?", a: "Deleting moves items to the recycle bin by default and can be undone; system and program folders are protected, and permanent deletion asks first." },
      { q: "Why does the quick scan ask for administrator rights?", a: "Reading the NTFS Master File Table needs administrator rights, so a small read-only helper asks once. The regular parallel scan needs no special rights." },
    ],
    footer: {
      tagline: "A modern, cross-platform disk usage analyzer.",
      disclaimer: "SpaceKnife is an independent project inspired by SpaceSniffer and is not affiliated with SpaceSniffer or its author.",
      license: "MIT license",
      agents: "For AI assistants: llms.txt",
      source: "Source on GitHub",
    },
    appCategory: "UtilitiesApplication",
    featureList: [
      "Live nested treemap of disk usage",
      "Flame graph and polar flame graph views, switchable at any time",
      "Parallel scanning with a native Rust extension",
      "NTFS Master File Table quick scan on Windows",
      "Configurable disk load limit while scanning",
      "Live file-change monitoring after the scan",
      "Filter language (name, type, size, dates, attributes, tags)",
      "Snapshots and snapshot comparison",
      "Delete basket with recycle bin and undo",
      "Command palette",
      "Export to CSV, JSON and HTML; command line",
      "English and Simplified Chinese interface",
    ],
  },
  zh: {
    lang: "zh",
    htmlLang: "zh-CN",
    ogLocale: "zh_CN",
    title: "SpaceKnife：免费开源的硬盘占用分析工具（Windows / Linux）",
    description:
      "SpaceKnife 用实时树图展示磁盘被什么占满：并行扫描与 NTFS MFT 快速扫描、快照对比、过滤语言和安全清理。免费开源（MIT），支持 Windows 10+ 与 Linux。",
    nav: { features: "功能", screenshots: "截图", download: "下载", faq: "常见问题", docs: "文档", github: "GitHub", switchTo: "English" },
    hero: {
      badge: "预发布 · 免费开源",
      h1a: "看清磁盘",
      h1b: "被什么占满。",
      sub: "SpaceKnife 把每个文件夹、每个文件画成大小与占用成正比的方块，边扫描边显示。一眼找到最占地方的东西，看出这个月多了什么，再放心地清理。",
      ctaWindows: "下载 Windows 版",
      ctaLinux: "下载 Linux 版",
      ctaMac: "下载 macOS 版",
      ctaOther: "下载",
      ctaGithub: "在 GitHub 上点星",
      meta: "Windows 10+ · macOS 14+ · Ubuntu、Debian、RHEL、Fedora · MIT 许可证",
      shotAlt: "SpaceKnife 用嵌套树图显示一块 2 TB 的磁盘，悬停在一个大视频文件上显示详情",
    },
    highlights: [
      { value: "MFT", label: "NTFS 快速扫描" },
      { value: "Rust", label: "原生并行扫描" },
      { value: "≤ X %", label: "磁盘负载由你定" },
      { value: "MIT", label: "免费开源" },
    ],
    featuresTitle: "腾出空间需要的，都在这里",
    featuresSub: "为今天的大容量磁盘重新设计：扫得快、不拖慢电脑、清理有保障。",
    features: [
      { icon: "layers", title: "三种实时可视化", body: "嵌套树图、火焰图、火焰-极坐标图，WebGL 渲染，边扫边画，扫描中也能随时切换；缩放动画、悬停详情、细节层级，可用空间与未知空间也画出来。" },
      { icon: "bolt", title: "扫描快", body: "Rust 原生扩展并行遍历目录；Windows 上可直接读取 NTFS 主文件表（MFT）快速扫描。" },
      { icon: "gauge", title: "不拖慢电脑", body: "可以限制扫描自身造成的磁盘负载（极速、均衡、安静、空闲或自定义），别的程序用磁盘时自动让路。" },
      { icon: "pulse", title: "实时监控", body: "扫描完成后继续跟踪文件变化，变化的方块会闪烁，画面不会过时。" },
      { icon: "filter", title: "真正的过滤语言", body: "按名称、类型、大小、时间、属性、标签过滤，可以组合；与 SpaceSniffer 的过滤写法兼容。" },
      { icon: "diff", title: "快照与对比", body: "每次扫描都会保存；任意两次对比，或与「现在」对比，清楚看到什么变大、变小、新增或消失。" },
      { icon: "basket", title: "安全清理", body: "先放进删除篮，默认移到回收站，可以撤销；系统和程序目录受保护。" },
      { icon: "search", title: "命令面板", body: "Ctrl+K 按名称搜索整个扫描结果，也能执行任何命令，不用离开键盘。" },
      { icon: "terminal", title: "导出与命令行", body: "导出 CSV、JSON、HTML 等报告，也可以在终端里扫描、对比和导出。" },
    ],
    shotsTitle: "细看一下",
    shotsSub: "SpaceKnife 0.3 的真实界面，显示的是一块虚构的 2 TB 磁盘。",
    shots: [
      { id: "treemap", title: "整块磁盘，一目了然", body: "方块的面积就是占用，按文件夹层层嵌套。悬停任意方块查看大小、类型和时间，右侧列出最大的子项。", alt: "一块 2 TB 磁盘的树图：视频、照片、游戏、备份以嵌套方块显示" },
      { id: "filter", title: "用过滤找到它", body: "`:class:视频;>1gb` 只亮出 1 GB 以上的视频，其余变暗；匹配结果按大小列在树图旁边。", alt: "过滤 1 GB 以上的视频：匹配的方块高亮，其余变暗，右侧列出匹配结果" },
      { id: "compare", title: "这个月多了什么？", body: "对比两次快照：橙色是增长，蓝色是减少，新增和删除的项都有标记，还会估算可用空间什么时候耗尽。", alt: "相隔五周的快照对比：增长为橙色，减少为蓝色" },
      { id: "palette", title: "键盘优先", body: "命令面板按名称搜索扫描结果里的每个文件和文件夹（支持拼音首字母），也能执行任何命令。", alt: "命令面板在扫描结果中搜索「mountain」" },
    ],
    mockNote: "截图使用模拟数据，不是真实的磁盘。",
    openTitle: "开源，公开开发",
    openBody: "SpaceKnife 使用 MIT 许可证。扫描、树图布局和过滤引擎运行在 Python 后端和 Rust 原生扩展里，Electron 界面通过有文档的 WebSocket 协议与后端通信。",
    openPoints: [
      "不需要账号，没有遥测：一切都在你的电脑上本地运行。",
      "Python 3.12 后端、Rust（PyO3）原生扫描器、Electron + React + WebGL 界面。",
      "在 Windows 和 Linux 上有上千项自动化测试。",
    ],
    openCta: "阅读源码",
    downloadTitle: "下载 SpaceKnife",
    downloadSub: "个人和商业使用都免费。每个版本都附带 SHA-256 校验和。",
    download: {
      windows: { title: "Windows", note: "Windows 10 与 11，x64。可只为自己安装，也可为所有用户安装。" },
      portableWin: { title: "Windows · 便携版", note: "免安装：解压到任意位置即可运行，数据都存在旁边的 data 文件夹里。" },
      macArm64: { title: "macOS · Apple Silicon", note: "macOS 14+，M 系列芯片的 Mac。预览版：还没经过苹果公证。" },
      macX64: { title: "macOS · Intel", note: "macOS 14+，Intel 芯片的 Mac。预览版：还没经过苹果公证。" },
      deb: { title: "Ubuntu / Debian", note: "Ubuntu 22.04+、Debian 12+ 及衍生版。" },
      rpm: { title: "RHEL / Fedora", note: "RHEL 9+、Rocky、Alma、Fedora 及衍生版。" },
      appimage: { title: "AppImage", note: "任意 x64 Linux（glibc 2.34+），需要 FUSE 2。" },
      portableLinux: { title: "Linux · 便携版", note: "x64，glibc 2.34+。解压后直接运行，不需要 FUSE；数据存在旁边。" },
      button: "下载",
      sha: "SHA-256",
      copy: "复制",
      copied: "已复制",
      allReleases: "全部版本",
      checksums: "SHA256SUMS.txt",
      prerelease: "这是预发布版：安装包还没有代码签名，Windows 第一次运行时 SmartScreen 会提示一次（更多信息 › 仍要运行）。",
      mac: "macOS 版还没经过苹果公证：第一次打开时要在「系统设置 › 隐私与安全性」里点「仍要打开」。",
      macNone: "暂不支持 macOS。",
    },
    faqTitle: "常见问题",
    faqs: [
      { q: "SpaceKnife 免费吗？", a: "免费。SpaceKnife 以 MIT 许可证开源，个人和商业使用都可以。" },
      { q: "它和 SpaceSniffer 有关系吗？", a: "没有。SpaceKnife 受 SpaceSniffer 启发，是独立开发的项目，与 SpaceSniffer 及其作者没有任何关联。" },
      { q: "支持哪些系统？", a: "Windows 10 与 11（x64）、macOS 14+（Apple Silicon 与 Intel），以及 x64 Linux：Ubuntu 22.04+、Debian、RHEL 9+、Fedora，其他发行版可以用 AppImage。" },
      { q: "为什么 macOS 第一次会拦下它？", a: "macOS 版还没有经过苹果公证。先打开一次 SpaceKnife，再到「系统设置 › 隐私与安全性」点「仍要打开」，之后就能正常打开。经过公证的版本正在准备。" },
      { q: "会上传我的数据吗？", a: "不会。SpaceKnife 不需要账号，也没有遥测；扫描结果、快照和设置都留在你的电脑上。它唯一发起的请求是检查本项目在 GitHub 上的新版本，每天最多一次，不发送任何关于你的信息，可以在设置里关掉。" },
      { q: "会不会误删文件？", a: "删除默认移到回收站，可以撤销；系统和程序目录受保护，永久删除前会再确认。" },
      { q: "为什么快速扫描要管理员权限？", a: "读取 NTFS 主文件表需要管理员权限，所以一个只读的小助手会请求一次。普通的并行扫描不需要特殊权限。" },
    ],
    footer: {
      tagline: "现代化的跨平台硬盘占用分析工具。",
      disclaimer: "SpaceKnife 受 SpaceSniffer 启发，是独立开发的项目，与 SpaceSniffer 及其作者没有任何关联。",
      license: "MIT 许可证",
      agents: "给 AI 助手：llms.txt",
      source: "GitHub 上的源码",
    },
    appCategory: "UtilitiesApplication",
    featureList: [
      "实时嵌套树图显示磁盘占用",
      "火焰图与火焰-极坐标图，随时切换",
      "Rust 原生扩展并行扫描",
      "Windows 上的 NTFS 主文件表快速扫描",
      "扫描时可限制磁盘负载",
      "扫描后实时监控文件变化",
      "过滤语言（名称、类型、大小、时间、属性、标签）",
      "快照与快照对比",
      "删除篮：移到回收站，可撤销",
      "命令面板",
      "导出 CSV、JSON、HTML；命令行",
      "英文与简体中文界面",
    ],
  },
};

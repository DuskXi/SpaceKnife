// The 「重复文件」 drawer and the other places duplicate files show (server-side render, no DOM):
// the plan before a search, the funnel and controls while it runs, the verified groups, the
// coverage bar, the hover card line, the context-menu entry and the colour segment.

import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DupeNodeInfo, DupesFolderInfo, DupesPlan } from "@/protocol/generated";
import { crossIds, dupesSummary, getDupes, resetDupes, setDupes } from "@/state/dupes";
import { Dup } from "@/treemap/paint";
import { set } from "@/state/store";
import { group, scan, status } from "@/state/testing/dupes";
import { encodeFrame } from "@/visual/testing/fixtures";
import { Kind } from "@/visual/frame";

import { CoverageBar, DupesDrawer, DupesLegend, groupName } from "./DupesDrawer";
import { dupesItem } from "./ContextMenu";
import { DupeLine, drawnCopies } from "./HoverCard";
import { colorModes } from "./Toolbar";

// a server render reads a store's initial state: these tests read the current one
vi.mock(import("@/state/store"), async (original) => {
  const store = await original();
  return { ...store, useApp: Object.assign(<T>(pick: (s: ReturnType<typeof store.get>) => T) => pick(store.get()), store.useApp) as typeof store.useApp };
});
vi.mock(import("@/state/dupes"), async (original) => {
  const d = await original();
  return { ...d, useDupes: Object.assign(<T>(pick: (s: ReturnType<typeof d.getDupes>) => T) => pick(d.getDupes()), d.useDupes) as typeof d.useDupes };
});

const html = () => renderToString(createElement(DupesDrawer));
const text = (h: string) => h.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

const plan = (over: Partial<DupesPlan> = {}): DupesPlan => ({
  candidates: 52460,
  groups: 9000,
  bytes: 2 ** 40,
  probe_bytes: 410 << 20,
  full_bytes_max: 2 ** 40,
  reclaim_max: 2 ** 39,
  excluded: { small: 1200, cloud: 31, system: 4, bins: 0, network: 0, unique: 88000 },
  cross_extra: null,
  last: null,
  current: null,
  min_size: 1 << 20,
  load: "quiet",
  scans: [{ scan_id: "s1", roots: ["s1:\\"], candidates: 52460 }],
  disks: [{ disk: "C:\\", media: "sata_ssd", scans: ["s1"] }],
  ...over,
});

beforeEach(() => {
  resetDupes();
  set({
    tabs: [{ id: "t1", kind: "scan", scanId: "s1", viewIds: ["v1"], focusedPane: 0, title: "C:" }],
    active: "t1",
    scans: { s1: scan("s1"), s2: scan("s2"), snap: scan("snap", { source: "snapshot", method: "snapshot" }) },
    settings: null,
    views: {},
  });
});
afterEach(() => vi.clearAllMocks());

describe("before a search", () => {
  it("shows what would be read, what is left out and why, and 「开始查找」", () => {
    setDupes({ plan: { s1: plan({ last: { at: new Date(Date.now() - 3 * 86400_000).toISOString(), groups: 12, files: 30, reclaim: 5 * 2 ** 30, rate: 200 << 20 } }) } });
    const h = html();
    const out = text(h);
    expect(out).toContain("52,460");
    expect(out).toContain("个文件和别的文件大小相同");
    expect(out).toContain("大小独一无二的 88,000 个文件不读");
    expect(out).toContain("云占位 31 个（读内容会触发下载）");
    expect(out).toContain("1,200 个文件小于下限 1.0 MB");
    expect(out).toMatch(/上次的结果（3 天前）：12 组，可腾出 5(\.0+)? GB · 这次先只核对文件有没有变过/);
    // the worst case, in bytes and (from the last search here) in time, with the disk
    expect(out).toMatch(/最坏要读 1(\.0+)? TB（按上次的速度最多约 88 分钟 · SATA SSD）· 最多能腾出 512(\.0+)? GB/);
    // the load level and optional caps before it starts
    expect(h).toMatch(/role="radio" aria-checked="true"[^>]*>安静</);
    expect(out).toContain("最多读");
    expect(out).toContain("开始查找");
    expect(out).toContain("跨盘比较：把打开的扫描放在一起比");
  });

  it("says when the plan could not be made, with 「重试」, instead of estimating forever", () => {
    setDupes({ planError: { s1: "有一个扫描已经关闭或不在内存里了，请重新扫描或去掉它" } });
    const out = text(html());
    expect(out).toContain("没能估计要读多少：有一个扫描已经关闭或不在内存里了");
    expect(out).toContain("重试");
    expect(out).not.toContain("正在估计要读多少");
  });

  it("lists the scans a cross-volume search would compare: only those open in a tab, each can be left out", () => {
    set({
      tabs: [
        { id: "t1", kind: "scan", scanId: "s1", viewIds: ["v1"], focusedPane: 0, title: "C:" },
        { id: "t2", kind: "scan", scanId: "s2", viewIds: ["v2"], focusedPane: 0, title: "D:" },
      ],
      settings: { values: { "dupes.cross_volume": true } } as never,
      scans: { s1: scan("s1"), s2: scan("s2"), gone: scan("gone") }, // "gone": no tab (closed, or let go)
    });
    setDupes({ plan: { s1: plan() }, leftOut: ["s2"] });
    const h = html();
    expect(text(h)).toContain("s1:\\");
    expect(text(h)).toContain("s2:\\");
    expect(text(h)).not.toContain("gone:\\");
    expect(crossIds("s1")).toEqual(["s1"]);
    setDupes({ leftOut: [] });
    expect(crossIds("s1")).toEqual(["s1", "s2"]);
  });

  it("says why a snapshot cannot be searched instead of offering it", () => {
    set({ tabs: [{ id: "t1", kind: "scan", scanId: "snap", viewIds: ["v1"], focusedPane: 0, title: "old" }] });
    const out = text(html());
    expect(out).toContain("快照是过去的状态，不能核对内容");
    expect(out).not.toContain("开始查找");
  });
});

describe("while it runs", () => {
  it("shows the funnel, the progress, the load level, pause and stop; groups come in as verified", () => {
    setDupes({ byScan: { s1: status({ state: "verifying", progress: 0.43, rate: 200 << 20, eta_max_s: 130, groups: 0, extra: 0, reclaim: 0 }) } });
    const h = html();
    const out = text(h);
    expect(out).toContain("同样大小（不读盘） 10");
    expect(out).toContain("同一个文件的几个名字算一份后 9");
    expect(out).toContain("内容相同（已读完全文核对） 2 组 · 5 个文件");
    expect(out).toContain("核对内容 43%");
    expect(out).toContain("最多还要 3 分钟");
    expect(h).toContain('aria-valuenow="43"');
    for (const level of ["空闲", "安静", "均衡", "最大"]) expect(out).toContain(level);
    expect(h).toMatch(/role="radio" aria-checked="true"[^>]*>安静</);
    expect(h).toContain('aria-label="暂停"');
    expect(h).toContain('aria-label="停止"');
    expect(out).toContain("核对完的组会出现在这里，不必等全部做完");
    expect(out).not.toContain("清除结果"); // only once it stops
  });

  it("says when a scan of the same disk makes it wait, and when another program moved access times", () => {
    setDupes({ byScan: { s1: status({ state: "waiting_scan", waiting_for: "D:", atime_moved_by_others: 3 }) } });
    const out = text(html());
    expect(out).toContain("等 D: 的扫描结束");
    expect(out).toContain("读取后有 3 个文件的访问时间变了");
  });

  it("says the speed is being estimated before the first rate", () => {
    setDupes({ byScan: { s1: status({ state: "verifying", progress: 0, rate: 0 }) } });
    expect(text(html())).toContain("速度估计中");
  });

  it("shows the cache's step between the sizes and the heads read", () => {
    setDupes({ byScan: { s1: status({ bytes: { read: 0, probe: 0, full: 0, planned_probe: 0, full_upper: 9 << 20, cache_hits: 260 } }) } });
    expect(text(html())).toContain("缓存认出（不读盘） 260");
  });
});

describe("a search that ended early", () => {
  it("stopped: the bar stays where it was, the list says the result is incomplete", () => {
    setDupes({ byScan: { s1: status({ state: "cancelled", progress: 0.3, groups: 0, extra: 0, reclaim: 0, undecided: 7, stop_reason: "cancelled", skipped_by_reason: { stopped: 7 } }) } });
    const h = html();
    const out = text(h);
    expect(h).toContain('aria-valuenow="30"');
    expect(h).toContain("dp-bar stopped");
    expect(out).toContain("已停止");
    expect(out).toContain("还有 7 个候选没核对，结果不完整");
    expect(out).toContain("查找已停止：7 个候选没核对，结果不完整");
    expect(out).not.toContain("没有找到内容相同的文件");
    expect(out).toContain("至少可腾出");
    expect(dupesSummary(getDupes().byScan.s1!)).toContain("查找重复已停止");
  });

  it("failed: a readable message (the details are in the log), never the exception", () => {
    setDupes({ byScan: { s1: status({ state: "failed", progress: 0.5, error: "查找中途出错，已核对的 116 组保留，可以再查一次（详情见日志）", stop_reason: "failed", undecided: 3 }) } });
    const out = text(html());
    expect(out).toContain("出错");
    expect(out).toContain("查找中途出错，已核对的 116 组保留");
    expect(out).not.toContain("uint8");
    expect(dupesSummary(getDupes().byScan.s1!)).toBe("查找重复出错");
  });

  it("a byte cap reached says so", () => {
    setDupes({ byScan: { s1: status({ state: "done", stop_reason: "budget", undecided: 2, progress: 0.6 }) } });
    expect(text(html())).toContain("读到上限，停了");
  });
});

describe("the results", () => {
  it("lists verified groups by the space they free, with roles, 「保留这一份」 and more pages", () => {
    setDupes({ byScan: { s1: status() }, groups: [group(1), group(2, { names: 3, cross: true, volumes: ["C:", "D:"] })], groupsTotal: 140, groupsJob: "dupes-1" });
    const h = html();
    const out = text(h);
    expect(out).toContain("可腾出");
    expect(out).toContain("3 份多余的副本 · 2 组");
    expect(out).toContain("f1.bin");
    expect(out).toContain("f2.bin（另有 2 个不同的文件名）");
    expect(out).toContain("C: · D:");
    // the verdict is an icon with its text, the keeper's reason on its role
    expect(h).toContain('aria-label="内容相同（已读完全文核对）"');
    expect(h).toMatch(/class="dp-role keep" title="按「最早创建」保留"/);
    expect(h).toContain('class="dp-role extra"');
    expect(out).toContain("保留这一份");
    expect(out).toContain("再显示 100 组");
    // no one-click deletion: the way on is the cleanup plan
    expect(out).not.toContain("加入删除篮");
    expect(out).toContain("清理方案…");
    // the filters: by name or path, the folder in view
    expect(h).toContain('placeholder="按名字或路径筛选"');
    expect(out).toContain("只看当前文件夹");
    expect(out).toContain("看过风险、确认之后才会改动文件");
    expect(out).toContain("清除结果");
    expect(out).toContain("再查一次");
    // the scope and the keep rules
    expect(out).toContain("整个扫描 · 不小于 1.0 MB");
    expect(out).toContain("同一对文件夹留在同一边");
    expect(out).not.toContain("每个卷至少留一份"); // one volume: no such rule
  });

  it("says when changes took groups apart", () => {
    setDupes({ byScan: { s1: status({ stale_groups: 2 }) } });
    expect(text(html())).toContain("2 组因为文件变了退回「待核对」，不会被删");
  });

  it("never calls one file under several names a duplicate", () => {
    setDupes({ byScan: { s1: status() }, groups: [group(3, { state: "shared", reclaim: 0 })], groupsTotal: 1, groupsJob: "dupes-1", includeShared: true });
    const h = html();
    expect(h).toContain('aria-label="同一份数据（硬链接或块克隆）：删了不腾空间"');
    expect(h).not.toContain('aria-label="内容相同（已读完全文核对）"');
  });

  it("shows another search's list as nothing, not as this one's", () => {
    setDupes({ byScan: { s1: status() }, groups: [group(1)], groupsTotal: 1, groupsJob: "dupes-9" });
    expect(text(html())).toContain("没有找到内容相同的文件");
  });

  it("offers the cross-volume rules and filter across scans, and notes copies left unlit", () => {
    setDupes({
      byScan: { s1: status({ scan_ids: ["s1", "s2"], cross_volume: true, ledger: [{ volume: "C:", extra: 1, reclaim: 1 << 20, shared: 0, groups: 1, note: "" }, { volume: "D:", extra: 2, reclaim: 2 << 20, shared: 0, groups: 1, note: "这个卷能共享数据块" }] }) },
      located: { v1: { seq: 1, rows: [], truncated: true } },
    });
    const out = text(html());
    expect(out).toContain("跨盘：s1:\\ · s2:\\");
    expect(out).toContain("每个卷至少留一份");
    expect(out).toContain("只看跨盘");
    expect(out).toContain("D: · 这个卷能共享数据块");
    expect(out).toContain("只点亮最大的 2,000 份");
  });

  it("lists the folders holding the most extra copies", () => {
    setDupes({ byScan: { s1: status() }, tab: "folders", folders: [{ node: 4, path: "C:\\backup\\2019", redundant: 3 << 20, alloc: 4 << 20, twin: "C:\\photos\\2019", twin_scan_id: "s1", twin_node: 7, twin_bytes: 3 << 20 }], foldersTotal: 3 });
    const h = html();
    expect(text(h)).toContain("C:\\backup\\2019");
    expect(text(h)).toContain("75% 在 C:\\photos\\2019 也有"); // the folder it repeats
    expect(h).toContain("width:75%");
    expect(text(h)).toContain("再显示 2 个文件夹"); // a real button now
  });
});

describe("the coverage bar", () => {
  const info: DupesFolderInfo = {
    node: 9,
    redundant: 1 << 20,
    groups: 1,
    only_here: 2,
    only_here_bytes: 3 << 20,
    elsewhere: 1,
    elsewhere_bytes: 1 << 20,
    unchecked: 0,
    unchecked_bytes: 0,
    same_data: 0,
    same_data_bytes: 0,
    small: 5,
    small_bytes: 4096,
    only_here_list: [{ node: 11, path: "C:\\photos\\a.cr3", alloc: 3 << 20 }],
    elsewhere_list: [{ scan_id: "s1", node: 12, path: "D:\\backup\\photos", bytes: 1 << 20 }],
  };

  it("splits the folder by bytes into elsewhere, only here, the same data and unchecked", () => {
    const h = renderToString(createElement(CoverageBar, { info, label: "photos" }));
    const out = text(h);
    expect(out).toContain("在别处有一份 1.0 MB");
    expect(out).toContain("只在这里 3.0 MB");
    expect(out).toContain("没核对 4.0 KB");
    expect(out).toContain("其中 5 个小于下限，没有核对");
    expect(out).toContain("C:\\photos\\a.cr3");
    expect(out).toContain("删除整个文件夹不在这里做");
    expect(h.split("dp-cover-legend")[0]).not.toContain("dp-c-same"); // no bytes, no segment; the legend still names it
    expect(out).toContain("同一份数据 0 B");
    expect(h).toContain('class="dp-c-only"'); // 「只在这里」 has its colour (a real token, test in app.css)
    expect(out).toContain("别处的副本主要在：");
    expect(out).toContain("D:\\backup\\photos");
    expect(out).toContain("这个文件夹在别处有没有？");
  });

  it("shows for a search limited to a folder", () => {
    setDupes({ byScan: { s1: status({ mode: "against", node: 9 }) }, coverage: { "dupes-1": { node: 9, info } } });
    const out = text(html());
    expect(out).toContain("这个文件夹在别处有没有");
    expect(out).toContain("只在这里 3.0 MB");
  });
});

describe("the hover card line", () => {
  const d = (over: Partial<DupeNodeInfo>): DupeNodeInfo => ({ state: "none", group: null, role: null, why: null, copies: 0, reclaim: 0, others: [], other_volumes: [], redundant: 0, reason: "", ...over });
  const line = (info: DupeNodeInfo, drawn: number | null = null) => text(renderToString(createElement(DupeLine, { d: info, drawn })));

  it("names verified copies, where the others are and whether this one goes", () => {
    const others = [{ scan_id: "s1", node: 2, path: "C:\\b\\x.bin", volume: "C:", mtime: 0, ctime: 0, alloc: 1, role: "keep" as const, why: null, view: false }];
    const extra = line(d({ state: "extra", copies: 3, reclaim: 96 << 20, others, other_volumes: ["D:"] }), 1);
    expect(extra).toContain("另有 2 份内容相同 · 删掉多余的能腾出 96.0 MB");
    expect(extra).toContain("C:\\b\\x.bin");
    expect(extra).toContain("其中有的在 D:");
    expect(extra).toContain("另 2 份不在画面里");
    expect(extra).toContain("这一份是多余的");
    expect(line(d({ state: "keep", copies: 2, why: "rule" }))).toContain("按「最早创建」保留这一份");
    expect(line(d({ state: "keep", copies: 2, why: "manual" }))).toContain("手动选择保留");
  });

  it("makes no claim while checking, and tells the same data and skips apart", () => {
    expect(line(d({ state: "pending" }))).toBe(" 同样大小的文件正在核对内容… ");
    expect(line(d({ state: "shared", others: [{ scan_id: "s1", node: 2, path: "C:\\x", volume: "", mtime: 0, ctime: 0, alloc: 0, role: "alias", why: null, view: false }] }))).toContain("和 C:\\x 是同一份数据（硬链接或块克隆），删了不腾空间");
    expect(line(d({ state: "skipped", reason: "云占位" }))).toContain("没有比较：云占位");
    expect(line(d({ state: "none", redundant: 300 << 20 }))).toContain("里面有多余的副本 300.0 MB");
    expect(line(d({ state: "none" }))).toBe("");
    expect(line(d({ state: "unique" }))).toContain("比较过：没有别的文件和它内容相同");
  });

  it("counts the copies the picture draws from the group column", () => {
    const f = encodeFrame("drawlist.v1", [0, 1, 2, 3].map((i) => ({ key: i + 1, node: i + 1, parent: -1, kind: Kind.FILE, depth: 1, geo: [i * 10, 0, 10, 10] as const, cls: 0, size: 1n }))).frame;
    expect(drawnCopies(f, 0)).toBeNull(); // no column
    f.dupg = Uint32Array.of(4, 4, 0, 4);
    expect(drawnCopies(f, 1)).toBe(3);
    expect(drawnCopies(f, 2)).toBeNull();
  });
});

describe("the context menu entry", () => {
  const f = encodeFrame("drawlist.v1", [{ key: 1, node: 1, parent: -1, kind: Kind.FILE, depth: 1, geo: [0, 0, 10, 10] as const, cls: 0, size: 1n, name: "a.bin" }]).frame;

  it("asks about a folder, or finds a file's twins; greyed with the reason where it cannot", () => {
    expect(dupesItem("s1", "v1", true, 5, f, 0, undefined)).toMatchObject({ label: "这个文件夹在别处有没有？", disabled: undefined });
    expect(dupesItem("s1", "v1", false, 5, f, 0, undefined)).toMatchObject({ label: "查找与它相同的文件", disabled: undefined });
    expect(dupesItem("snap", "v1", false, 5, f, 0, undefined).disabled).toBe("快照是过去的状态，不能核对内容");
    expect(dupesItem("s1", "v1", false, 5, f, 0, "已经不在磁盘上了").disabled).toBe("已经不在磁盘上了");
  });

  it("lights a verified file's group, and does not replace a finished search for a file without one", () => {
    setDupes({ byScan: { s1: status() } });
    // what the frame's dup column says decides the reason: no claim of 「没有相同的」 unless compared
    f.dup = Uint8Array.of(Dup.UNIQUE);
    expect(dupesItem("s1", "v1", false, 5, f, 0, undefined).disabled).toBe("这次查找里没有和它内容相同的文件");
    f.dup = Uint8Array.of(Dup.SKIPPED);
    expect(dupesItem("s1", "v1", false, 5, f, 0, undefined).disabled).toBe("这次查找跳过了它（原因见悬停卡）");
    f.dup = Uint8Array.of(Dup.NONE);
    expect(dupesItem("s1", "v1", false, 5, f, 0, undefined).disabled).toBe("这次查找没有比较它（小于下限，或不在比较范围里）");
    setDupes({ byScan: { s1: status({ state: "cancelled", undecided: 4 }) } });
    expect(dupesItem("s1", "v1", false, 5, f, 0, undefined).disabled).toBe("这次查找没核对完，可以「再查一次」");
    setDupes({ byScan: { s1: status() } });
    f.dupg = Uint32Array.of(8);
    const item = dupesItem("s1", "v1", false, 5, f, 0, undefined);
    expect(item.label).toBe("另有相同内容的文件 · 点亮它们");
    item.run!();
    expect(getDupes().focus).toEqual({ jobId: "dupes-1", group: 7 });
    expect(getDupes().open).toBe(true);
    setDupes({ byScan: { s1: status({ state: "verifying" }) } });
    expect(dupesItem("s1", "v1", true, 5, f, 0, undefined).disabled).toBe("先等这次查找结束");
  });
});

describe("the colour segment", () => {
  it("offers 「重复」 only while results cover the scan", () => {
    expect(colorModes(false)).toEqual(["class", "flat", "time", "branch"]);
    expect(colorModes(undefined)).not.toContain("dupes");
    expect(colorModes(true)).toEqual(["class", "flat", "time", "branch", "dupes"]);
  });
});

describe("group names", () => {
  it("say how many names the copies go by", () => {
    expect(groupName(group(1))).toBe("f1.bin");
    expect(groupName(group(1, { names: 2 }))).toBe("f1.bin（另有 1 个不同的文件名）");
  });
});

describe("the copies of a group", () => {
  it("show what tells them apart, not the same shortened path twice", () => {
    const g = group(4, {
      members: [
        { scan_id: "s1", node: 1, path: "C:\\Users\\leizi\\Downloads\\Projects\\site\\assets\\video\\intro.mp4", volume: "C:", mtime: 0, ctime: 0, alloc: 1, role: "keep", why: "rule", view: false },
        { scan_id: "s1", node: 2, path: "C:\\Users\\leizi\\Downloads\\Projects\\site-old\\assets\\video\\intro.mp4", volume: "C:", mtime: 0, ctime: 0, alloc: 1, role: "extra", why: null, view: false },
      ],
    });
    setDupes({ byScan: { s1: status() }, groups: [g], groupsTotal: 1, groupsJob: "dupes-1" });
    const out = text(html());
    expect(out).toContain("C:\\…\\site\\…\\intro.mp4");
    expect(out).toContain("C:\\…\\site-old\\…\\intro.mp4");
  });

  it("let a copy in another scan be clicked (its tab), and mark another path of the same file", () => {
    const g = group(5, {
      cross: true,
      volumes: ["C:", "D:"],
      members: [
        { scan_id: "s1", node: 1, path: "C:\\a\\x.bin", volume: "C:", mtime: 0, ctime: 0, alloc: 1, role: "keep", why: "rule", view: false },
        { scan_id: "s2", node: 2, path: "D:\\b\\x.bin", volume: "D:", mtime: 0, ctime: 0, alloc: 1, role: "extra", why: null, view: false },
        { scan_id: "s3", node: 3, path: "C:\\sub\\a\\x.bin", volume: "C:", mtime: 0, ctime: 0, alloc: 1, role: "keep", why: "rule", view: true },
      ],
    });
    setDupes({ byScan: { s1: status() }, groups: [g], groupsTotal: 1, groupsJob: "dupes-1" });
    const h = html();
    expect(h).toContain("切换到它的页签并找到它：D:\\b\\x.bin");
    expect(h).not.toMatch(/dp-path"[^>]*disabled/);
    expect(text(h)).toContain("同一个文件");
  });
});

describe("the colouring's key", () => {
  it("names every file state and the folder tiers with their thresholds", () => {
    const out = text(renderToString(createElement(DupesLegend)));
    for (const s of ["多余的副本（可删）", "保留的一份", "待核对", "同一份数据（删了不腾空间）", "没有重复，或没比较", "文件夹：多余副本占的比例", "≥ 10%", "≥ 40%", "≥ 80%"]) expect(out).toContain(s);
  });
});

// Cleanup mode rendered (server-side, no DOM): only the review page runs a plan, and only when the
// risk check is done and every high risk acknowledged; the last confirmation starts on 「取消」 and
// says where the originals go; impossible actions stay in the list, greyed, with their reason; the
// last keeper cannot be switched off; previews name risks by their title; the layout folds by width.

import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { resetCleanup, setCleanup } from "@/state/cleanup";
import { bucket, copy, cstate, group, summary } from "@/state/testing/cleanup";
import type { Tab } from "@/state/store";

import { CleanupPage, layoutFor, pairLabels } from "./CleanupPage";
import { CleanupDialogs } from "./Dialogs";
import { GroupTable } from "./GroupTable";
import { History } from "./History";
import { ActionPicker, pickerChoices } from "./parts";
import { ReviewPage } from "./Review";

vi.mock(import("@/state/cleanup"), async (original) => {
  const c = await original();
  return { ...c, useCleanup: Object.assign(<T>(pick: (s: ReturnType<typeof c.getCleanup>) => T) => pick(c.getCleanup()), c.useCleanup) as typeof c.useCleanup };
});

const text = (h: string) => h.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/&quot;/g, '"').replace(/&gt;/g, ">").replace(/&lt;/g, "<").replace(/&amp;/g, "&").replace(/\s+/g, " ");
const review = () => renderToString(createElement(ReviewPage));
const dialogs = () => renderToString(createElement(CleanupDialogs));
const execButton = (h: string) => /<button class="primary cl-exec"( disabled="")?/.exec(h)![1] === undefined;
const TAB: Tab = { id: "t1", kind: "scan", scanId: "s1", viewIds: ["v1"], focusedPane: 0, title: "C:" } as Tab;

beforeEach(() => resetCleanup());

describe("the review page", () => {
  it("runs nothing while a high risk is not acknowledged or the risk check goes on", () => {
    setCleanup({ planId: "plan-1", state: cstate({ summary: summary({ buckets: [bucket()] }) }) });
    let h = review();
    expect(text(h)).toContain("要你确认（高）");
    expect(text(h)).toContain("点「看这些」，看过之后才能确认");
    expect(h).not.toContain("我知道了，照这样做"); // the tick only under the list of copies
    expect(execButton(h)).toBe(false);
    setCleanup({ state: cstate({ facts_done: 1, facts_total: 4, summary: summary({ gates: ["还在核对权限和附加数据（25%）"] }) }) });
    h = review();
    expect(text(h)).toContain("还在核对权限和附加数据（25%）");
    expect(execButton(h)).toBe(false);
    setCleanup({ state: cstate({ summary: summary({ buckets: [bucket({ acked: true, unacked: 0 })] }) }) });
    expect(execButton(review())).toBe(true);
  });

  it("the acknowledgement sits under the copies it covers", () => {
    const copies = { version: 3, risk: bucket(), total: 1, items: [group(1).members[1]!] };
    setCleanup({ planId: "plan-1", state: cstate({ summary: summary({ buckets: [bucket()] }) }), riskOpen: copies });
    const h = text(review());
    expect(h.indexOf("C:\\copy\\f1.jpg")).toBeGreaterThan(-1);
    expect(h.indexOf("我知道了，照这样做（3 份）")).toBeGreaterThan(h.indexOf("C:\\copy\\f1.jpg"));
  });

  it("says what is freed at once and once emptied, and offers no script export", () => {
    setCleanup({ planId: "plan-1", state: cstate() });
    const h = text(review());
    expect(h).toContain("清空或清除之后");
    expect(h).toContain("（清空回收站后）");
    expect(h).toContain("不提供「导出成脚本」");
  });

  it("shows a preflight only for the version it checked", () => {
    const task = { task_id: "c-1", kind: "preflight" as const, phase: "preflight", state: "done" as const, done: 2, total: 2, bytes: 0, current: "", counts: { would_do: 2 }, freed_now: 0, freed_later: 0, run_id: "", plan_id: "plan-1", message: "", skipped: [], needs_confirm: false };
    const approval = { token: "t", version: 3, steps: 2, by_action: {}, acks: {}, at: "" };
    setCleanup({ planId: "plan-1", state: cstate({ approval }), task });
    expect(text(review())).toContain("预检：2 项可以做");
    setCleanup({ state: cstate({ version: 4, approval }) });
    expect(text(review())).not.toContain("预检：");
  });
});

describe("the confirmations", () => {
  it("running starts on 「取消」 and says where the originals go", () => {
    setCleanup({ planId: "plan-1", state: cstate(), dialog: { kind: "execute" } });
    const h = dialogs();
    const buttons = [...h.matchAll(/<button[^>]*>([^<]*)/g)].map((m) => m[0]);
    expect(buttons[0]).toContain("autofocus");
    expect(text(buttons[0]!)).toContain("取消");
    // only the recycle bin here: no recovery area to speak of
    expect(text(h)).toContain("移到回收站的在回收站里");
    expect(text(h)).not.toContain("恢复区");
    setCleanup({ state: cstate({ summary: summary({ by_action: [{ action: "hardlink", copies: 2, groups: 2, freed_now: 0, freed_later: 2 << 20 }] }) }) });
    expect(text(dialogs())).toContain("原件先放在恢复区");
  });

  it("emptying says it cannot be undone and what is checked first", () => {
    setCleanup({ planId: "plan-1", state: cstate(), dialog: { kind: "purge", runId: "r-1", plan: { run_id: "r-1", items: [], can: 2, cannot: 1, bytes: 5 << 20, streams: 1, in_trash: 4 }, ackStreams: false } });
    const h = text(dialogs());
    expect(h).toContain("这一步不能撤销");
    expect(h).toContain("核对每一份原件没有变过");
    expect(h).toContain("SpaceKnife 不替你清空回收站");
    expect(h).toContain("5 秒后清除 2 份");
  });

  it("a preview names risks by title and says what a risk rule turned into 「不动」", () => {
    const diff = {
      version: 3,
      before: { trash: 3 },
      after: { leave: 3 },
      freed_before: 3,
      freed_after: 0,
      unavailable: { risk_rule: 3, fs_no_clone: 1 },
      risks_added: { R02: 3 },
      risks_removed: { R08: 3 },
      risk_titles: { R02: "像一份备份", R08: "保留那份最近还改过" },
      protected: 0,
      changed: 3,
      examples: [{ path: "C:\\Users\\alice\\Pictures\\old\\bg.jpg", before: "trash", after: "leave" }],
      refused: "",
    };
    setCleanup({ planId: "plan-1", state: cstate(), dialog: { kind: "preview", edit: { op: "action_set", action: "hardlink", scope: { groups: [1] } } as never, diff, label: "" } });
    const h = text(dialogs());
    expect(h).toContain("像一份备份 · 3");
    expect(h).toContain("保留那份最近还改过 · 3");
    expect(h).not.toMatch(/R0\d×/);
    expect(h).toContain("3 份有高风险，规则「任何高风险 → 不动」把它们改成了不动");
    expect(h).toContain("文件系统不能共用数据块 · 1");
  });
});

describe("the group table", () => {
  it("keeps impossible actions in the list, greyed, with the reason", () => {
    const choices = pickerChoices(copy());
    expect(choices.find((c) => c.action === "clone")).toMatchObject({ disabled: true, why: expect.stringContaining("NTFS 不支持共用数据块") });
    expect(choices.filter((c) => c.disabled).map((c) => c.action)).toEqual(["clone"]);
    const h = renderToString(createElement(ActionPicker, { copy: copy(), onPick: () => undefined, initialOpen: true }));
    expect(h).toContain('role="listbox"');
    expect(h).toMatch(/id="cl-pick-clone" role="option" aria-selected="false" aria-disabled="true"/);
    expect(h).toMatch(/id="cl-pick-trash" role="option" aria-selected="true"/);
    expect(text(h)).toContain("NTFS 不支持共用数据块");
  });

  it("the last keeper cannot be switched off, and says so", () => {
    setCleanup({ planId: "plan-1", state: cstate(), groups: [group(1)], total: 1, expanded: [1] });
    const h = renderToString(createElement(GroupTable)).replace(/<!-- -->/g, "");
    expect(h).toMatch(/aria-label="每组至少留一份：先选另一份做保留点"[^>]*disabled=""/);
    const kept = h.slice(h.indexOf('class="cl-copy kept last"'), h.indexOf('class="cl-copy"'));
    expect(text(kept)).toContain("唯一的保留点");
    expect(kept).not.toContain("只留它"); // already the only one
    expect(text(h)).toContain("只留它"); // on the other copy
  });

  it("links point at the keeper by its short path", () => {
    const base = "C:\\Users\\alice\\Documents\\Projects\\website\\assets\\images";
    const g = group(1, {
      members: [copy({ row: 10, path: `${base}\\Pictures\\bg.jpg`, keep: "keep", action: null, available: {} }), copy({ row: 11, path: `${base}\\Temp\\bg.jpg`, action: "hardlink", target: `${base}\\Pictures\\bg.jpg` })],
    });
    setCleanup({ planId: "plan-1", state: cstate(), groups: [g], total: 1, expanded: [1] });
    const h = text(renderToString(createElement(GroupTable)));
    expect(h).toMatch(/→ \S*…\S*\\Pictures\\bg\.jpg/);
    expect(h).not.toContain(`→ ${base}`);
  });

  it("says why it is empty and the way back", () => {
    setCleanup({ planId: "plan-1", state: cstate({ summary: summary({ in_plan: 0, not_done: [{ reason: "cleaned", text: "已清理", count: 4, unit: "groups" }] }) }), groups: [], total: 0 });
    let h = text(renderToString(createElement(GroupTable)));
    expect(h).toContain("方案里没有要处理的组");
    expect(h).toContain("已清理 · 4 组");
    expect(h).toContain("回到查找结果");
    setCleanup({ filter: "ext:psd" });
    h = text(renderToString(createElement(GroupTable)));
    expect(h).toContain("没有符合筛选的组");
  });
});

describe("the page", () => {
  it("folds by its own width", () => {
    expect([1600, 1280, 1279, 900, 899, 600].map(layoutFor)).toEqual(["wide", "wide", "medium", "medium", "narrow", "narrow"]);
  });

  it("names the two folders of a pair from where they part", () => {
    expect(pairLabels("C:\\Users\\alice\\Pictures\\Backup\\2021", "C:\\Users\\alice\\Pictures\\2021")).toEqual(["…\\Pictures\\Backup\\2021", "…\\Pictures\\2021"]);
    expect(pairLabels("/home/alice/Videos", "/home/alice/Videos/old")).toEqual(["…/Videos", "…/Videos/old"]);
    expect(pairLabels("C:\\a", "D:\\a")).toEqual(["C:\\a", "D:\\a"]); // nothing in common: as they are
  });

  it("names the severities in words and shows the terms the filter left out", () => {
    setCleanup({ planId: "plan-1", jobId: "dupes-1", state: cstate({ summary: summary({ by_severity: { none: 0, info: 0, low: 2, medium: 5, high: 1, blocked: 0 } }) }), problems: ["看不懂「size:>>2MB」：要写数值"] });
    const h = text(renderToString(createElement(CleanupPage, { tab: TAB })));
    expect(h).toContain("▲ 高 1");
    expect(h).toContain("■ 中 5");
    expect(h).toContain("○ 低 2");
    expect(h).toContain("看不懂「size:>>2MB」：要写数值");
  });

  it("lists the keys, none of which runs, undoes or empties", () => {
    setCleanup({ planId: "plan-1", state: cstate(), keysHelp: true });
    const h = text(renderToString(createElement(CleanupPage, { tab: TAB })));
    expect(h).toContain("组表的按键");
    expect(h).toContain("这组只保留第几份");
    expect(h).toContain("执行、撤销、清除都没有按键");
  });
});

describe("the history", () => {
  it("says what became of each item (an undone item is not 「完成」)", () => {
    const run = { run_id: "r-1", started: "2026-10-24T16:58:18+00:00", finished: "", state: "undone" as const, roots: ["C:\\Users\\alice"], by_action: { trash: 1 }, counts: { done: 1 }, freed_now: 0, held: 0, held_by_volume: {}, in_trash: 0, can_undo: 0, can_purge: 0, attention: 0, purge_reminder: false };
    setCleanup({ runs: [run], run: { run, total: 1, items: [{ seq: 1, action: "trash", path: "C:\\Users\\alice\\a.jpg", target: "", code: "done", text: "完成", state: "undone", recovery: "", freed: 1 }] } });
    const h = text(renderToString(createElement(History)));
    expect(h).toContain("已撤销");
    expect(h).toMatch(/2026-10-2\d \d\d:58:18/); // in this computer's time zone
  });
});

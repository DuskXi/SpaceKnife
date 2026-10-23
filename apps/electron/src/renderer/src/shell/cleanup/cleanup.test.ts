// Cleanup mode rendered (server-side, no DOM): only the review page runs a plan, and only when the
// risk check is done and every high risk acknowledged; the last confirmation starts on 「取消」;
// impossible actions are greyed with their reason; the last keeper cannot be switched off; the
// drawer has no one-click deletion.

import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { resetCleanup, setCleanup } from "@/state/cleanup";
import { bucket, cstate, group, summary } from "@/state/testing/cleanup";

import { CleanupDialogs } from "./Dialogs";
import { GroupTable } from "./GroupTable";
import { ReviewPage } from "./Review";

vi.mock(import("@/state/cleanup"), async (original) => {
  const c = await original();
  return { ...c, useCleanup: Object.assign(<T>(pick: (s: ReturnType<typeof c.getCleanup>) => T) => pick(c.getCleanup()), c.useCleanup) as typeof c.useCleanup };
});

const text = (h: string) => h.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");
const review = () => renderToString(createElement(ReviewPage));
const execButton = (h: string) => /<button class="primary cl-exec"( disabled="")?/.exec(h)![1] === undefined;

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
});

describe("the confirmations", () => {
  it("running starts on 「取消」", () => {
    setCleanup({ planId: "plan-1", state: cstate(), dialog: { kind: "execute" } });
    const h = renderToString(createElement(CleanupDialogs));
    const buttons = [...h.matchAll(/<button[^>]*>([^<]*)/g)].map((m) => m[0]);
    expect(buttons[0]).toContain("autofocus");
    expect(text(buttons[0]!)).toContain("取消");
    expect(text(h)).toContain("原件先放在恢复区，可以撤销");
  });

  it("emptying says it cannot be undone and what is checked first", () => {
    setCleanup({ planId: "plan-1", state: cstate(), dialog: { kind: "purge", runId: "r-1", plan: { run_id: "r-1", items: [], can: 2, cannot: 1, bytes: 5 << 20, streams: 1, in_trash: 4 }, ackStreams: false } });
    const h = text(renderToString(createElement(CleanupDialogs)));
    expect(h).toContain("这一步不能撤销");
    expect(h).toContain("核对每一份原件没有变过");
    expect(h).toContain("SpaceKnife 不替你清空回收站");
    expect(h).toContain("5 秒后清除 2 份");
  });
});

describe("the group table", () => {
  it("greys what cannot be done here, with the reason, and keeps the last keeper", () => {
    setCleanup({ planId: "plan-1", state: cstate(), groups: [group(1)], total: 1, expanded: [1] });
    const h = renderToString(createElement(GroupTable)).replace(/<!-- -->/g, "");
    expect(h).toMatch(/<option value="clone" disabled="">[^<]*NTFS 不支持共用数据块/);
    expect(h).toMatch(/aria-label="每组至少留一份：先选另一份做保留点"[^>]*disabled=""/);
    expect(text(h)).toContain("只留它");
  });
});

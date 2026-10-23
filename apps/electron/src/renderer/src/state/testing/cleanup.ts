// Test fixtures of the cleanup plan's protocol models.

import type { CleanupBucket, CleanupCopy, CleanupGroup, CleanupState, CleanupSummary } from "@/protocol/generated";

export function copy(over: Partial<CleanupCopy> = {}): CleanupCopy {
  return {
    row: 0,
    scan_id: "s1",
    node: 10,
    path: "C:\\k\\a.jpg",
    volume: "C:",
    fs: "ntfs",
    keep: "none",
    action: "trash",
    target: "",
    symlink_text: "",
    source: "规则 1：其余 → 回收站", // i18n-ignore
    source_code: "action_rule",
    manual: false,
    available: { leave: "", trash: "", hardlink: "", symlink: "", clone: "NTFS 不支持共用数据块（ReFS、btrfs、xfs（开了 reflink）、bcachefs 可以）" }, // i18n-ignore
    risks: [],
    severity: "none",
    freed_now: 0,
    freed_later: 1 << 20,
    alloc: 1 << 20,
    mtime: 0,
    btime: 0,
    view_of: "",
    tags: [],
    ...over,
  };
}

export function group(id: number, over: Partial<CleanupGroup> = {}): CleanupGroup {
  return {
    id,
    digest16: "00112233aabbccdd",
    name: `f${id}.jpg`,
    names: 1,
    size: 1 << 20,
    copies: 2,
    keepers: 1,
    actions: { keep: 1, trash: 1 },
    freed_now: 0,
    freed_later: 1 << 20,
    severity: "none",
    risk_count: 0,
    source: "rule",
    reviewed: false,
    changed_since_review: false,
    state: "in",
    keep_line: "按「最早创建」保留 C:\\k\\a.jpg", // i18n-ignore
    cross: false,
    volumes: ["C:"],
    conflict: false,
    only_avoided: false,
    safety_net: false,
    members: [copy({ row: id * 10, path: `C:\\k\\f${id}.jpg`, keep: "keep", action: null, available: {} }), copy({ row: id * 10 + 1, path: `C:\\copy\\f${id}.jpg` })],
    total_members: 2,
    ...over,
  };
}

export function bucket(over: Partial<CleanupBucket> = {}): CleanupBucket {
  return { id: "T01", code: "sync_delete", severity: "high", title: "删除会同步到云端", copies: 3, groups: 3, bytes: 3 << 20, acked: false, unacked: 3, example: "删除会同步到 OneDrive 的云端", ...over }; // i18n-ignore
}

export function summary(over: Partial<CleanupSummary> = {}): CleanupSummary {
  return {
    version: 3,
    groups: 2,
    copies: 2,
    freed_now: 0,
    freed_later: 2 << 20,
    by_action: [{ action: "trash", copies: 2, groups: 2, freed_now: 0, freed_later: 2 << 20 }],
    by_volume: [{ volume: "C:", fs: "ntfs", freed_now: 0, freed_later: 2 << 20, copies: 2, recovery: "C:\\.SpaceKnife-recovery", trash: "", snapshots_note: "" }],
    by_severity: { none: 2, info: 0, low: 0, medium: 0, high: 0, blocked: 0 },
    buckets: [],
    not_done: [],
    gates: [],
    reviewed: 0,
    in_plan: 2,
    compare_bytes: 0,
    ...over,
  };
}

export function cstate(over: Partial<CleanupState> = {}): CleanupState {
  return {
    plan_id: "plan-1",
    job_id: "dupes-1",
    scan_ids: ["s1"],
    version: 3,
    preset: "recommended",
    criteria: ["oldest_created", "plain_name", "shortest_path"],
    per_volume: true,
    folder_consistent: true,
    default_action: "trash",
    options: { on_failure: "continue", verify: "auto", symlink_style: "auto" },
    keep_rules: [],
    action_rules: [],
    pairs: [],
    auto_pairs: 0,
    manual: 0,
    summary: summary(),
    facts_done: 4,
    facts_total: 4,
    approval: null,
    can_undo: false,
    can_redo: false,
    resume_offer: null,
    platform: "win32",
    symlink_right: true,
    macos_links: false,
    running: "",
    ...over,
  };
}

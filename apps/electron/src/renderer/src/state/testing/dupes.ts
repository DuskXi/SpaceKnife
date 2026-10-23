// Duplicate-file search fixtures for the renderer's tests: a status, a verified group, a scan.

import type { DupeGroup, DupesStatus, ScanStatus } from "@/protocol/generated";

export const status = (over: Partial<DupesStatus> = {}): DupesStatus =>
  ({
    job_id: "dupes-1",
    state: "done",
    scan_ids: ["s1"],
    mode: "all",
    node: null,
    min_size: 1 << 20,
    cross_volume: false,
    progress: 1,
    funnel: { candidates: 10, identified: 10, physical: 9, aliases: 1, clones: 0, probed: 9, verifying: 0, confirmed_groups: 2, confirmed_files: 5, shared_groups: 0, unique: 0, skipped: 0 },
    bytes: { read: 3 << 20, probe: 1 << 16, full: 3 << 20, planned_probe: 0, full_upper: 9 << 20, cache_hits: 0 },
    rate: 0,
    eta_max_s: null,
    load: "quiet",
    waiting_for: "",
    ledger: [],
    skipped_by_reason: {},
    atime_unkept: 0,
    atime_moved_by_others: 0,
    groups: 2,
    stale_groups: 0,
    extra: 3,
    reclaim: 3 << 20,
    version: 1,
    error: "",
    stop_reason: "",
    undecided: 0,
    elapsed_s: 1,
    ...over,
  }) as DupesStatus;

export const group = (id: number, over: Partial<DupeGroup> = {}): DupeGroup => ({
  id,
  size: 1 << 20,
  copies: 2,
  names: 1,
  reclaim: 1 << 20,
  shared: 0,
  state: "confirmed",
  volumes: ["C:"],
  cross: false,
  keep_why: "rule",
  members: [
    { scan_id: "s1", node: 10 + id, path: `C:\\a\\f${id}.bin`, volume: "C:", mtime: 0, ctime: 0, alloc: 1 << 20, role: "keep", why: "rule", view: false },
    { scan_id: "s1", node: 20 + id, path: `C:\\b\\f${id}.bin`, volume: "C:", mtime: 0, ctime: 0, alloc: 1 << 20, role: "extra", why: null, view: false },
  ],
  total_members: 2,
  ...over,
});

export const scan = (id: string, over: Partial<ScanStatus> = {}): ScanStatus => ({ scan_id: id, state: "done", source: "scan", method: "hard", roots: [{ node: 1, path: `${id}:\\`, volume: null, whole_volume: true }], ...over }) as ScanStatus;


"""Duplicate-file cleanup plans (capability ``dupes.cleanup.v1``, needs ``dupes.v1``; docs/dupes.md §16).

A plan belongs to one finished search. Nothing in it changes a file until the plan is approved
(``dupes.cleanup_approve``: a frozen list of steps and a token), the high risks are acknowledged,
and ``dupes.cleanup_execute`` is called with that token; every step checks its file again first.
Originals of replaced copies stay in a recovery area until ``dupes.cleanup_purge``, the only step
that cannot be undone. There is no one-click deletion: no method turns search results into
deletions without a plan.
"""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload

CleanupAction = Literal["leave", "trash", "hardlink", "symlink", "clone"]
CleanupSeverity = Literal["none", "info", "low", "medium", "high", "blocked"]
CleanupKeep = Literal["keep", "locked", "none"]
CleanupGroupState = Literal["in", "stale", "shared", "excluded", "no_extra", "cleaned"]
CleanupPreset = Literal["watch", "conservative", "recommended", "links", "backup", "custom"]
CleanupKeepEffect = Literal["keep_all", "prefer", "avoid", "per_folder", "keep_n", "order"]
CleanupCriterion = Literal[
    "oldest_created",
    "oldest_modified",
    "newest_modified",
    "shortest_path",
    "longest_path",
    "shallowest",
    "deepest",
    "plain_name",
]
CleanupSort = Literal["freed", "risk", "size", "copies", "path", "action"]
CleanupTaskKind = Literal["facts", "preflight", "execute", "undo", "purge", "recover"]
CleanupRunState = Literal[
    "waiting", "preflight", "running", "paused", "done", "partial", "stopped", "failed", "undone", "recovered"
]
CleanupEditOp = Literal[
    "keep_add",
    "keep_remove",
    "keep_only",
    "keep_set",
    "keep_clear",
    "action_set",
    "target_set",
    "exclude",
    "include",
    "override_clear",
    "rule_add",
    "rule_update",
    "rule_remove",
    "rule_move",
    "pair_set",
    "pair_remove",
    "preset",
    "options",
    "review",
    "ack",
    "unack",
]

_ID = Field(min_length=1, max_length=64)


class CleanupSelector(Params):
    under: list[str] = Field(default_factory=list, max_length=64)
    not_under: list[str] = Field(default_factory=list, max_length=64)
    volumes: list[str] = Field(default_factory=list, max_length=64)
    fs: list[str] = Field(default_factory=list, max_length=16)
    ext: list[str] = Field(default_factory=list, max_length=256)
    classes: list[str] = Field(default_factory=list, max_length=64)
    name_glob: str = ""
    path_glob: str = ""
    size_min: int | None = Field(default=None, ge=0)
    size_max: int | None = Field(default=None, ge=0)
    copies_min: int | None = Field(default=None, ge=2)
    cross: bool | None = None
    same_folder: bool | None = None
    mtime_before: int | None = None
    mtime_after: int | None = None
    tags: list[str] = Field(default_factory=list, max_length=64)
    not_tags: list[str] = Field(default_factory=list, max_length=64)
    can: list[CleanupAction] = Field(default_factory=list)
    same_volume: bool | None = None
    risk_at_least: CleanupSeverity | None = Field(default=None, description="Risk-condition rules only")
    risk_any: list[str] = Field(default_factory=list, max_length=128)


class CleanupRule(Params):
    id: int = Field(default=0, ge=0, description="0 when adding")
    kind: Literal["keep", "action"]
    effect: str = Field(description="keep: a CleanupKeepEffect; action: a CleanupAction")
    when: CleanupSelector = Field(default_factory=CleanupSelector)
    enabled: bool = True
    label: str = Field(default="", max_length=200)
    label_key: str = Field(default="", max_length=64)
    folders: list[str] = Field(default_factory=list, max_length=64)
    n: int = Field(default=1, ge=1, le=64)
    criteria: list[CleanupCriterion] = Field(default_factory=list)
    fallback: list[CleanupAction] = Field(default_factory=list)
    target_under: str = ""
    symlink_style: Literal["", "relative", "absolute"] = ""


class CleanupPairRule(Params):
    a: str
    b: str
    keep: Literal["a", "b", "both"] = "a"
    other_action: Literal["", "leave", "trash", "hardlink", "symlink", "clone"] = ""
    auto: bool = False


class CleanupOptions(Params):
    on_failure: Literal["continue", "stop"] | None = None
    verify: Literal["auto", "content", "metadata"] | None = None
    symlink_style: Literal["auto", "relative", "absolute"] | None = None
    criteria: list[CleanupCriterion] | None = None
    per_volume: bool | None = None
    folder_consistent: bool | None = None
    default_action: CleanupAction | None = None


class CleanupCopyRef(Params):
    group: int = Field(ge=0)
    path: str


class CleanupScope(Params):
    groups: list[int] | None = Field(default=None, max_length=200000)
    copies: list[CleanupCopyRef] | None = Field(default=None, max_length=200000)
    filter: str | None = Field(
        default=None, max_length=2000, description="The filter syntax of docs/dupes.md §16.7.3"
    )


class CleanupEdit(Params):
    op: CleanupEditOp
    scope: CleanupScope = Field(default_factory=CleanupScope)
    action: CleanupAction | None = None
    target: str = ""
    paths: list[str] = Field(default_factory=list, max_length=4096)
    rule: CleanupRule | None = None
    rule_id: int = 0
    index: int = 0
    pair: CleanupPairRule | None = None
    preset: CleanupPreset | None = None
    backup_volume: str = ""
    options: CleanupOptions | None = None
    value: bool = True
    risk: str = ""
    include_overridden: bool = Field(
        default=False, description="Bulk edits also change copies chosen by hand"
    )


# ------------------------------------------------------------------ results
class CleanupRisk(Payload):
    id: str
    code: str
    severity: CleanupSeverity
    title: str
    text: str
    acked: bool = False


class CleanupCopy(Payload):
    row: int
    scan_id: str
    node: int
    path: str
    volume: str
    fs: str
    keep: CleanupKeep
    action: CleanupAction | None = Field(default=None, description="None for a keeper")
    target: str = Field(default="", description="The keeper a link or clone points at")
    symlink_text: str = ""
    source: str = Field(
        default="", description="Why it is kept, or why it gets its action (the user's words)"
    )
    source_code: str = ""
    manual: bool = False
    available: dict[str, str] = Field(
        default_factory=dict, description="Per action: '' when it can be done, else why not"
    )
    risks: list[CleanupRisk] = Field(default_factory=list)
    severity: CleanupSeverity = "none"
    freed_now: int = 0
    freed_later: int = 0
    alloc: int = 0
    mtime: int = 0
    btime: int = 0
    view_of: str = Field(
        default="", description="Another path of the same single-named file: it follows that one"
    )
    tags: list[str] = Field(default_factory=list)


class CleanupGroup(Payload):
    id: int
    digest16: str
    name: str
    names: int
    size: int
    copies: int
    keepers: int
    actions: dict[str, int]
    freed_now: int
    freed_later: int
    severity: CleanupSeverity
    risk_count: int
    source: str = Field(description="rule / manual / pair / safety / mixed")
    reviewed: bool = False
    changed_since_review: bool = False
    state: CleanupGroupState
    keep_line: str
    cross: bool
    volumes: list[str]
    conflict: bool = False
    only_avoided: bool = False
    safety_net: bool = False
    members: list[CleanupCopy] = Field(default_factory=list)
    total_members: int = 0


class CleanupGroupsPage(Payload):
    version: int
    total: int
    offset: int
    items: list[CleanupGroup]


class CleanupReason(Payload):
    layer: str
    text: str
    decided: bool = False


class CleanupFactRow(Payload):
    label: str
    this: str = Field(description="The copy's value")
    keeper: str
    differs: bool = False


class CleanupCopyDetail(Payload):
    item: CleanupCopy
    keeper: str = ""
    keep_chain: list[CleanupReason] = Field(default_factory=list)
    action_chain: list[CleanupReason] = Field(default_factory=list)
    facts: list[CleanupFactRow] = Field(default_factory=list)


class CleanupGroupDetail(Payload):
    version: int
    group: CleanupGroup
    copies: list[CleanupCopyDetail]
    out_reason: str = ""


class CleanupActionTotal(Payload):
    action: CleanupAction
    copies: int
    groups: int
    freed_now: int
    freed_later: int


class CleanupVolumeTotal(Payload):
    volume: str
    fs: str
    freed_now: int
    freed_later: int
    copies: int
    recovery: str = Field(default="", description="Where the originals of replaced copies stay")
    trash: str = Field(default="", description="The volume's recycle bin, or why it has none")
    snapshots_note: str = ""


class CleanupBucket(Payload):
    id: str
    code: str
    severity: CleanupSeverity
    title: str
    copies: int
    groups: int
    bytes: int
    acked: bool = False
    unacked: int = Field(default=0, description="High kinds: copies not covered by the acknowledgement")
    example: str = ""


class CleanupNotDone(Payload):
    reason: str
    text: str
    count: int
    unit: Literal["groups", "copies"] = "copies"


class CleanupSummary(Payload):
    version: int
    groups: int
    copies: int
    freed_now: int
    freed_later: int
    by_action: list[CleanupActionTotal]
    by_volume: list[CleanupVolumeTotal]
    by_severity: dict[str, int]
    buckets: list[CleanupBucket]
    not_done: list[CleanupNotDone]
    gates: list[str]
    reviewed: int
    in_plan: int
    compare_bytes: int = Field(default=0, description="Bytes read before running (verify, block clones)")


class CleanupApprovalInfo(Payload):
    token: str
    version: int
    steps: int
    by_action: dict[str, int]
    acks: dict[str, int]
    at: str


class CleanupRuleView(Payload):
    id: int
    kind: Literal["keep", "action"]
    effect: str
    enabled: bool
    label: str
    when: dict[str, object]
    folders: list[str]
    n: int
    criteria: list[str]
    fallback: list[str]
    target_under: str
    risky: bool
    hits: int = Field(default=0, description="Copies it decides")
    bytes: int = 0
    preset: bool = False


class CleanupPairView(Payload):
    a: str
    b: str
    keep: Literal["a", "b", "both"]
    other_action: str
    auto: bool


class CleanupResumeOffer(Payload):
    saved_at: str
    manual: int
    rules: int


class CleanupState(Payload):
    plan_id: str
    job_id: str
    scan_ids: list[str]
    version: int
    preset: str
    criteria: list[str]
    per_volume: bool
    folder_consistent: bool
    default_action: str
    options: dict[str, str]
    keep_rules: list[CleanupRuleView]
    action_rules: list[CleanupRuleView]
    pairs: list[CleanupPairView]
    auto_pairs: int = 0
    manual: int = 0
    summary: CleanupSummary
    facts_done: int = 0
    facts_total: int = 0
    approval: CleanupApprovalInfo | None = None
    can_undo: bool = False
    can_redo: bool = False
    resume_offer: CleanupResumeOffer | None = None
    platform: str = ""
    symlink_right: bool = True
    macos_links: bool = False
    running: str = Field(default="", description="The task id of a cleanup task in progress")


class CleanupDiffRow(Payload):
    path: str
    before: str
    after: str


class CleanupDiff(Payload):
    version: int
    before: dict[str, int]
    after: dict[str, int]
    freed_before: int
    freed_after: int
    unavailable: dict[str, int] = Field(
        default_factory=dict, description="Copies left as they were, by reason"
    )
    risks_added: dict[str, int] = Field(default_factory=dict)
    risks_removed: dict[str, int] = Field(default_factory=dict)
    protected: int = Field(default=0, description="Copies chosen by hand that the edit leaves alone")
    changed: int = 0
    examples: list[CleanupDiffRow] = Field(default_factory=list)
    refused: str = ""


class CleanupOpenParams(Params):
    job_id: str = _ID
    preset: CleanupPreset | None = None
    backup_volume: str = ""


class CleanupPlanParams(Params):
    plan_id: str = _ID


class CleanupResumeParams(Params):
    plan_id: str = _ID
    accept: bool


class CleanupEditParams(Params):
    plan_id: str = _ID
    base_version: int = Field(ge=0)
    edit: CleanupEdit


class CleanupVersionParams(Params):
    plan_id: str = _ID
    base_version: int = Field(ge=0)


class CleanupGroupsParams(Params):
    plan_id: str = _ID
    filter: str = Field(default="", max_length=2000)
    sort: CleanupSort = "freed"
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=100, ge=1, le=500)
    include_out: bool = Field(default=False, description="Also list the groups that are not in the plan")


class CleanupGroupParams(Params):
    plan_id: str = _ID
    group: int = Field(ge=0)


class CleanupFacets(Payload):
    version: int
    total: int
    action: dict[str, int]
    severity: dict[str, int]
    risk: dict[str, int]
    volume: dict[str, int]
    fs: dict[str, int]
    source: dict[str, int]
    state: dict[str, int]
    tag: dict[str, int]


class CleanupFilterParams(Params):
    plan_id: str = _ID
    filter: str = Field(default="", max_length=2000)


class CleanupPairsParams(Params):
    plan_id: str = _ID
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=100, ge=1, le=500)
    merge: bool = True


class CleanupPairRow(Payload):
    a: str
    b: str
    groups: int
    bytes: int
    a_in_b: float = Field(description="Share of a's compared files that have a copy in b")
    b_in_a: float
    keep: str = Field(description="a / b / both / mixed / none: where the keepers of these groups are")
    rule: CleanupPairView | None = None
    merged: int = Field(default=0, description="Folder pairs below it taken into this one")


class CleanupPairsPage(Payload):
    version: int
    total: int
    items: list[CleanupPairRow]


class CleanupVolumeCell(Payload):
    a: str
    b: str
    groups: int
    bytes: int


class CleanupVolumeMatrix(Payload):
    version: int
    volumes: list[str]
    cells: list[CleanupVolumeCell]


class CleanupRiskCopiesParams(Params):
    plan_id: str = _ID
    risk: str
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=50, ge=1, le=500)


class CleanupRiskCopies(Payload):
    version: int
    risk: CleanupBucket
    total: int
    items: list[CleanupCopy]


class CleanupSampleParams(Params):
    plan_id: str = _ID
    per_bucket: int = Field(default=10, ge=1, le=100)


class CleanupSample(Payload):
    groups: list[int]


class CleanupWhoUses(Payload):
    users: dict[str, list[str]]


class CleanupExportParams(Params):
    plan_id: str = ""
    run_id: str = ""
    format: Literal["json", "csv"] = "json"
    path: str = Field(min_length=1, max_length=4096, description="Where the user chose to save it")


class CleanupExportResult(Payload):
    path: str
    bytes: int


class CleanupImportParams(Params):
    plan_id: str = _ID
    path: str = Field(min_length=1, max_length=4096)


class CleanupImportResult(Payload):
    applied: int
    unmatched: list[str]
    state: CleanupState


class CleanupApproveParams(Params):
    plan_id: str = _ID
    version: int = Field(ge=0)


class CleanupTokenParams(Params):
    plan_id: str = _ID
    token: str = Field(min_length=8, max_length=64)


class CleanupExecuteParams(Params):
    plan_id: str = _ID
    token: str = Field(min_length=8, max_length=64)
    on_failure: Literal["continue", "stop"] | None = None
    verify: Literal["auto", "content", "metadata"] | None = None
    proceed_after_drops: bool = Field(
        default=False, description="Run the rest after the preflight skipped some steps"
    )


class CleanupTask(Payload):
    task_id: str
    kind: CleanupTaskKind
    run_id: str = ""
    total: int = 0
    delay_s: float = 0.0


class CleanupTaskParams(Params):
    task_id: str = _ID


class CleanupSkipped(Payload):
    code: str
    text: str
    count: int
    examples: list[str] = Field(default_factory=list)


class CleanupProgressEvent(Payload):
    task_id: str
    kind: CleanupTaskKind
    phase: str
    state: CleanupRunState
    done: int
    total: int
    bytes: int = 0
    current: str = ""
    counts: dict[str, int] = Field(default_factory=dict)
    freed_now: int = 0
    freed_later: int = 0
    run_id: str = ""
    plan_id: str = ""
    message: str = ""
    skipped: list[CleanupSkipped] = Field(
        default_factory=list, description="Final event: what was not done, by reason"
    )
    needs_confirm: bool = Field(
        default=False, description="Preflight skipped steps: run the rest only when asked again"
    )


class CleanupRunSummary(Payload):
    run_id: str
    started: str
    finished: str = ""
    state: CleanupRunState
    roots: list[str]
    by_action: dict[str, int]
    counts: dict[str, int]
    freed_now: int = 0
    held: int = Field(default=0, description="Bytes of originals still in the recovery area")
    held_by_volume: dict[str, int] = Field(default_factory=dict)
    in_trash: int = 0
    can_undo: int = 0
    can_purge: int = 0
    attention: int = 0
    purge_reminder: bool = False


class CleanupRunsParams(Params):
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=50, ge=1, le=200)


class CleanupRunsPage(Payload):
    total: int
    items: list[CleanupRunSummary]


class CleanupRunParams(Params):
    run_id: str = _ID
    filter: str = Field(default="", max_length=64, description="A result code, or '' for every item")
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=200, ge=1, le=2000)


class CleanupRunItem(Payload):
    seq: int
    action: CleanupAction
    path: str
    target: str = ""
    code: str
    text: str
    state: Literal["pending", "done", "skipped", "failed", "attention", "undone", "purged", "cancelled"]
    recovery: str = ""
    freed: int = 0


class CleanupRunDetail(Payload):
    run: CleanupRunSummary
    total: int
    items: list[CleanupRunItem]


class CleanupRunSeqsParams(Params):
    run_id: str = _ID
    seqs: list[int] | None = Field(default=None, max_length=200000)
    beside: bool = Field(
        default=False, description="Undo: an original that cannot go back goes beside it instead"
    )


class CleanupPlanItem(Payload):
    seq: int
    path: str
    can: bool
    why: str = ""
    bytes: int = 0


class CleanupUndoPlan(Payload):
    run_id: str
    items: list[CleanupPlanItem]
    can: int
    cannot: int


class CleanupPurgeParams(Params):
    run_id: str = _ID
    seqs: list[int] | None = None
    verify: Literal["auto", "content"] = "auto"
    ack_streams: bool = Field(default=False, description="Also originals with extra data never compared")


class CleanupPurgePlan(Payload):
    run_id: str
    items: list[CleanupPlanItem]
    can: int
    cannot: int
    bytes: int
    streams: int = Field(
        default=0, description="Originals with extra streams or attributes (need ack_streams)"
    )
    in_trash: int = Field(default=0, description="Copies in the recycle bin: emptying it is up to the user")


class CleanupResolveParams(Params):
    run_id: str = _ID
    seq: int = Field(ge=1)
    choice: Literal["put_back", "keep_as_is"]


class CleanupInterrupted(Payload):
    run_id: str
    at_seq: int
    restored: int
    done: int
    attention: int
    message: str


class CleanupRecoverReport(Payload):
    report: CleanupInterrupted | None = None


class CleanupChangedEvent(Payload):
    plan_id: str
    version: int
    reason: Literal["edit", "facts", "results", "approved", "basket", "closed", "run"]

//! Snapshot comparison (M6 W6, docs/manage.md §3).
//!
//! `compare.open` merges two trees (a snapshot and the tab's live tree, or two snapshots) in the
//! background; `compare.progress` events follow and the last one carries [`CompareInfo`]. The
//! result behaves like a scan: `view.open {scan_id: compare_id}` gives a treemap whose areas are
//! the changes (|target − base|), with two extra frame columns, `delta` (signed net change, i8) and
//! `dstat` (u1: 0 unchanged, 1 new, 2 deleted, 3 changed).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::{DateTime, yes};
use crate::macros::str_enum;
use crate::scan::SnapshotInfo;
use crate::validate::{self as v, Validate, ValidationResult};

pub const NOW: &str = "now";
pub const DSTAT_SAME: u8 = 0;
pub const DSTAT_NEW: u8 = 1;
pub const DSTAT_DELETED: u8 = 2;
pub const DSTAT_CHANGED: u8 = 3;

str_enum! {
    pub enum CompareDirection {
        All = "all",
        Grow = "grow",
        Shrink = "shrink",
    }
}

impl Default for CompareDirection {
    fn default() -> Self {
        CompareDirection::All
    }
}

str_enum! {
    pub enum ChangeStatus {
        Same = "same",
        New = "new",
        Deleted = "deleted",
        Changed = "changed",
    }
}

str_enum! {
    pub enum ChangeKind {
        File = "file",
        Dir = "dir",
        Link = "link",
        Other = "other",
        Group = "group",
    }
}

str_enum! {
    pub enum ComparePhase {
        Loading = "loading",
        Comparing = "comparing",
        Ready = "ready",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

str_enum! {
    pub enum CompareSideKind {
        Snapshot = "snapshot",
        Now = "now",
    }
}

/// One end of a comparison: a snapshot file, or the tab's live tree ("now").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareSide {
    pub kind: CompareSideKind,
    /// The snapshot file (kind 'snapshot')
    #[serde(default)]
    pub path: Option<String>,
    /// When the snapshot was taken; for 'now' the moment compared
    pub created_at: DateTime,
    #[serde(default)]
    pub bytes_alloc: i64,
    #[serde(default)]
    pub node_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareSourcesParams {
    pub scan_id: String,
}

impl Validate for CompareSourcesParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareSourcesResult {
    /// Snapshots of the same scan roots, newest first
    pub snapshots: Vec<SnapshotInfo>,
    /// The live tree, when it can be compared
    #[serde(default)]
    pub now: Option<CompareSide>,
    /// Why the live tree cannot be compared yet
    #[serde(default)]
    pub now_note: String,
}

fn default_now() -> String {
    NOW.to_owned()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareOpenParams {
    /// The tab's scan (its roots, its live tree)
    pub scan_id: String,
    /// A snapshot file, or 'now'
    pub base: String,
    /// Same as base
    #[serde(default = "default_now")]
    pub target: String,
}

impl Validate for CompareOpenParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::str_len("base", &self.base, 1, 32767)?;
        v::str_len("target", &self.target, 1, 32767)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareTask {
    pub compare_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareIdParams {
    pub compare_id: String,
}

impl Validate for CompareIdParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("compare_id", &self.compare_id, 1, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareInfo {
    pub compare_id: String,
    pub scan_id: String,
    pub base: CompareSide,
    pub target: CompareSide,
    /// Allocated bytes added, every file
    pub grow: i64,
    /// Allocated bytes gone, every file (≥ 0)
    pub shrink: i64,
    /// Items that changed (files and folders)
    pub changed: i64,
    /// Rows of the change tree
    pub nodes: i64,
    /// The target is 'now' and follows file changes
    #[serde(default)]
    pub live: bool,
    /// Bumped each time live changes recompute it
    #[serde(default)]
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareProgressEvent {
    pub compare_id: String,
    pub phase: ComparePhase,
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub message: String,
    /// Present once ready
    #[serde(default)]
    pub info: Option<CompareInfo>,
}

/// A changed item: a file, a folder, or an aggregate of small items ('group').
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeItem {
    /// Node of the change tree (kind 'group': the folder holding them)
    pub node: u32,
    pub path: String,
    /// A short, recognisable name: 「WeChat Files\…\Video」
    pub label: String,
    pub kind: ChangeKind,
    /// Net change of allocated bytes (target − base)
    pub delta: i64,
    /// Allocated bytes in the base (0 when new)
    pub base: i64,
    /// Allocated bytes in the target (0 when deleted)
    pub target: i64,
    pub status: ChangeStatus,
    /// Files below it that are new
    #[serde(default)]
    pub new_files: i64,
    /// Kind 'group': changed small items in it
    #[serde(default)]
    pub members: i64,
    /// Known cause (insights rules), e.g. 「游戏更新」
    #[serde(default)]
    pub reason: String,
    /// Still on disk (file operations allowed)
    #[serde(default = "yes")]
    pub on_disk: bool,
}

fn default_summary_limit() -> u32 {
    12
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareSummaryParams {
    pub view_id: String,
    #[serde(default = "default_summary_limit")]
    pub limit: u32,
}

impl Validate for CompareSummaryParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("view_id", &self.view_id, 1, 64)?;
        v::int_range("limit", self.limit, 1, 500)
    }
}

/// The sidebar of a compare view, for its zoom root and filter (UI-W6-24…31).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareSummary {
    pub view_id: String,
    pub compare_id: String,
    /// Path of the zoom root the numbers cover
    pub scope: String,
    /// The scope is the whole comparison
    pub whole: bool,
    pub base: CompareSide,
    pub target: CompareSide,
    /// target time − base time (negative after a swap)
    pub span_s: f64,
    pub grow: i64,
    pub shrink: i64,
    pub net: i64,
    /// Free bytes of the volume now (live target)
    #[serde(default)]
    pub free: Option<i64>,
    /// Net bytes per day, when it grows
    #[serde(default)]
    pub daily: Option<f64>,
    /// free ÷ daily (an estimate)
    #[serde(default)]
    pub days_to_full: Option<f64>,
    pub grow_items: Vec<ChangeItem>,
    pub shrink_items: Vec<ChangeItem>,
    /// Change units that grew (the list shows the first ones)
    pub grow_count: i64,
    pub shrink_count: i64,
}

fn default_node_children() -> u32 {
    4
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareNodeParams {
    pub view_id: String,
    /// A block of the view's last frame
    pub key: i64,
    #[serde(default = "default_node_children")]
    pub children: u32,
}

impl Validate for CompareNodeParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("view_id", &self.view_id, 1, 64)?;
        v::int_range("children", self.children, 0, 50)
    }
}

/// The hover card of a compare view (UI-W6-20…22).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareNodeResult {
    pub item: ChangeItem,
    /// Its children that changed most
    #[serde(default)]
    pub children: Vec<ChangeItem>,
}

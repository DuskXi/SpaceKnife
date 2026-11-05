//! Scanning, tree queries, volumes and snapshots.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::{DateTime, yes};
use crate::filter::TagColor;
use crate::io::{IoStatus, LoadLevel};
use crate::macros::str_enum;
use crate::validate::{self as v, Validate, ValidationResult};

pub const TOPIC_SCAN: &str = "scan";

str_enum! {
    pub enum ScanStateName {
        Queued = "queued",
        Scanning = "scanning",
        Done = "done",
        Cancelled = "cancelled",
        Failed = "failed",
    }
}

str_enum! {
    pub enum NodeKindName {
        File = "file",
        Dir = "dir",
        Link = "link",
        Other = "other",
    }
}

str_enum! {
    pub enum ScanMethod {
        Auto = "auto",
        Mft = "mft",
        Hard = "hard",
    }
}

str_enum! {
    pub enum ScanMethodUsed {
        Hard = "hard",
        Mft = "mft",
        Snapshot = "snapshot",
    }
}

impl Default for ScanMethodUsed {
    fn default() -> Self {
        ScanMethodUsed::Hard
    }
}

str_enum! {
    /// How a saved tree was read.
    pub enum TreeMethod {
        Hard = "hard",
        Mft = "mft",
    }
}

str_enum! {
    pub enum MediaName {
        NvmeSsd = "nvme_ssd",
        SataSsd = "sata_ssd",
        Ssd = "ssd",
        Hdd = "hdd",
        UsbHdd = "usb_hdd",
        UsbSsd = "usb_ssd",
        UsbFlash = "usb_flash",
        SdCard = "sd_card",
        Network = "network",
        Virtual = "virtual",
        Unknown = "unknown",
    }
}

impl Default for MediaName {
    fn default() -> Self {
        MediaName::Unknown
    }
}

// --------------------------------------------------------------------------- volumes

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeInfo {
    pub mount_point: String,
    pub device: String,
    pub fs_type: String,
    pub label: String,
    pub total: i64,
    pub free: i64,
    pub used: i64,
    pub cluster_size: i64,
    pub is_remote: bool,
    pub is_removable: bool,
    #[serde(default)]
    pub serial: String,
    /// Storage media (start page cards, IO tuning in M4)
    #[serde(default)]
    pub media: MediaName,
    /// The system volume (selected by default on the start page)
    #[serde(default)]
    pub is_system: bool,
    /// Network volumes: the server path (\\server\share, host:/export)
    #[serde(default)]
    pub remote_path: String,
    /// Why the volume cannot be scanned now ('' = available)
    #[serde(default)]
    pub reason: String,
    /// Scannable but not recommended, and why (e.g. a Windows drive seen from WSL over 9P)
    #[serde(default)]
    pub note: String,
    /// Free only for the super-user (ext4's 5 %): counted neither as used nor free
    #[serde(default)]
    pub reserved: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeListResult {
    pub volumes: Vec<VolumeInfo>,
    /// An MFT scan needs no administrator prompt now (the backend has administrator rights, or the read-only helper is running)
    #[serde(default)]
    pub elevated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathCheckParams {
    /// Paths as typed; blank entries are ignored
    pub paths: Vec<String>,
}

impl Validate for PathCheckParams {
    fn validate(&self) -> ValidationResult {
        v::list_len("paths", &self.paths, 0, 16)
    }
}

str_enum! {
    pub enum PathCheckState {
        Ok = "ok",
        Missing = "missing",
        NotDir = "not_dir",
        Denied = "denied",
        Error = "error",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PathCheck {
    /// As typed
    pub path: String,
    /// Absolute, normalised
    pub normalized: String,
    pub state: PathCheckState,
    /// What is wrong, ready to show
    #[serde(default)]
    pub message: String,
    /// Mount point of the volume holding the path
    #[serde(default)]
    pub volume: String,
    /// Inside (or equal to) another path of the list
    #[serde(default)]
    pub merged_into: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PathCheckResult {
    pub items: Vec<PathCheck>,
    /// The valid paths to scan, nested ones merged into their ancestors
    pub roots: Vec<String>,
}

// --------------------------------------------------------------------------- scans

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanStartParams {
    /// Absolute paths; drive roots allowed
    pub roots: Vec<String>,
    /// Descend into other filesystems mounted below a root; None = the setting
    #[serde(default)]
    pub cross_mounts: Option<bool>,
    /// 0 = scan in-process (no pool); None = the concurrency setting
    #[serde(default)]
    pub workers: Option<u32>,
    /// Save a snapshot automatically when the scan completes; None = the setting
    #[serde(default)]
    pub auto_snapshot: Option<bool>,
    /// auto and mft: the MFT fast scan for a whole local NTFS volume (the read-only helper asks for administrator rights once, unless the scan.elevate setting is off), else a hard scan; None = the setting
    #[serde(default)]
    pub method: Option<ScanMethod>,
    /// Disk load limit; None = the setting
    #[serde(default)]
    pub load_level: Option<LoadLevel>,
    /// Percent for load_level=custom
    #[serde(default)]
    pub load_custom: Option<u32>,
    /// Count NTFS alternate data streams (MFT scans); None = the setting
    #[serde(default)]
    pub ads: Option<bool>,
    /// macOS without Full Disk Access: leave out the places that would each ask for consent (Desktop, Documents, Downloads, cloud drives, Photos library, other apps' data), shown as not accessible; None = the scan.protected_places setting
    #[serde(default)]
    pub skip_protected: Option<bool>,
}

impl Validate for ScanStartParams {
    fn validate(&self) -> ValidationResult {
        v::list_len("roots", &self.roots, 1, 16)?;
        v::opt_int_range("workers", self.workers, 0, 64)?;
        v::opt_int_range("load_custom", self.load_custom, 5, 100)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanSetLoadParams {
    pub scan_id: String,
    pub level: LoadLevel,
    /// Percent for level=custom
    #[serde(default)]
    pub custom: Option<u32>,
    /// Fixed parallelism; None = automatic (the governor's)
    #[serde(default)]
    pub concurrency: Option<u32>,
}

impl Validate for ScanSetLoadParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::opt_int_range("custom", self.custom, 5, 100)?;
        v::opt_int_range("concurrency", self.concurrency, 1, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScanRootInfo {
    pub path: String,
    pub node: u32,
    pub whole_volume: bool,
    #[serde(default)]
    pub volume: Option<VolumeInfo>,
}

str_enum! {
    pub enum MonitorState {
        Starting = "starting",
        Running = "running",
        Degraded = "degraded",
        Failed = "failed",
    }
}

/// Live change monitoring of a scan (M5): the status bar's 「实时监控」 item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MonitorStatus {
    pub state: MonitorState,
    /// Why it is degraded or failed (Chinese, shown as is)
    #[serde(default)]
    pub detail: String,
    /// rdcw | inotify | fsevents | poll (one per kind in use)
    #[serde(default)]
    pub watcher: String,
    /// Folders under live watch; -1: the whole tree
    #[serde(default)]
    pub watched_dirs: i64,
    #[serde(default)]
    pub polled_dirs: i64,
    #[serde(default)]
    pub events: i64,
    /// Changes applied to the tree so far
    #[serde(default)]
    pub changes: i64,
    #[serde(default)]
    pub overflows: i64,
    /// Changes and folders waiting to be applied
    #[serde(default)]
    pub pending: i64,
    #[serde(default)]
    pub last_apply_ms: f64,
}

str_enum! {
    pub enum ScanSource {
        Scan = "scan",
        Snapshot = "snapshot",
    }
}

impl Default for ScanSource {
    fn default() -> Self {
        ScanSource::Scan
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScanStatus {
    pub scan_id: String,
    pub state: ScanStateName,
    pub roots: Vec<ScanRootInfo>,
    pub items: i64,
    pub files: i64,
    pub dirs: i64,
    pub bytes_alloc: i64,
    pub bytes_size: i64,
    pub errors: i64,
    pub pending_dirs: i64,
    pub current_path: String,
    pub elapsed_s: f64,
    /// Items per second, averaged over the last few seconds
    pub rate: f64,
    /// Allocated bytes per second, same window
    #[serde(default)]
    pub byte_rate: f64,
    /// 0..1 against the volume's used space, or against the item count of the last scan of the same roots; None if unknown
    pub progress: Option<f64>,
    /// Estimated seconds left; None while estimating
    #[serde(default)]
    pub remaining_s: Option<f64>,
    /// Items of the last scan of the same roots
    #[serde(default)]
    pub expected_items: Option<i64>,
    #[serde(default)]
    pub started_at: Option<DateTime>,
    #[serde(default)]
    pub finished_at: Option<DateTime>,
    /// Directories that could not be listed (capped)
    #[serde(default)]
    pub denied: Vec<String>,
    #[serde(default)]
    pub message: String,
    /// Free bytes (whole-volume scans only)
    #[serde(default)]
    pub free: Option<i64>,
    /// Used but not scanned bytes (whole-volume scans only)
    #[serde(default)]
    pub unknown: Option<i64>,
    /// Path of the snapshot this scan was saved to/loaded from
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub source: ScanSource,
    /// How the tree was (or is being) read
    #[serde(default)]
    pub method: ScanMethodUsed,
    /// What a fast scan is doing: helper (starting the read-only helper, may wait for the administrator prompt), mft (reading and parsing the $MFT), receive, build; empty otherwise
    #[serde(default)]
    pub phase: String,
    /// Why an MFT scan fell back to a hard scan (shown once)
    #[serde(default)]
    pub fallback: String,
    /// The target is a whole local NTFS volume: scan.use_mft can switch to MFT
    #[serde(default)]
    pub mft_available: bool,
    #[serde(default)]
    pub paused: bool,
    /// Load state of the disk being scanned (W2)
    #[serde(default)]
    pub io: Option<IoStatus>,
    /// Live change monitoring; None: not monitored (off, or a snapshot)
    #[serde(default)]
    pub monitor: Option<MonitorStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanIdParams {
    pub scan_id: String,
}

impl Validate for ScanIdParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanRescanParams {
    pub scan_id: String,
    /// Folder to read again, with everything below it
    pub node: u32,
}

impl Validate for ScanRescanParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::int_range("node", self.node, 1, u32::MAX)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScanListResult {
    pub scans: Vec<ScanStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanTopicParams {
    pub scan_id: String,
}

impl Validate for ScanTopicParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)
    }
}

/// ``scan.progress``: periodic status of a scan (≤ 4 Hz), plus a final one when it ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScanProgressEvent {
    pub sub: String,
    pub status: ScanStatus,
}

// --------------------------------------------------------------------------- tree

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NodeInfo {
    pub id: u32,
    pub parent: i64,
    pub name: String,
    pub kind: NodeKindName,
    pub depth: i64,
    /// Logical bytes (subtree total for directories)
    pub size: i64,
    /// Allocated bytes on disk (subtree total for directories)
    pub alloc: i64,
    #[serde(default)]
    pub files: i64,
    #[serde(default)]
    pub dirs: i64,
    #[serde(default)]
    pub child_count: i64,
    #[serde(default)]
    pub mtime: i64,
    #[serde(default)]
    pub ctime: i64,
    #[serde(default)]
    pub atime: i64,
    #[serde(default)]
    pub ext: String,
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TreeNodeParams {
    pub scan_id: String,
    pub node: u32,
}

impl Validate for TreeNodeParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeNodeResult {
    pub node: NodeInfo,
    pub path: String,
    /// False inside the MFT pseudo folders ($NTFS 元数据, $孤立文件): no such path exists
    #[serde(default = "yes")]
    pub on_disk: bool,
}

str_enum! {
    pub enum ChildSort {
        Size = "size",
        Name = "name",
    }
}

impl Default for ChildSort {
    fn default() -> Self {
        ChildSort::Size
    }
}

fn default_children_limit() -> u32 {
    200
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TreeChildrenParams {
    pub scan_id: String,
    pub node: u32,
    #[serde(default)]
    pub sort: ChildSort,
    #[serde(default)]
    pub offset: u32,
    #[serde(default = "default_children_limit")]
    pub limit: u32,
}

impl Validate for TreeChildrenParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::int_range("limit", self.limit, 1, 5000)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeChildrenResult {
    pub parent: NodeInfo,
    pub total: i64,
    pub items: Vec<NodeInfo>,
}

fn default_top() -> u32 {
    5
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TreeSummaryParams {
    pub scan_id: String,
    pub node: u32,
    /// How many of the largest files to return
    #[serde(default = "default_top")]
    pub top: u32,
}

impl Validate for TreeSummaryParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::int_range("top", self.top, 0, 100)
    }
}

fn default_class_other() -> String {
    crate::classes::OTHER.to_owned()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TopFile {
    pub node: NodeInfo,
    /// Folder of the file relative to the summarised node, ending in a separator
    pub rel_parent: String,
    #[serde(default)]
    pub tag: Option<TagColor>,
    /// The file's class (colour): by extension, and by size for ts / mts
    #[serde(default = "default_class_other")]
    pub class_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClassPart {
    /// The class the files would have by their extension
    pub class_id: String,
    pub bytes: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClassStat {
    pub class_id: String,
    pub bytes: i64,
    pub files: i64,
    /// Files a folder class took over, by the class of their extension, largest first (「游戏 · 其中视频 3 GB」); empty when none
    #[serde(default)]
    pub parts: Vec<ClassPart>,
}

/// Why a folder's files take a folder class (spaceknife.pathclass).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FolderClassInfo {
    pub class_id: String,
    /// What the folder was recognised as, in the interface language
    pub reason: String,
    /// The folder that decided (the node itself or an ancestor)
    pub folder: String,
    /// The user chose it (class_folders.json)
    pub user: bool,
    /// Only files whose extension is in no class take it
    pub fallback: bool,
}

str_enum! {
    /// The part of a node's summary a [`SummaryGap`] is about.
    pub enum SummaryPart {
        Contents = "contents",
        Classes = "classes",
        TopFiles = "top_files",
        FolderClass = "folder_class",
        Tags = "tags",
        Volume = "volume",
        Parent = "parent",
    }
}

/// A part of a node's summary the backend could not work out; the rest is still given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SummaryGap {
    /// contents: largest files, class totals and newest change (left empty); classes: the folder classes (classes counted by extension alone); top_files, folder_class, tags, volume, parent: that field (left empty)
    pub part: SummaryPart,
    /// Why, in the interface language
    pub reason: String,
}

/// Everything the details panel shows for one node (UI-MAIN-35…43, G11).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeSummaryResult {
    pub node: NodeInfo,
    pub path: String,
    #[serde(default)]
    pub parent: Option<NodeInfo>,
    /// Newest modification time in the subtree (0 = unknown)
    pub latest_mtime: i64,
    pub top_files: Vec<TopFile>,
    /// Allocated bytes and files per file class, largest first
    pub classes: Vec<ClassStat>,
    #[serde(default)]
    pub tag: Option<TagColor>,
    /// The volume of the scan root that holds the node
    #[serde(default)]
    pub volume: Option<VolumeInfo>,
    /// False inside the MFT pseudo folders ($NTFS 元数据, $孤立文件): no such path exists
    #[serde(default = "yes")]
    pub on_disk: bool,
    /// The folder class the node (or a file's folder) takes, if any
    #[serde(default)]
    pub folder_class: Option<FolderClassInfo>,
    /// Parts that could not be worked out this time; empty when complete
    #[serde(default)]
    pub gaps: Vec<SummaryGap>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TreeFindParams {
    pub scan_id: String,
    pub path: String,
}

impl Validate for TreeFindParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::str_len("path", &self.path, 1, 32767)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeFindResult {
    #[serde(default)]
    pub node: Option<NodeInfo>,
}

// --------------------------------------------------------------------------- snapshots

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotInfo {
    pub path: String,
    pub created_at: DateTime,
    pub roots: Vec<String>,
    pub node_count: i64,
    #[serde(default)]
    pub bytes_alloc: i64,
    #[serde(default)]
    pub files: i64,
    /// Size of the snapshot file itself
    #[serde(default)]
    pub file_bytes: i64,
    /// How the saved tree was read (None: saved before M4, a hard scan)
    #[serde(default)]
    pub method: Option<TreeMethod>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotListResult {
    pub snapshots: Vec<SnapshotInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotSaveParams {
    pub scan_id: String,
    /// Target file; default = the auto-save folder
    #[serde(default)]
    pub path: Option<String>,
}

impl Validate for SnapshotSaveParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotLoadParams {
    pub path: String,
}

impl Validate for SnapshotLoadParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("path", &self.path, 1, 32767)
    }
}

//! Command palette (W4): commands, recents, direct items and path search, all ranked by the backend.
//!
//! Frontends only render what comes back and run the chosen command by its `id`; scoring, hit
//! ranges, availability and status texts are computed here so that every frontend shows the same
//! list (UI-W4-49). Hit ranges are UTF-16 `[start, end)` offsets into the text they belong to.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::yes;
use crate::filter::{FilterDiagnostic, FilterToken};
use crate::macros::str_enum;
use crate::validate::{self as v, Validate, ValidationResult};

str_enum! {
    pub enum PaletteTab {
        Scan = "scan",
        Start = "start",
        Settings = "settings",
    }
}

str_enum! {
    pub enum RecentKind {
        Jump = "jump",
        Filter = "filter",
    }
}

str_enum! {
    pub enum PathState {
        InScan = "in_scan",
        Outside = "outside",
        Missing = "missing",
    }
}

str_enum! {
    pub enum DirectKind {
        Filter = "filter",
        Path = "path",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// Where the palette was opened: the active tab (or the focused pane of a split).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaletteContext {
    pub tab: PaletteTab,
    #[serde(default)]
    pub scan_id: Option<String>,
    #[serde(default)]
    pub view_id: Option<String>,
    /// Items in the delete basket (held by the frontend until M6)
    #[serde(default)]
    pub basket: u32,
}

impl Validate for PaletteContext {
    fn validate(&self) -> ValidationResult {
        v::opt_str_len("scan_id", &self.scan_id, 0, 64)?;
        v::opt_str_len("view_id", &self.view_id, 0, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaletteQueryParams {
    #[serde(default)]
    pub text: String,
    pub context: PaletteContext,
}

impl Validate for PaletteQueryParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("text", &self.text, 0, 4096)?;
        self.context.validate().map_err(|e| e.at("context"))
    }
}

/// One command, where the user is: a keyboard shortcut runs it only when the palette would.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaletteCheckParams {
    /// A command id (palette.commands)
    pub id: String,
    pub context: PaletteContext,
}

impl Validate for PaletteCheckParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("id", &self.id, 1, 64)?;
        self.context.validate().map_err(|e| e.at("context"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaletteCommand {
    /// Run by the frontend; stable across versions
    pub id: String,
    pub name: String,
    /// Matched characters of the name
    #[serde(default)]
    pub hits: Vec<Span>,
    /// State info, or the reason when disabled
    #[serde(default)]
    pub status: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Bindings in neutral notation ('Ctrl+Shift+F5'); the first is shown, Ctrl = ⌘ on macOS
    #[serde(default)]
    pub keys: Vec<String>,
    /// Icon name from the design icon set
    pub icon: String,
    /// Ends with '…': running it asks for one more choice
    #[serde(default)]
    pub more: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaletteRecent {
    pub kind: RecentKind,
    /// Absolute path (jump) or filter expression (filter)
    pub text: String,
    #[serde(default)]
    pub hits: Vec<Span>,
    /// Filter colouring (W3 rules)
    #[serde(default)]
    pub tokens: Vec<FilterToken>,
    /// jump: the path is still in this scan
    #[serde(default = "yes")]
    pub exists: bool,
    #[serde(default)]
    pub node: Option<u32>,
    #[serde(default = "yes")]
    pub is_dir: bool,
    /// jump: current size; filter: see palette.preview
    #[serde(default)]
    pub alloc: Option<i64>,
}

/// The input itself as a filter expression or a path (UI-W4-31…33).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaletteDirect {
    pub kind: DirectKind,
    pub text: String,
    // filter
    #[serde(default)]
    pub tokens: Vec<FilterToken>,
    #[serde(default)]
    pub diagnostics: Vec<FilterDiagnostic>,
    #[serde(default = "yes")]
    pub valid: bool,
    // path
    #[serde(default)]
    pub state: Option<PathState>,
    #[serde(default)]
    pub node: Option<u32>,
    #[serde(default = "yes")]
    pub is_dir: bool,
    #[serde(default)]
    pub alloc: Option<i64>,
    /// Tab: the path completed by one segment
    #[serde(default)]
    pub complete: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaletteResult {
    pub text: String,
    pub direct: Vec<PaletteDirect>,
    pub recent: Vec<PaletteRecent>,
    pub commands: Vec<PaletteCommand>,
    /// Ask palette.paths for the path group (scan tab only)
    pub paths: bool,
    /// Empty input: the selection's name searched for
    #[serde(default)]
    pub seed: Option<String>,
}

fn default_paths_limit() -> u32 {
    50
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PalettePathsParams {
    pub scan_id: String,
    #[serde(default)]
    pub text: String,
    /// Empty text: seed with its selection
    #[serde(default)]
    pub view_id: Option<String>,
    #[serde(default = "default_paths_limit")]
    pub limit: u32,
}

impl Validate for PalettePathsParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::str_len("text", &self.text, 0, 4096)?;
        v::opt_str_len("view_id", &self.view_id, 0, 64)?;
        v::int_range("limit", self.limit, 1, 500)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PalettePath {
    pub node: u32,
    pub name: String,
    /// Absolute path of the containing folder
    pub parent: String,
    pub is_dir: bool,
    pub alloc: i64,
    pub hits: Vec<Span>,
    /// 0 exact name · 1 prefix · 2 inside the name · 3 scattered letters · 4 child listing
    pub tier: u8,
    /// Tab: absolute path, folders with a trailing separator
    pub complete: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PalettePaths {
    pub text: String,
    pub items: Vec<PalettePath>,
    /// All hits, of which the best `limit` are listed
    pub total: i64,
    /// The scan is still running: results may be incomplete
    pub partial: bool,
    #[serde(default)]
    pub seed: Option<String>,
    /// Listing the children of this folder
    #[serde(default)]
    pub folder: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PalettePreviewParams {
    pub scan_id: String,
    pub text: String,
}

impl Validate for PalettePreviewParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::str_len("text", &self.text, 0, 8192)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PalettePreview {
    pub text: String,
    pub valid: bool,
    #[serde(default)]
    pub files: i64,
    #[serde(default)]
    pub alloc: i64,
    #[serde(default)]
    pub size: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaletteRememberParams {
    pub kind: RecentKind,
    pub text: String,
    /// jump: the scan the path belongs to
    #[serde(default)]
    pub scan_id: Option<String>,
}

impl Validate for PaletteRememberParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("text", &self.text, 1, 8192)?;
        v::opt_str_len("scan_id", &self.scan_id, 0, 64)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaletteClearParams {
    /// Clear this scan target's recents
    #[serde(default)]
    pub scan_id: Option<String>,
}

impl Validate for PaletteClearParams {
    fn validate(&self) -> ValidationResult {
        v::opt_str_len("scan_id", &self.scan_id, 0, 64)
    }
}

/// Every command with its default keys (W7 快捷键 page; the keymap becomes editable in M6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaletteCommandList {
    pub commands: Vec<PaletteCommand>,
}

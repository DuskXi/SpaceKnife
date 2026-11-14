//! Views (one per tab / split pane) and their frames.
//!
//! Every method that changes a view answers with a [`ViewUpdate`] (JSON) and, when the picture
//! changed, sends one binary frame (stream `"view:<view_id>"`, layout `FrameInfo.layout`) whose `seq`
//! equals `ViewUpdate.frame.seq` and whose `request_id` is the request's id. Frames pushed while a
//! scan is running carry no `request_id` and arrive with a `view.frame` event.
//!
//! The column layouts (`drawlist.v1`, `flame.v1`, `polar.v1`) are described in docs/protocol.md §4.1;
//! [`columns`] lists the column names and dtypes.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::compare::CompareDirection;
use crate::filter::{FilterDiagnostic, TagColor};
use crate::macros::{const_str, str_enum};
use crate::validate::{self as v, Validate, ValidationResult};

pub const DRAWLIST_LAYOUT: &str = "drawlist.v1";
pub const FLAME_LAYOUT: &str = "flame.v1";
pub const POLAR_LAYOUT: &str = "polar.v1";
/// `polar.v1` angle units per turn: every value stays exact in an f32.
pub const ANGLE_UNIT: i64 = 1 << 20;
pub const BUILTIN_VISUALS: [&str; 3] = ["treemap", "flame", "flame_polar"];
pub const VIEW_STREAM_PREFIX: &str = "view:";

/// The binary stream name of a view's frames.
pub fn view_stream(view_id: &str) -> String {
    let mut s = String::with_capacity(VIEW_STREAM_PREFIX.len() + view_id.len());
    s.push_str(VIEW_STREAM_PREFIX);
    s.push_str(view_id);
    s
}

/// Column names and dtypes of the frame layouts (docs/protocol.md §4.1–4.4).
pub mod columns {
    use crate::binary::Dtype;

    pub const KEY: (&str, Dtype) = ("key", Dtype::U4);
    pub const NODE: (&str, Dtype) = ("node", Dtype::I4);
    pub const PARENT: (&str, Dtype) = ("parent", Dtype::I4);
    pub const KIND: (&str, Dtype) = ("kind", Dtype::U1);
    pub const DEPTH: (&str, Dtype) = ("depth", Dtype::U1);
    pub const CLS: (&str, Dtype) = ("cls", Dtype::U1);
    pub const LABEL: (&str, Dtype) = ("label", Dtype::U1);
    pub const SIZE: (&str, Dtype) = ("size", Dtype::I8);
    pub const COUNT: (&str, Dtype) = ("count", Dtype::U4);
    pub const FLAGS: (&str, Dtype) = ("flags", Dtype::U2);
    pub const TAG: (&str, Dtype) = ("tag", Dtype::U1);
    pub const TONE: (&str, Dtype) = ("tone", Dtype::U1);
    pub const BRANCH: (&str, Dtype) = ("branch", Dtype::U1);
    pub const NAME_LEN: (&str, Dtype) = ("name_len", Dtype::U4);
    /// Compare views only.
    pub const DELTA: (&str, Dtype) = ("delta", Dtype::I8);
    /// Compare views only.
    pub const DSTAT: (&str, Dtype) = ("dstat", Dtype::U1);
    /// `drawlist.v1` / `flame.v1` geometry.
    pub const X: (&str, Dtype) = ("x", Dtype::I4);
    pub const Y: (&str, Dtype) = ("y", Dtype::I4);
    pub const W: (&str, Dtype) = ("w", Dtype::I4);
    pub const H: (&str, Dtype) = ("h", Dtype::I4);
    /// `polar.v1` geometry.
    pub const A0: (&str, Dtype) = ("a0", Dtype::I4);
    pub const A1: (&str, Dtype) = ("a1", Dtype::I4);
    pub const R0: (&str, Dtype) = ("r0", Dtype::I4);
    pub const R1: (&str, Dtype) = ("r1", Dtype::I4);
    /// Blob of the rows' UTF-8 names, concatenated in row order.
    pub const NAMES_BLOB: &str = "names";

    /// Row kinds (`kind` column).
    pub const KIND_FILE: u8 = 0;
    pub const KIND_DIR: u8 = 1;
    pub const KIND_COLLAPSED: u8 = 2;
    pub const KIND_PENDING: u8 = 3;
    pub const KIND_SMALL: u8 = 4;
    pub const KIND_MORE: u8 = 5;
    pub const KIND_FREE: u8 = 6;
    pub const KIND_UNKNOWN: u8 = 7;

    /// `label` bits.
    pub const LABEL_NAME: u8 = 1;
    pub const LABEL_SIZE: u8 = 2;
    pub const LABEL_LARGE: u8 = 4;
    pub const LABEL_SHORT: u8 = 8;
    pub const LABEL_RADIAL: u8 = 16;

    /// `flags` bits.
    pub const FLAG_MATCH: u16 = 1;
    pub const FLAG_DIMMED: u16 = 2;
    pub const FLAG_FOLDER_HIT: u16 = 4;
    pub const FLAG_TAGGED: u16 = 8;
    pub const FLAG_SELECTED: u16 = 16;
    pub const FLAG_HALO: u16 = 32;
    pub const FLAG_SCANNING: u16 = 64;
    pub const FLAG_IN_TRASH: u16 = 128;
    pub const FLAG_TRASH_ROOT: u16 = 256;

    /// `cls` of a block in no one class.
    pub const CLS_NONE: u8 = 255;
}

str_enum! {
    pub enum ColorModeName {
        Class = "class",
        Flat = "flat",
        Time = "time",
        Branch = "branch",
    }
}

str_enum! {
    pub enum FilterModeName {
        Dim = "dim",
        Hide = "hide",
    }
}

str_enum! {
    pub enum ResultsSortName {
        Size = "size",
        Name = "name",
        Count = "count",
    }
}

impl Default for ResultsSortName {
    fn default() -> Self {
        ResultsSortName::Size
    }
}

str_enum! {
    pub enum ResultKind {
        File = "file",
        Folder = "folder",
        Group = "group",
        Rest = "rest",
    }
}

fn check_view_id(id: &str) -> ValidationResult {
    v::str_len("view_id", id, 1, 64)
}

fn one() -> f64 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    /// CSS px
    pub width: f64,
    /// CSS px
    pub height: f64,
    #[serde(default = "one")]
    pub dpr: f64,
}

impl Validate for Viewport {
    fn validate(&self) -> ValidationResult {
        v::float_gt_le("width", self.width, 0.0, 32768.0)?;
        v::float_gt_le("height", self.height, 0.0, 32768.0)?;
        v::float_ge_le("dpr", self.dpr, 0.5, 8.0)
    }
}

/// Fields left out keep their value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewSettingsParams {
    #[serde(default)]
    pub detail: Option<u32>,
    /// CSS px
    #[serde(default)]
    pub min_element: Option<f64>,
    #[serde(default)]
    pub sort_by_size: Option<bool>,
    #[serde(default)]
    pub show_free: Option<bool>,
    #[serde(default)]
    pub show_unknown: Option<bool>,
    /// The recycle bins: drawn, or taken out of the picture
    #[serde(default)]
    pub show_trash: Option<bool>,
    /// The folders of the setting treemap.hidden_folders: taken out of the picture, or drawn
    #[serde(default)]
    pub hide_folders: Option<bool>,
    /// A visual.list id (needs visuals.v1)
    #[serde(default)]
    pub visual: Option<String>,
    /// Of the view's current visualization; 'branch' needs visuals.v1
    #[serde(default)]
    pub color_mode: Option<ColorModeName>,
    /// Colour mode per visualization (a split pane or a reopened view keeps them); color_mode, when also given, wins for the current one
    #[serde(default)]
    pub colors: Option<BTreeMap<String, ColorModeName>>,
    #[serde(default)]
    pub filter_mode: Option<FilterModeName>,
    #[serde(default)]
    pub viewport: Option<Viewport>,
    /// Compare views: changes shown
    #[serde(default)]
    pub direction: Option<CompareDirection>,
}

impl Validate for ViewSettingsParams {
    fn validate(&self) -> ValidationResult {
        v::opt_int_range("detail", self.detail, 1, 15)?;
        if let Some(m) = self.min_element {
            v::float_ge_le("min_element", m, 3.0, 20.0)?;
        }
        if let Some(vis) = &self.visual {
            v::str_len("visual", vis, 1, 48)?;
            v::pattern(
                "visual",
                v::is_visual_id(vis),
                r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$",
            )?;
        }
        self.viewport.validate().map_err(|e| e.at("viewport"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewOpenParams {
    pub scan_id: String,
    pub viewport: Viewport,
    #[serde(default)]
    pub settings: Option<ViewSettingsParams>,
}

impl Validate for ViewOpenParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        self.viewport.validate().map_err(|e| e.at("viewport"))?;
        self.settings.validate().map_err(|e| e.at("settings"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewIdParams {
    pub view_id: String,
}

impl Validate for ViewIdParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewZoomParams {
    pub view_id: String,
    /// A block's key from the last frame, or a node id
    pub key: u32,
}

impl Validate for ViewZoomParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewSelectParams {
    pub view_id: String,
    #[serde(default)]
    pub key: Option<u32>,
}

impl Validate for ViewSelectParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewSetParams {
    pub view_id: String,
    pub settings: ViewSettingsParams,
}

impl Validate for ViewSetParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)?;
        self.settings.validate().map_err(|e| e.at("settings"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewFilterParams {
    pub view_id: String,
    pub text: String,
    #[serde(default)]
    pub cursor: Option<u32>,
}

impl Validate for ViewFilterParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)?;
        v::str_len("text", &self.text, 0, 8192)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewTagParams {
    pub view_id: String,
    /// None clears the selection's tag and every tag below it (Ctrl+0)
    #[serde(default)]
    pub color: Option<TagColor>,
}

impl Validate for ViewTagParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewListParams {
    #[serde(default)]
    pub scan_id: Option<String>,
}

impl Validate for ViewListParams {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewRevealParams {
    pub view_id: String,
    /// A result row's key, or any node id
    pub key: u32,
}

impl Validate for ViewRevealParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewNodeParams {
    pub view_id: String,
    /// A node of the view's scan (not an aggregate block's key)
    pub node: u32,
}

impl Validate for ViewNodeParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)
    }
}

/// Whether a folder is in the setting treemap.hidden_folders (「不再隐藏这个文件夹」), can be
/// added (「在画面中隐藏这个文件夹」), or why neither (UI-W5-52).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HiddenFolderInfo {
    /// The entries of the list that name this folder, as written; any: it is listed
    pub listed: Vec<String>,
    /// What hiding it adds to the list (its path); "" when it is listed or can't be hidden
    pub path: String,
    /// Why it can't be hidden (interface language); "" when it can or is listed
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewHideFolderParams {
    pub view_id: String,
    /// A folder of the view's scan
    pub node: u32,
    /// True adds it to treemap.hidden_folders; false takes out the entries that name it
    pub hide: bool,
}

impl Validate for ViewHideFolderParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)
    }
}

fn default_results_limit() -> u32 {
    100
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewResultsParams {
    pub view_id: String,
    /// New order (remembered per view); None keeps it
    #[serde(default)]
    pub sort: Option<ResultsSortName>,
    #[serde(default)]
    pub offset: u32,
    #[serde(default = "default_results_limit")]
    pub limit: u32,
}

impl Validate for ViewResultsParams {
    fn validate(&self) -> ValidationResult {
        check_view_id(&self.view_id)?;
        v::int_range("limit", self.limit, 0, 500)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Crumb {
    pub key: u32,
    /// Folder name; 'small:N' / 'more:N' for an aggregate block
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FilterInfo {
    pub text: String,
    pub active: bool,
    /// The text has errors; the previous valid filter is still shown
    pub stale: bool,
    pub mode: FilterModeName,
    #[serde(default)]
    pub files: i64,
    #[serde(default)]
    pub alloc: i64,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub diagnostics: Vec<FilterDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FlashItem {
    /// Row of this frame's draw list: the changed block or its drawn ancestor; -1 = the zoom root (flame.v1 / polar.v1 only)
    pub row: i64,
    /// Change of allocated bytes (the static badge under reduced motion)
    pub delta: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

const_str!(pub struct FlameKind = "flame";);
const_str!(pub struct PolarKind = "polar";);

str_enum! {
    pub enum FlameOrigin {
        Top = "top",
        Bottom = "bottom",
    }
}

/// ``flame.v1``: one band per level, the zoom root's bar in band 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FlameGeometry {
    pub kind: FlameKind,
    /// Side of the canvas holding the root bar
    pub origin: FlameOrigin,
    /// Band pitch in physical px (bar height + gap)
    pub row_h: i32,
    /// Levels drawn beyond the root bar
    pub rows: i32,
    /// Gap between bands and between neighbouring bars, physical px
    pub gap: i32,
    /// The zoom root's bar
    pub root: PixelRect,
}

fn angle_unit() -> i64 {
    ANGLE_UNIT
}

/// ``polar.v1``: the zoom root is the centre disc, level k the k-th ring around it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolarGeometry {
    pub kind: PolarKind,
    /// Centre, physical px
    pub cx: i32,
    pub cy: i32,
    /// Outer edge of the last ring
    pub radius: i32,
    /// Radius of the centre disc
    pub hole: i32,
    /// Ring pitch: ring k spans [hole + (k-1)·ring_w, hole + k·ring_w)
    pub ring_w: i32,
    /// Levels drawn around the centre disc
    pub rings: i32,
    /// Gap between rings and between neighbouring sectors, physical px
    pub gap: i32,
    /// Angle units per turn
    #[serde(default = "angle_unit")]
    pub unit: i64,
}

/// Layout geometry of a frame, tagged by `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(inline)]
pub enum FrameGeometry {
    Flame(FlameGeometry),
    Polar(PolarGeometry),
}

fn default_tone() -> u8 {
    4
}

/// The zoom root of a flame.v1 / polar.v1 frame (root bar, centre disc): the same facts as a row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FrameRoot {
    pub key: u32,
    pub node: i64,
    pub kind: u8,
    pub size: i64,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub flags: u16,
    #[serde(default)]
    pub tag: u8,
    #[serde(default = "default_tone")]
    pub tone: u8,
    #[serde(default)]
    pub branch: u8,
    /// Compare views only
    #[serde(default)]
    pub delta: Option<i64>,
    /// Compare views only
    #[serde(default)]
    pub dstat: Option<u8>,
}

fn treemap() -> String {
    "treemap".to_owned()
}

fn drawlist() -> String {
    DRAWLIST_LAYOUT.to_owned()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameInfo {
    pub view_id: String,
    pub seq: u64,
    /// Canvas size in physical px
    pub width: i64,
    pub height: i64,
    pub dpr: f64,
    pub rows: i64,
    #[serde(default)]
    pub selected_row: Option<i64>,
    pub breadcrumb: Vec<Crumb>,
    /// Allocated bytes below the zoom root
    pub scanned: i64,
    #[serde(default)]
    pub filter: Option<FilterInfo>,
    pub layout_ms: f64,
    #[serde(default)]
    pub reason: String,
    /// Rows changed by a file change since the last frame (UI-TM-43)
    #[serde(default)]
    pub flash: Vec<FlashItem>,
    /// Visualization that drew this frame
    #[serde(default = "treemap")]
    pub visual: String,
    /// Layout of the binary frame that follows
    #[serde(default = "drawlist")]
    pub layout: String,
    /// None for drawlist.v1
    #[serde(default)]
    pub geometry: Option<FrameGeometry>,
    /// The zoom root (flame.v1 / polar.v1)
    #[serde(default)]
    pub root: Option<FrameRoot>,
    /// The numbering of the node ids and keys in this frame: another one once the tree is
    /// renumbered (compaction after file changes, an MFT scan taking over, a comparison computed
    /// again). basket.add takes it with ids from this frame (node-generation.v1)
    #[serde(default)]
    pub generation: u64,
    /// How many folders of the setting treemap.hidden_folders the scan holds, as
    /// ViewState.hidden_folders: it changes while a scan runs or files change, between the calls
    /// that answer with the view's state
    #[serde(default)]
    pub hidden_folders: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ViewState {
    pub view_id: String,
    pub scan_id: String,
    pub zoom_node: u32,
    pub zoom_key: u32,
    pub detail: u32,
    pub min_element: f64,
    pub sort_by_size: bool,
    pub show_free: bool,
    pub show_unknown: bool,
    /// The recycle bins are drawn (not taken out of the picture)
    #[serde(default = "crate::base::yes")]
    pub show_trash: bool,
    /// The scan holds a recycle bin / trash folder (the toggle is offered)
    #[serde(default)]
    pub has_trash: bool,
    /// The folders of the setting treemap.hidden_folders are taken out of the picture (a view
    /// opens with them hidden; one the view is zoomed into stays)
    #[serde(default = "crate::base::yes")]
    pub hide_folders: bool,
    /// How many of those folders the scan holds (the toggle is offered when any)
    #[serde(default)]
    pub hidden_folders: u32,
    /// A visual.list id
    #[serde(default = "treemap")]
    pub visual: String,
    /// Of the current visualization (each remembers its own)
    pub color_mode: ColorModeName,
    /// Colour mode each visualization remembers
    #[serde(default)]
    pub colors: BTreeMap<String, ColorModeName>,
    pub filter_mode: FilterModeName,
    pub viewport: Viewport,
    #[serde(default)]
    pub selected: Option<u32>,
    #[serde(default)]
    pub results_sort: ResultsSortName,
    /// A compare view: areas are changes (W6)
    #[serde(default)]
    pub compare: bool,
    #[serde(default)]
    pub direction: CompareDirection,
    pub can_back: bool,
    pub can_forward: bool,
    pub can_up: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ViewUpdate {
    pub state: ViewState,
    #[serde(default = "crate::base::yes")]
    pub changed: bool,
    /// Present when a new frame was sent
    #[serde(default)]
    pub frame: Option<FrameInfo>,
    /// Current filter state (also without a frame)
    #[serde(default)]
    pub filter: Option<FilterInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ViewListResult {
    pub views: Vec<ViewState>,
}

/// One line of the W3 result list: a drawn matching file, or the matches inside one block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResultRow {
    /// Block to locate with view.reveal (the aggregate's key for 'group')
    pub key: u32,
    /// The file or folder; the containing folder for 'group' and 'rest'
    pub node: u32,
    /// file: drawn on its own · folder: matches inside a folder block · group: matches inside a small-items / +N block · rest: matches too small to draw
    pub kind: ResultKind,
    /// '' for a group of several files and for 'rest'
    pub name: String,
    /// Matching files in this row
    pub count: i64,
    pub alloc: i64,
    pub size: i64,
    /// Folder holding the block, relative to the scan root, platform separators
    pub path: String,
    #[serde(default)]
    pub tag: Option<TagColor>,
    /// The tag comes from an enclosing folder
    #[serde(default)]
    pub tag_inherited: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ViewResults {
    pub view_id: String,
    /// Frame the rows were grouped by
    pub seq: u64,
    /// A filter is in effect (otherwise there are no rows)
    pub active: bool,
    pub sort: ResultsSortName,
    /// Number of rows
    pub total: i64,
    /// Matching files below the zoom root (sum of the rows' counts)
    pub files: i64,
    pub alloc: i64,
    pub size: i64,
    /// Largest row, for the relative size bars
    pub max_alloc: i64,
    pub offset: i64,
    pub rows: Vec<ResultRow>,
    /// The numbering of the rows' node ids and keys (FrameInfo.generation of the frame `seq`)
    #[serde(default)]
    pub generation: u64,
}

/// One visualization this server offers (``visual.list``).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VisualInfo {
    pub id: String,
    /// Layout of its frames
    pub layout: String,
    /// Display name in the session's language
    pub name: String,
    pub icon: String,
    pub order: i64,
    /// Can show compare views
    pub compare: bool,
    /// Shows free / unknown space at a volume root
    pub free_space: bool,
    /// Colour mode a view starts with on this visualization
    pub default_color: ColorModeName,
    /// This session negotiated what it needs
    pub available: bool,
    /// Why it is not available (display text)
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VisualListResult {
    pub visuals: Vec<VisualInfo>,
}

/// ``view.frame``: a frame pushed without a request (scan progress, time-based filters).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ViewFrameEvent {
    pub frame: FrameInfo,
}

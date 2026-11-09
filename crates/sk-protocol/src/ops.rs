//! File operations (M6, docs/manage.md): path formats, the delete basket and its execution (W5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::yes;
use crate::compare::ChangeKind;
use crate::macros::str_enum;
use crate::validate::{self as v, Validate, ValidationResult};

/// Kinds of basket items: the same five as a change item.
pub type BasketItemKind = ChangeKind;

str_enum! {
    pub enum BasketItemStatus {
        Ready = "ready",
        Locked = "locked",
        Missing = "missing",
        Protected = "protected",
    }
}

impl Default for BasketItemStatus {
    fn default() -> Self {
        BasketItemStatus::Ready
    }
}

str_enum! {
    pub enum OpMode {
        Trash = "trash",
        Delete = "delete",
    }
}

impl Default for OpMode {
    fn default() -> Self {
        OpMode::Trash
    }
}

str_enum! {
    pub enum OpCode {
        Ok = "ok",
        Missing = "missing",
        Denied = "denied",
        Locked = "locked",
        Protected = "protected",
        NoTrash = "no_trash",
        Exists = "exists",
        Cancelled = "cancelled",
        AmbiguousName = "ambiguous_name",
        Error = "error",
    }
}

// --------------------------------------------------------------------------- path formats (W5-09…14)

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathFormsParams {
    pub scan_id: String,
    pub node: u32,
}

impl Validate for PathFormsParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::int_range("node", self.node, 1, u32::MAX)
    }
}

/// One path four ways, formatted by the backend so every front end copies the same text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PathForms {
    pub raw: String,
    /// In double quotes (Windows) or shell-quoted (POSIX)
    pub quoted: String,
    /// Forward slashes
    pub posix: String,
    /// file:// URI (RFC 8089), non-ASCII percent-encoded
    pub uri: String,
    /// False for MFT pseudo folders: nothing to open
    #[serde(default = "yes")]
    pub on_disk: bool,
}

// --------------------------------------------------------------------------- the basket (W5-25…40)

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BasketItem {
    pub id: String,
    pub scan_id: String,
    pub path: String,
    pub name: String,
    pub kind: BasketItemKind,
    /// Small items of an aggregate block (kind 'group')
    #[serde(default)]
    pub members: i64,
    /// Bytes freed by deleting it (cloud placeholders, partial hard links: 0)
    pub alloc: i64,
    pub size: i64,
    #[serde(default)]
    pub files: i64,
    #[serde(default)]
    pub dirs: i64,
    /// Mount point of the volume holding it
    pub volume: String,
    #[serde(default)]
    pub status: BasketItemStatus,
    /// Why it cannot be deleted as is (locked by …, missing …)
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub tag: Option<String>,
    /// Changes whenever what the item covers grows or it is added anew (a small-item group that
    /// took in more members keeps its id): basket.execute's `confirm` names the rev the user saw
    /// (basket-confirm.v1)
    #[serde(default)]
    pub rev: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BasketVolume {
    pub mount_point: String,
    pub total: i64,
    pub free: i64,
    /// Bytes the basket frees on this volume
    pub freeable: i64,
    /// The volume has a recycle bin
    pub trash: bool,
    /// Where deleted items go (or why they cannot)
    #[serde(default)]
    pub trash_note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BasketState {
    pub items: Vec<BasketItem>,
    pub volumes: Vec<BasketVolume>,
    /// Bytes freed in all, every volume
    pub freeable: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BasketAddParams {
    pub scan_id: String,
    #[serde(default)]
    pub nodes: Vec<u32>,
    /// With key: the view whose frame holds that block
    #[serde(default)]
    pub view_id: Option<String>,
    /// A block of the view's frame (an aggregate's members)
    #[serde(default)]
    pub key: Option<i64>,
    /// The numbering `nodes` (or `key`) were read in: the `generation` of the FrameInfo,
    /// ViewResults or TreeSummaryResult they came from. Ids of another numbering are refused
    /// (UNKNOWN_NODE: the tree was renumbered since); None: not checked (node-generation.v1)
    #[serde(default)]
    pub generation: Option<u64>,
}

impl Validate for BasketAddParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("scan_id", &self.scan_id, 1, 64)?;
        v::list_len("nodes", &self.nodes, 0, 100_000)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Refusal {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BasketAddResult {
    pub state: BasketState,
    pub added: i64,
    /// Already covered by an item (or one of its folders)
    pub merged: i64,
    /// Items replaced by a folder that holds them
    pub absorbed: i64,
    #[serde(default)]
    pub refused: Vec<Refusal>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BasketRemoveParams {
    pub ids: Vec<String>,
}

impl Validate for BasketRemoveParams {
    fn validate(&self) -> ValidationResult {
        v::list_len("ids", &self.ids, 0, 100_000)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BasketEvent {
    pub state: BasketState,
}

// --------------------------------------------------------------------------- execution (W5-41…49)

/// One item as the user confirmed it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BasketConfirm {
    pub id: String,
    /// Its `rev` then
    pub rev: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BasketExecuteParams {
    pub mode: OpMode,
    /// None: every item
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// The items the user confirmed, as they were shown (instead of `ids`): exactly these run,
    /// and nothing runs (BASKET_CHANGED) when one of them left the basket or changed since
    /// (basket-confirm.v1)
    #[serde(default)]
    pub confirm: Option<Vec<BasketConfirm>>,
}

impl Validate for BasketExecuteParams {
    fn validate(&self) -> ValidationResult {
        if let Some(c) = &self.confirm {
            v::list_len("confirm", c, 0, 100_000)?;
            if self.ids.is_some() {
                return Err(v::ParamsError::new(
                    "ids",
                    "value_error",
                    "ids and confirm cannot both be given",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpTask {
    pub task_id: String,
    pub mode: OpMode,
    pub total: i64,
    /// Permanent deletion waits this long (undo window)
    #[serde(default)]
    pub delay_s: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpTaskParams {
    pub task_id: String,
}

impl Validate for OpTaskParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("task_id", &self.task_id, 1, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpItemResult {
    pub path: String,
    pub ok: bool,
    pub code: OpCode,
    #[serde(default)]
    pub message: String,
}

str_enum! {
    pub enum OpPhase {
        Waiting = "waiting",
        Running = "running",
        Done = "done",
        Cancelled = "cancelled",
        Undone = "undone",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpProgressEvent {
    pub task_id: String,
    #[serde(default)]
    pub mode: OpMode,
    pub done: i64,
    pub total: i64,
    #[serde(default)]
    pub current: String,
    pub phase: OpPhase,
    /// Filled once the task ends
    #[serde(default)]
    pub results: Vec<OpItemResult>,
    /// Bytes moved to the trash / deleted (items that succeeded)
    #[serde(default)]
    pub freed: i64,
    #[serde(default)]
    pub can_undo: bool,
}

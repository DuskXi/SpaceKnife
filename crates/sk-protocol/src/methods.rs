//! Registry of every method, client notification and server event in protocol v1.
//!
//! Each method has a name constant (`SCAN_START = "scan.start"`) and a marker type implementing
//! [`Method`] (`ScanStart`), which ties the name to its params and result types:
//!
//! ```
//! use sk_protocol::methods::{Method, ScanStart};
//! let params: <ScanStart as Method>::Params =
//!     sk_protocol::parse_params_str(r#"{"roots": ["C:\\"]}"#).unwrap();
//! assert_eq!(ScanStart::NAME, "scan.start");
//! assert_eq!(params.roots, ["C:\\"]);
//! ```
//!
//! [`METHODS`], [`NOTIFICATIONS`] and [`EVENTS`] list them all (for routing tables and the schema).

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::base::{Empty, EmptyResult};
use crate::classes::*;
use crate::compare::*;
use crate::export::*;
use crate::filter::*;
use crate::io::*;
use crate::machine::*;
use crate::ops::*;
use crate::palette::*;
use crate::scan::*;
use crate::session::*;
use crate::settings::*;
use crate::streams::*;
use crate::system::*;
use crate::update::*;
use crate::validate::Validate;
use crate::view::*;

/// Produces the (referencing) schema of a type; used to build the protocol schema.
pub type SchemaFn = fn(&mut SchemaGenerator) -> Schema;

fn sub<T: JsonSchema>(g: &mut SchemaGenerator) -> Schema {
    g.subschema_for::<T>()
}

/// A request/response method.
pub trait Method {
    const NAME: &'static str;
    const DOC: &'static str;
    const REQUIRES_SESSION: bool;
    type Params: DeserializeOwned + Serialize + Validate + JsonSchema;
    type Result: DeserializeOwned + Serialize + JsonSchema;
}

/// A client → server notification (no response).
pub trait ClientNotification {
    const NAME: &'static str;
    const DOC: &'static str;
    type Params: DeserializeOwned + Serialize + Validate + JsonSchema;
}

/// A server → client event.
pub trait Event {
    const NAME: &'static str;
    const DOC: &'static str;
    type Payload: DeserializeOwned + Serialize + JsonSchema;
}

#[derive(Clone, Copy, Debug)]
pub struct MethodSpec {
    pub name: &'static str,
    pub doc: &'static str,
    pub requires_session: bool,
    pub params: SchemaFn,
    pub result: SchemaFn,
}

#[derive(Clone, Copy, Debug)]
pub struct NotificationSpec {
    pub name: &'static str,
    pub doc: &'static str,
    pub params: SchemaFn,
}

#[derive(Clone, Copy, Debug)]
pub struct EventSpec {
    pub name: &'static str,
    pub doc: &'static str,
    pub payload: SchemaFn,
}

macro_rules! methods {
    ($( $konst:ident / $marker:ident = $name:literal ( $params:ty ) -> $result:ty $(, session: $session:expr)? ; $doc:expr ; )*) => {
        $(
            pub const $konst: &str = $name;

            #[doc = $doc]
            #[derive(Clone, Copy, Debug)]
            pub struct $marker;

            impl Method for $marker {
                const NAME: &'static str = $name;
                const DOC: &'static str = $doc;
                const REQUIRES_SESSION: bool = methods!(@session $($session)?);
                type Params = $params;
                type Result = $result;
            }
        )*

        /// Every method, in the order of the Python registry.
        pub static METHODS: &[MethodSpec] = &[
            $(MethodSpec {
                name: $name,
                doc: $doc,
                requires_session: methods!(@session $($session)?),
                params: sub::<$params>,
                result: sub::<$result>,
            },)*
        ];
    };
    (@session) => { true };
    (@session $s:expr) => { $s };
}

macro_rules! events {
    ($( $konst:ident / $marker:ident = $name:literal ( $payload:ty ) ; $doc:expr ; )*) => {
        $(
            pub const $konst: &str = $name;

            #[doc = $doc]
            #[derive(Clone, Copy, Debug)]
            pub struct $marker;

            impl Event for $marker {
                const NAME: &'static str = $name;
                const DOC: &'static str = $doc;
                type Payload = $payload;
            }
        )*

        /// Every server event.
        pub static EVENTS: &[EventSpec] = &[
            $(EventSpec { name: $name, doc: $doc, payload: sub::<$payload> },)*
        ];
    };
}

methods! {
    SESSION_HELLO / SessionHello = "session.hello"(HelloParams) -> HelloResult, session: false;
        "Authenticate and negotiate protocol/capabilities.";
    SYSTEM_INFO / SystemInfoMethod = "system.info"(Empty) -> SystemInfo;
        "Server, platform and session information.";
    UPDATE_STATUS / UpdateStatusMethod = "update.status"(Empty) -> UpdateStatus;
        "What the last update check found (updates.v1).";
    UPDATE_CHECK / UpdateCheck = "update.check"(Empty) -> UpdateStatus;
        "Check the GitHub releases now (also when automatic checks are off).";
    UPDATE_DISMISS / UpdateDismiss = "update.dismiss"(UpdateVersionParams) -> UpdateStatus;
        "The card / dialog for this version was closed.";
    UPDATE_MUTE / UpdateMute = "update.mute"(UpdateMuteParams) -> UpdateStatus;
        "Turn the prompts of a series (UpdateStatus.series) off or back on: no card, dialog, badge or automatic download for its releases.";
    UPDATE_DOWNLOAD / UpdateDownloadMethod = "update.download"(UpdateVersionParams) -> UpdateStatus;
        "Download this version's file for this install into the updates folder (progress: update.changed).";
    UPDATE_RELEASES / UpdateReleasesMethod = "update.releases"(Empty) -> UpdateReleases;
        "The recent releases with their notes (the timeline of the About & updates page).";
    UPDATE_CANCEL / UpdateCancel = "update.cancel"(Empty) -> UpdateStatus;
        "Stop a running download.";
    SYSTEM_PING / SystemPing = "system.ping"(PingParams) -> PingResult;
        "Round-trip check; echoes the payload.";
    APP_SHUTDOWN / AppShutdown = "app.shutdown"(Empty) -> ShutdownResult;
        "Ask the backend to shut down gracefully.";
    SUBSCRIBE / Subscribe = "$/subscribe"(SubscribeParams) -> SubscribeResult;
        "Subscribe to an event topic.";
    UNSUBSCRIBE / Unsubscribe = "$/unsubscribe"(UnsubscribeParams) -> EmptyResult;
        "Cancel a subscription.";
    VOLUME_LIST / VolumeList = "volume.list"(Empty) -> VolumeListResult;
        "Volumes offered on the start page (W1).";
    MACHINE_DISKS / MachineDisksMethod = "machine.disks"(Empty) -> MachineDisks;
        "This computer's disks with their links, the volumes on each and what scans tell of each volume.";
    MACHINE_READ_HEALTH / MachineReadHealth = "machine.read_health"(ReadHealthParams) -> ReadHealthResult;
        "Read the disks' own records that need administrator rights (Linux: the elevated helper, behind polkit's password prompt). Only when the user asks; the answer is the whole page again.";
    SCAN_CHECK_PATHS / ScanCheckPaths = "scan.check_paths"(PathCheckParams) -> PathCheckResult;
        "Validate typed paths and merge nested ones (UI-W1-25).";
    SCAN_START / ScanStart = "scan.start"(ScanStartParams) -> ScanStatus;
        "Start scanning one or more roots.";
    SCAN_CANCEL / ScanCancel = "scan.cancel"(ScanIdParams) -> ScanStatus;
        "Stop a running scan (partial tree stays).";
    SCAN_STATUS / ScanStatusMethod = "scan.status"(ScanIdParams) -> ScanStatus;
        "Current status of a scan.";
    SCAN_LIST / ScanList = "scan.list"(Empty) -> ScanListResult;
        "Scans held by the backend.";
    SCAN_CLOSE / ScanClose = "scan.close"(ScanIdParams) -> EmptyResult;
        "Forget a scan and free its memory.";
    SCAN_PAUSE / ScanPause = "scan.pause"(ScanIdParams) -> ScanStatus;
        "Stop dispatching reads; batches in flight finish (W2).";
    SCAN_RESUME / ScanResume = "scan.resume"(ScanIdParams) -> ScanStatus;
        "Continue a paused scan where it stopped.";
    SCAN_USE_MFT / ScanUseMft = "scan.use_mft"(ScanIdParams) -> ScanStatus;
        "Switch a running hard scan to the MFT fast scan (asks for administrator rights; the hard scan goes on until the helper is ready, and on if it cannot start).";
    SCAN_SET_LOAD / ScanSetLoad = "scan.set_load"(ScanSetLoadParams) -> ScanStatus;
        "Change the disk load limit of a running scan (W2).";
    SCAN_CONTINUE / ScanContinue = "scan.continue"(ScanIdParams) -> ScanStatus;
        "Go on with a stopped hard scan: the folders it had not read yet (W2 待定 16).";
    SCAN_RESCAN / ScanRescan = "scan.rescan"(ScanRescanParams) -> ScanStatus;
        "Read a folder of a finished scan again with everything below it, in place (F5, M5).";
    IO_STATUS / IoStatusMethod = "io.status"(Empty) -> IoStatusList;
        "Load state of every local disk, with history.";
    TREE_NODE / TreeNode = "tree.node"(TreeNodeParams) -> TreeNodeResult;
        "One node with its full path.";
    TREE_CHILDREN / TreeChildren = "tree.children"(TreeChildrenParams) -> TreeChildrenResult;
        "A page of a node's children.";
    TREE_FIND / TreeFind = "tree.find"(TreeFindParams) -> TreeFindResult;
        "Look up a node by absolute path.";
    TREE_SUMMARY / TreeSummary = "tree.summary"(TreeSummaryParams) -> TreeSummaryResult;
        "Details-panel data of a node: parent, largest files, per-class totals, newest change, tag, volume.";
    SNAPSHOT_LIST / SnapshotList = "snapshot.list"(Empty) -> SnapshotListResult;
        "Auto-saved snapshots, newest first (W1).";
    SNAPSHOT_SAVE / SnapshotSave = "snapshot.save"(SnapshotSaveParams) -> SnapshotInfo;
        "Save a scan as a snapshot file.";
    SNAPSHOT_LOAD / SnapshotLoad = "snapshot.load"(SnapshotLoadParams) -> ScanStatus;
        "Open a snapshot as a finished scan.";
    FILTER_PARSE / FilterParseMethod = "filter.parse"(FilterParseParams) -> FilterParse;
        "Parse a filter: tokens, diagnostics, pending condition, completion and normal form.";
    FILTER_VOCAB / FilterVocabMethod = "filter.vocab"(Empty) -> FilterVocab;
        "File classes and platform limits for the filter.";
    VIEW_OPEN / ViewOpen = "view.open"(ViewOpenParams) -> ViewUpdate;
        "Open a view (tab / pane) on a scan; sends its first frame.";
    VIEW_CLOSE / ViewClose = "view.close"(ViewIdParams) -> EmptyResult;
        "Close a view.";
    VIEW_LIST / ViewList = "view.list"(ViewListParams) -> ViewListResult;
        "Views of this session (optionally of one scan).";
    VIEW_RENDER / ViewRender = "view.render"(ViewIdParams) -> ViewUpdate;
        "Send a fresh frame of a view.";
    VIEW_ZOOM / ViewZoom = "view.zoom"(ViewZoomParams) -> ViewUpdate;
        "Zoom into a block (double-click / Enter).";
    VIEW_BACK / ViewBack = "view.back"(ViewIdParams) -> ViewUpdate;
        "Back to the previous zoom root.";
    VIEW_FORWARD / ViewForward = "view.forward"(ViewIdParams) -> ViewUpdate;
        "Forward again.";
    VIEW_UP / ViewUp = "view.up"(ViewIdParams) -> ViewUpdate;
        "Zoom to the parent; the folder left is selected.";
    VIEW_HOME / ViewHome = "view.home"(ViewIdParams) -> ViewUpdate;
        "Back to the whole scan / volume.";
    VIEW_SELECT / ViewSelect = "view.select"(ViewSelectParams) -> ViewUpdate;
        "Select a block (or clear the selection).";
    VIEW_SET / ViewSet = "view.set"(ViewSetParams) -> ViewUpdate;
        "Change detail, viewport, switches, colour or filter mode.";
    VIEW_FILTER / ViewFilter = "view.filter"(ViewFilterParams) -> ViewUpdate;
        "Set the view's filter expression.";
    VIEW_TAG / ViewTag = "view.tag"(ViewTagParams) -> ViewUpdate;
        "Tag the selection (Ctrl+1..4) or clear tags (Ctrl+0).";
    VIEW_REVEAL / ViewReveal = "view.reveal"(ViewRevealParams) -> ViewUpdate;
        "Select a block and navigate until it is drawn (W3 click).";
    VIEW_RESULTS / ViewResultsMethod = "view.results"(ViewResultsParams) -> ViewResults;
        "A page of the filter result list, grouped by visible block.";
    VISUAL_LIST / VisualList = "visual.list"(Empty) -> VisualListResult;
        "Visualizations this server offers and whether this session can show each.";
    SETTINGS_GET / SettingsGet = "settings.get"(Empty) -> SettingsState;
        "All settings with their metadata (W7).";
    SETTINGS_UPDATE / SettingsUpdate = "settings.update"(SettingsUpdateParams) -> SettingsState;
        "Change some settings (validated, saved atomically).";
    SETTINGS_RESET / SettingsReset = "settings.reset"(SettingsResetParams) -> SettingsState;
        "Restore the defaults of one page, or of everything.";
    PALETTE_COMMANDS / PaletteCommands = "palette.commands"(Empty) -> PaletteCommandList;
        "All commands with names, icons and default keys.";
    PALETTE_QUERY / PaletteQuery = "palette.query"(PaletteQueryParams) -> PaletteResult;
        "Commands, recents and direct items for the palette input.";
    PALETTE_CHECK / PaletteCheck = "palette.check"(PaletteCheckParams) -> PaletteCommand;
        "One command's state where the user is (a keyboard shortcut runs it only when it is enabled).";
    PALETTE_PATHS / PalettePathsMethod = "palette.paths"(PalettePathsParams) -> PalettePaths;
        "Path group: name search, folder listing or selection seed.";
    PALETTE_PREVIEW / PalettePreviewMethod = "palette.preview"(PalettePreviewParams) -> PalettePreview;
        "Matches of a filter expression on a whole scan.";
    PALETTE_REMEMBER / PaletteRemember = "palette.remember"(PaletteRememberParams) -> EmptyResult;
        "Record a path jump or filter run from the palette.";
    PALETTE_CLEAR / PaletteClear = "palette.clear"(PaletteClearParams) -> EmptyResult;
        "Clear the recents of a scan target and the shared filters.";
    PATH_FORMS / PathFormsMethod = "path.forms"(PathFormsParams) -> PathForms;
        "A node's path four ways (copy path, W5-09…14).";
    BASKET_LIST / BasketList = "basket.list"(Empty) -> BasketState;
        "The delete basket (held by the backend, W5).";
    BASKET_ADD / BasketAdd = "basket.add"(BasketAddParams) -> BasketAddResult;
        "Put nodes (or an aggregate block of a view) into the delete basket; folders absorb their items.";
    BASKET_REMOVE / BasketRemove = "basket.remove"(BasketRemoveParams) -> BasketState;
        "Take items out of the basket.";
    BASKET_CLEAR / BasketClear = "basket.clear"(Empty) -> BasketState;
        "Empty the basket (nothing is deleted).";
    BASKET_EXECUTE / BasketExecute = "basket.execute"(BasketExecuteParams) -> OpTask;
        "Move the items to the recycle bin, or delete them for good after an undo window; progress follows as 'ops.progress' events.";
    OPS_CANCEL / OpsCancel = "ops.cancel"(OpTaskParams) -> EmptyResult;
        "Stop a running file operation.";
    OPS_UNDO / OpsUndo = "ops.undo"(OpTaskParams) -> EmptyResult;
        "Undo a file operation: put trashed items back, or call off a permanent deletion still waiting.";
    COMPARE_SOURCES / CompareSources = "compare.sources"(CompareSourcesParams) -> CompareSourcesResult;
        "What a scan can be compared with: its roots' snapshots and the live tree (W6).";
    COMPARE_OPEN / CompareOpen = "compare.open"(CompareOpenParams) -> CompareTask;
        "Start comparing two trees; 'compare.progress' events follow, then view.open {scan_id: compare_id}.";
    COMPARE_INFO / CompareInfoMethod = "compare.info"(CompareIdParams) -> CompareInfo;
        "Totals and ends of a comparison.";
    COMPARE_CLOSE / CompareClose = "compare.close"(CompareIdParams) -> EmptyResult;
        "Forget a comparison (or stop computing it).";
    COMPARE_SUMMARY / CompareSummaryMethod = "compare.summary"(CompareSummaryParams) -> CompareSummary;
        "Net change, growth and shrink lists and the fill-up estimate for a compare view's zoom root and filter.";
    COMPARE_NODE / CompareNode = "compare.node"(CompareNodeParams) -> CompareNodeResult;
        "One block of a compare view with both sizes and its children that changed most (hover card).";
    CLASSES_GET / ClassesGet = "classes.get"(Empty) -> ClassesState;
        "The file class table (W7 文件类别).";
    CLASSES_UPDATE / ClassesUpdate = "classes.update"(ClassesUpdateParams) -> ClassesState;
        "Replace the file class table (order = colour precedence); views recolour and filters re-run.";
    CLASSES_RESET / ClassesReset = "classes.reset"(Empty) -> ClassesState;
        "Back to the built-in file classes.";
    CLASSES_FOLDER / ClassesFolder = "classes.folder"(ClassesFolderParams) -> ClassesState;
        "Choose the class of a folder's files (empty: by extension only), or forget the choice (UI-W7-38).";
    EXPORT_TARGETS / ExportTargetsMethod = "export.targets"(Empty) -> ExportTargets;
        "Built-in export templates and formats (W4 「导出报告…」).";
    EXPORT_CHECK / ExportCheck = "export.check"(ExportCheckParams) -> ExportCheckResult;
        "Diagnostics of a template's text.";
    EXPORT_RUN / ExportRun = "export.run"(ExportRunParams) -> ExportRunResult;
        "Write a view's tree (zoom root or everything, filtered or not) to a file with a template or format.";
    SETTINGS_EXPORT / SettingsExport = "settings.export"(SettingsFileParams) -> EmptyResult;
        "Write the settings (and edited file classes) to a file.";
    SETTINGS_IMPORT / SettingsImport = "settings.import"(SettingsImportParams) -> SettingsImportResult;
        "Read a SpaceKnife settings export or a SpaceSniffer configuration: what would change, then apply it.";
    SYSTEM_MAINTENANCE / SystemMaintenance = "system.maintenance"(Empty) -> MaintenanceInfo;
        "Scan helper, cache, snapshots and saved tags (W7 高级).";
    SYSTEM_HELPER_STOP / SystemHelperStop = "system.helper_stop"(Empty) -> MaintenanceInfo;
        "Stop the scan helper.";
    SYSTEM_CACHE_CLEAR / SystemCacheClear = "system.cache_clear"(Empty) -> MaintenanceInfo;
        "Delete the cache files nothing uses right now.";
    TAGS_CLEAR_SAVED / TagsClearSaved = "tags.clear_saved"(Empty) -> MaintenanceInfo;
        "Forget every remembered tag (all scan roots).";
}

// ------------------------------------------------------------------------ notifications

pub const CANCEL: &str = "$/cancel";

/// Cancel an in-flight request (answered with -32800).
#[derive(Clone, Copy, Debug)]
pub struct Cancel;

impl ClientNotification for Cancel {
    const NAME: &'static str = CANCEL;
    const DOC: &'static str = "Cancel an in-flight request (answered with -32800).";
    type Params = CancelParams;
}

/// Every client notification.
pub static NOTIFICATIONS: &[NotificationSpec] = &[NotificationSpec {
    name: Cancel::NAME,
    doc: Cancel::DOC,
    params: sub::<CancelParams>,
}];

// ------------------------------------------------------------------------ events

events! {
    EVENT_LOG / LogRecord = "log.record"(LogRecordEvent);
        "A backend log record for a 'log' subscription.";
    EVENT_PROGRESS / Progress = "$/progress"(ProgressEvent);
        "Progress of a long-running request.";
    EVENT_SCAN_PROGRESS / ScanProgress = "scan.progress"(ScanProgressEvent);
        "Status of a scan for a 'scan' subscription.";
    EVENT_VIEW_FRAME / ViewFrame = "view.frame"(ViewFrameEvent);
        "A frame pushed without a request (scan progress, time).";
    EVENT_SETTINGS_CHANGED / SettingsChanged = "settings.changed"(SettingsChangedEvent);
        "Settings changed (any client, or the file was edited).";
    EVENT_IO_LOAD / IoLoad = "io.load"(IoStatusEvent);
        "Load state of a disk for an 'io' subscription (2 Hz).";
    EVENT_BASKET / BasketChanged = "basket.changed"(BasketEvent);
        "The delete basket changed (every session).";
    EVENT_OPS_PROGRESS / OpsProgress = "ops.progress"(OpProgressEvent);
        "Progress and results of a file operation.";
    EVENT_COMPARE_PROGRESS / CompareProgress = "compare.progress"(CompareProgressEvent);
        "A comparison is computing, ready (with its totals) or failed.";
    EVENT_CLASSES / ClassesChanged = "classes.changed"(ClassesEvent);
        "The file class table changed (every session).";
    EVENT_UPDATE / UpdateChanged = "update.changed"(UpdateChangedEvent);
        "Update check result or download progress (sessions with updates.v1).";
}

/// Look up a method by name.
pub fn method(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|m| m.name == name)
}

/// Look up an event by name.
pub fn event(name: &str) -> Option<&'static EventSpec> {
    EVENTS.iter().find(|e| e.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique() {
        let names: HashSet<_> = METHODS.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), METHODS.len());
        assert_eq!(METHODS.len(), 91);
        let events: HashSet<_> = EVENTS.iter().map(|e| e.name).collect();
        assert_eq!(events.len(), EVENTS.len());
        assert_eq!(EVENTS.len(), 11);
        assert!(!method(SESSION_HELLO).unwrap().requires_session);
        assert!(method(SCAN_START).unwrap().requires_session);
        assert_eq!(method("visual.list").unwrap().name, VisualList::NAME);
    }
}

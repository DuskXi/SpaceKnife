"""Registry of every method, client notification and server event in protocol v1.

Both the backend router and every client import these specs, so a method's name and its
params/result models are defined exactly once.
"""

from __future__ import annotations

from dataclasses import dataclass

from pydantic import BaseModel

from .base import Empty, EmptyResult
from .classes import ClassesEvent, ClassesFolderParams, ClassesState, ClassesUpdateParams
from .compare import (
    CompareIdParams,
    CompareInfo,
    CompareNodeParams,
    CompareNodeResult,
    CompareOpenParams,
    CompareProgressEvent,
    CompareSourcesParams,
    CompareSourcesResult,
    CompareSummary,
    CompareSummaryParams,
    CompareTask,
)
from .export import ExportCheckParams, ExportCheckResult, ExportRunParams, ExportRunResult, ExportTargets
from .filter import FilterParse, FilterParseParams, FilterVocab
from .io import IoStatusEvent, IoStatusList
from .machine import MachineDisks, ReadHealthParams, ReadHealthResult
from .ops import (
    BasketAddParams,
    BasketAddResult,
    BasketEvent,
    BasketExecuteParams,
    BasketRemoveParams,
    BasketState,
    OpProgressEvent,
    OpTask,
    OpTaskParams,
    PathForms,
    PathFormsParams,
)
from .palette import (
    PaletteCheckParams,
    PaletteClearParams,
    PaletteCommand,
    PaletteCommandList,
    PalettePaths,
    PalettePathsParams,
    PalettePreview,
    PalettePreviewParams,
    PaletteQueryParams,
    PaletteRememberParams,
    PaletteResult,
)
from .plugins import (
    ExtensionsChangedEvent,
    ExtensionsState,
    InsightNodeParams,
    InsightNodeResult,
    InsightNodesParams,
    InsightNodesResult,
    PluginChangedEvent,
    PluginClipboardEvent,
    PluginDescribeResult,
    PluginEnableParams,
    PluginFieldsParams,
    PluginFieldsResult,
    PluginIdParams,
    PluginInfo,
    PluginInstallParams,
    PluginInstallResult,
    PluginLinkParams,
    PluginListResult,
    PluginProgressEvent,
    PluginToastEvent,
    PluginUninstallParams,
)
from .scan import (
    PathCheckParams,
    PathCheckResult,
    ScanIdParams,
    ScanListResult,
    ScanProgressEvent,
    ScanRescanParams,
    ScanSetLoadParams,
    ScanStartParams,
    ScanStatus,
    SnapshotInfo,
    SnapshotListResult,
    SnapshotLoadParams,
    SnapshotSaveParams,
    TreeChildrenParams,
    TreeChildrenResult,
    TreeFindParams,
    TreeFindResult,
    TreeNodeParams,
    TreeNodeResult,
    TreeSummaryParams,
    TreeSummaryResult,
    VolumeListResult,
)
from .session import HelloParams, HelloResult
from .settings import (
    MaintenanceInfo,
    SettingsChangedEvent,
    SettingsFileParams,
    SettingsImportParams,
    SettingsImportResult,
    SettingsResetParams,
    SettingsState,
    SettingsUpdateParams,
)
from .streams import (
    CancelParams,
    LogRecordEvent,
    ProgressEvent,
    SubscribeParams,
    SubscribeResult,
    UnsubscribeParams,
)
from .system import PingParams, PingResult, ShutdownResult, SystemInfo
from .update import UpdateChangedEvent, UpdateMuteParams, UpdateReleases, UpdateStatus, UpdateVersionParams
from .view import (
    ViewColors,
    ViewColorsParams,
    ViewFilterParams,
    ViewFrameEvent,
    ViewIdParams,
    ViewListParams,
    ViewListResult,
    ViewOpenParams,
    ViewResults,
    ViewResultsParams,
    ViewRevealParams,
    ViewSelectParams,
    ViewSetParams,
    ViewTagParams,
    ViewUpdate,
    ViewZoomParams,
    VisualListResult,
)


@dataclass(frozen=True, slots=True)
class MethodSpec[P: BaseModel, R: BaseModel]:
    """A request/response method."""

    name: str
    params: type[P]
    result: type[R]
    doc: str
    requires_session: bool = True


@dataclass(frozen=True, slots=True)
class NotificationSpec[P: BaseModel]:
    """A client → server notification (no response)."""

    name: str
    params: type[P]
    doc: str


@dataclass(frozen=True, slots=True)
class EventSpec[E: BaseModel]:
    """A server → client event."""

    name: str
    payload: type[E]
    doc: str


SESSION_HELLO = MethodSpec(
    "session.hello", HelloParams, HelloResult, "Authenticate and negotiate protocol/capabilities.", False
)
SYSTEM_INFO = MethodSpec("system.info", Empty, SystemInfo, "Server, platform and session information.")
SYSTEM_PING = MethodSpec("system.ping", PingParams, PingResult, "Round-trip check; echoes the payload.")
APP_SHUTDOWN = MethodSpec("app.shutdown", Empty, ShutdownResult, "Ask the backend to shut down gracefully.")
SUBSCRIBE = MethodSpec("$/subscribe", SubscribeParams, SubscribeResult, "Subscribe to an event topic.")
UNSUBSCRIBE = MethodSpec("$/unsubscribe", UnsubscribeParams, EmptyResult, "Cancel a subscription.")

SCAN_CHECK_PATHS = MethodSpec(
    "scan.check_paths",
    PathCheckParams,
    PathCheckResult,
    "Validate typed paths and merge nested ones (UI-W1-25).",
)
VOLUME_LIST = MethodSpec("volume.list", Empty, VolumeListResult, "Volumes offered on the start page (W1).")
SCAN_START = MethodSpec("scan.start", ScanStartParams, ScanStatus, "Start scanning one or more roots.")
SCAN_CANCEL = MethodSpec("scan.cancel", ScanIdParams, ScanStatus, "Stop a running scan (partial tree stays).")
SCAN_STATUS = MethodSpec("scan.status", ScanIdParams, ScanStatus, "Current status of a scan.")
SCAN_LIST = MethodSpec("scan.list", Empty, ScanListResult, "Scans held by the backend.")
SCAN_CLOSE = MethodSpec("scan.close", ScanIdParams, EmptyResult, "Forget a scan and free its memory.")
SCAN_PAUSE = MethodSpec(
    "scan.pause", ScanIdParams, ScanStatus, "Stop dispatching reads; batches in flight finish (W2)."
)
SCAN_RESUME = MethodSpec("scan.resume", ScanIdParams, ScanStatus, "Continue a paused scan where it stopped.")
SCAN_USE_MFT = MethodSpec(
    "scan.use_mft",
    ScanIdParams,
    ScanStatus,
    "Switch a running hard scan to the MFT fast scan (asks for administrator rights; the hard scan "
    "goes on until the helper is ready, and on if it cannot start).",
)
SCAN_SET_LOAD = MethodSpec(
    "scan.set_load", ScanSetLoadParams, ScanStatus, "Change the disk load limit of a running scan (W2)."
)
SCAN_CONTINUE = MethodSpec(
    "scan.continue",
    ScanIdParams,
    ScanStatus,
    "Go on with a stopped hard scan: the folders it had not read yet (W2 待定 16).",
)
SCAN_RESCAN = MethodSpec(
    "scan.rescan",
    ScanRescanParams,
    ScanStatus,
    "Read a folder of a finished scan again with everything below it, in place (F5, M5).",
)
IO_STATUS = MethodSpec("io.status", Empty, IoStatusList, "Load state of every local disk, with history.")
TREE_NODE = MethodSpec("tree.node", TreeNodeParams, TreeNodeResult, "One node with its full path.")
TREE_CHILDREN = MethodSpec(
    "tree.children", TreeChildrenParams, TreeChildrenResult, "A page of a node's children."
)
TREE_SUMMARY = MethodSpec(
    "tree.summary",
    TreeSummaryParams,
    TreeSummaryResult,
    "Details-panel data of a node: parent, largest files, per-class totals, newest change, tag, volume.",
)
TREE_FIND = MethodSpec("tree.find", TreeFindParams, TreeFindResult, "Look up a node by absolute path.")
SNAPSHOT_LIST = MethodSpec(
    "snapshot.list", Empty, SnapshotListResult, "Auto-saved snapshots, newest first (W1)."
)
SNAPSHOT_SAVE = MethodSpec(
    "snapshot.save", SnapshotSaveParams, SnapshotInfo, "Save a scan as a snapshot file."
)
SNAPSHOT_LOAD = MethodSpec(
    "snapshot.load", SnapshotLoadParams, ScanStatus, "Open a snapshot as a finished scan."
)

FILTER_PARSE = MethodSpec(
    "filter.parse",
    FilterParseParams,
    FilterParse,
    "Parse a filter: tokens, diagnostics, pending condition, completion and normal form.",
)
FILTER_VOCAB = MethodSpec(
    "filter.vocab", Empty, FilterVocab, "File classes and platform limits for the filter."
)

VIEW_OPEN = MethodSpec(
    "view.open", ViewOpenParams, ViewUpdate, "Open a view (tab / pane) on a scan; sends its first frame."
)
VIEW_CLOSE = MethodSpec("view.close", ViewIdParams, EmptyResult, "Close a view.")
VIEW_LIST = MethodSpec(
    "view.list", ViewListParams, ViewListResult, "Views of this session (optionally of one scan)."
)
VIEW_RENDER = MethodSpec("view.render", ViewIdParams, ViewUpdate, "Send a fresh frame of a view.")
VIEW_ZOOM = MethodSpec("view.zoom", ViewZoomParams, ViewUpdate, "Zoom into a block (double-click / Enter).")
VIEW_BACK = MethodSpec("view.back", ViewIdParams, ViewUpdate, "Back to the previous zoom root.")
VIEW_FORWARD = MethodSpec("view.forward", ViewIdParams, ViewUpdate, "Forward again.")
VIEW_UP = MethodSpec("view.up", ViewIdParams, ViewUpdate, "Zoom to the parent; the folder left is selected.")
VIEW_HOME = MethodSpec("view.home", ViewIdParams, ViewUpdate, "Back to the whole scan / volume.")
VIEW_SELECT = MethodSpec(
    "view.select", ViewSelectParams, ViewUpdate, "Select a block (or clear the selection)."
)
VIEW_SET = MethodSpec(
    "view.set", ViewSetParams, ViewUpdate, "Change detail, viewport, switches, colour or filter mode."
)
VIEW_FILTER = MethodSpec("view.filter", ViewFilterParams, ViewUpdate, "Set the view's filter expression.")
VIEW_TAG = MethodSpec(
    "view.tag", ViewTagParams, ViewUpdate, "Tag the selection (Ctrl+1..4) or clear tags (Ctrl+0)."
)
SETTINGS_GET = MethodSpec("settings.get", Empty, SettingsState, "All settings with their metadata (W7).")
SETTINGS_UPDATE = MethodSpec(
    "settings.update",
    SettingsUpdateParams,
    SettingsState,
    "Change some settings (validated, saved atomically).",
)
SETTINGS_RESET = MethodSpec(
    "settings.reset",
    SettingsResetParams,
    SettingsState,
    "Restore the defaults of one page, or of everything.",
)
PALETTE_COMMANDS = MethodSpec(
    "palette.commands", Empty, PaletteCommandList, "All commands with names, icons and default keys."
)
PALETTE_QUERY = MethodSpec(
    "palette.query",
    PaletteQueryParams,
    PaletteResult,
    "Commands, recents and direct items for the palette input.",
)
PALETTE_CHECK = MethodSpec(
    "palette.check",
    PaletteCheckParams,
    PaletteCommand,
    "One command's state where the user is (a keyboard shortcut runs it only when it is enabled).",
)
PALETTE_PATHS = MethodSpec(
    "palette.paths",
    PalettePathsParams,
    PalettePaths,
    "Path group: name search, folder listing or selection seed.",
)
PALETTE_PREVIEW = MethodSpec(
    "palette.preview", PalettePreviewParams, PalettePreview, "Matches of a filter expression on a whole scan."
)
PALETTE_REMEMBER = MethodSpec(
    "palette.remember",
    PaletteRememberParams,
    EmptyResult,
    "Record a path jump or filter run from the palette.",
)
PALETTE_CLEAR = MethodSpec(
    "palette.clear",
    PaletteClearParams,
    EmptyResult,
    "Clear the recents of a scan target and the shared filters.",
)
VIEW_REVEAL = MethodSpec(
    "view.reveal", ViewRevealParams, ViewUpdate, "Select a block and navigate until it is drawn (W3 click)."
)
VIEW_RESULTS = MethodSpec(
    "view.results",
    ViewResultsParams,
    ViewResults,
    "A page of the filter result list, grouped by visible block.",
)
COLORS_OF = MethodSpec(
    "colors.of",
    ViewColorsParams,
    ViewColors,
    "What colours some nodes in a view (its colour mode or another): classes, age bands, branch "
    "slots, a plugin's palette indices. Web panels paint with these (sk.colors).",
)
VISUAL_LIST = MethodSpec(
    "visual.list",
    Empty,
    VisualListResult,
    "Visualizations this server offers and whether this session can show each.",
)

CANCEL = NotificationSpec("$/cancel", CancelParams, "Cancel an in-flight request (answered with -32800).")

EVENT_LOG = EventSpec("log.record", LogRecordEvent, "A backend log record for a 'log' subscription.")
EVENT_PROGRESS = EventSpec("$/progress", ProgressEvent, "Progress of a long-running request.")
EVENT_SCAN_PROGRESS = EventSpec(
    "scan.progress", ScanProgressEvent, "Status of a scan for a 'scan' subscription."
)

PATH_FORMS = MethodSpec(
    "path.forms", PathFormsParams, PathForms, "A node's path four ways (copy path, W5-09…14)."
)
BASKET_LIST = MethodSpec("basket.list", Empty, BasketState, "The delete basket (held by the backend, W5).")
BASKET_ADD = MethodSpec(
    "basket.add",
    BasketAddParams,
    BasketAddResult,
    "Put nodes (or an aggregate block of a view) into the delete basket; folders absorb their items.",
)
BASKET_REMOVE = MethodSpec("basket.remove", BasketRemoveParams, BasketState, "Take items out of the basket.")
BASKET_CLEAR = MethodSpec("basket.clear", Empty, BasketState, "Empty the basket (nothing is deleted).")
BASKET_EXECUTE = MethodSpec(
    "basket.execute",
    BasketExecuteParams,
    OpTask,
    "Move the items to the recycle bin, or delete them for good after an undo window; progress follows "
    "as 'ops.progress' events.",
)
OPS_CANCEL = MethodSpec("ops.cancel", OpTaskParams, EmptyResult, "Stop a running file operation.")
OPS_UNDO = MethodSpec(
    "ops.undo",
    OpTaskParams,
    EmptyResult,
    "Undo a file operation: put trashed items back, or call off a permanent deletion still waiting.",
)
COMPARE_SOURCES = MethodSpec(
    "compare.sources",
    CompareSourcesParams,
    CompareSourcesResult,
    "What a scan can be compared with: its roots' snapshots and the live tree (W6).",
)
COMPARE_OPEN = MethodSpec(
    "compare.open",
    CompareOpenParams,
    CompareTask,
    "Start comparing two trees; 'compare.progress' events follow, then view.open {scan_id: compare_id}.",
)
COMPARE_INFO = MethodSpec("compare.info", CompareIdParams, CompareInfo, "Totals and ends of a comparison.")
COMPARE_CLOSE = MethodSpec(
    "compare.close", CompareIdParams, EmptyResult, "Forget a comparison (or stop computing it)."
)
COMPARE_SUMMARY = MethodSpec(
    "compare.summary",
    CompareSummaryParams,
    CompareSummary,
    "Net change, growth and shrink lists and the fill-up estimate for a compare view's zoom root and filter.",
)
COMPARE_NODE = MethodSpec(
    "compare.node",
    CompareNodeParams,
    CompareNodeResult,
    "One block of a compare view with both sizes and its children that changed most (hover card).",
)
CLASSES_GET = MethodSpec("classes.get", Empty, ClassesState, "The file class table (W7 文件类别).")
CLASSES_UPDATE = MethodSpec(
    "classes.update",
    ClassesUpdateParams,
    ClassesState,
    "Replace the file class table (order = colour precedence); views recolour and filters re-run.",
)
CLASSES_RESET = MethodSpec("classes.reset", Empty, ClassesState, "Back to the built-in file classes.")
CLASSES_FOLDER = MethodSpec(
    "classes.folder",
    ClassesFolderParams,
    ClassesState,
    "Choose the class of a folder's files (empty: by extension only), or forget the choice (UI-W7-38).",
)
EXPORT_TARGETS = MethodSpec(
    "export.targets", Empty, ExportTargets, "Built-in export templates and formats (W4 「导出报告…」)."
)
EXPORT_CHECK = MethodSpec(
    "export.check", ExportCheckParams, ExportCheckResult, "Diagnostics of a template's text."
)
EXPORT_RUN = MethodSpec(
    "export.run",
    ExportRunParams,
    ExportRunResult,
    "Write a view's tree (zoom root or everything, filtered or not) to a file with a template or format.",
)
SETTINGS_EXPORT = MethodSpec(
    "settings.export",
    SettingsFileParams,
    EmptyResult,
    "Write the settings (and edited file classes) to a file.",
)
SETTINGS_IMPORT = MethodSpec(
    "settings.import",
    SettingsImportParams,
    SettingsImportResult,
    "Read a SpaceKnife settings export or a SpaceSniffer configuration: what would change, then apply it.",
)
SYSTEM_MAINTENANCE = MethodSpec(
    "system.maintenance", Empty, MaintenanceInfo, "Scan helper, cache, snapshots and saved tags (W7 高级)."
)
SYSTEM_HELPER_STOP = MethodSpec("system.helper_stop", Empty, MaintenanceInfo, "Stop the scan helper.")
SYSTEM_CACHE_CLEAR = MethodSpec(
    "system.cache_clear", Empty, MaintenanceInfo, "Delete the cache files nothing uses right now."
)
TAGS_CLEAR_SAVED = MethodSpec(
    "tags.clear_saved", Empty, MaintenanceInfo, "Forget every remembered tag (all scan roots)."
)

UPDATE_STATUS = MethodSpec(
    "update.status", Empty, UpdateStatus, "What the last update check found (updates.v1)."
)
UPDATE_CHECK = MethodSpec(
    "update.check", Empty, UpdateStatus, "Check the GitHub releases now (also when automatic checks are off)."
)
UPDATE_DISMISS = MethodSpec(
    "update.dismiss", UpdateVersionParams, UpdateStatus, "The card / dialog for this version was closed."
)
UPDATE_MUTE = MethodSpec(
    "update.mute",
    UpdateMuteParams,
    UpdateStatus,
    "Turn the prompts of a series (UpdateStatus.series) off or back on: no card, dialog, badge or automatic"
    " download for its releases.",
)
UPDATE_DOWNLOAD = MethodSpec(
    "update.download",
    UpdateVersionParams,
    UpdateStatus,
    "Download this version's file for this install into the updates folder (progress: update.changed).",
)
UPDATE_CANCEL = MethodSpec("update.cancel", Empty, UpdateStatus, "Stop a running download.")
UPDATE_RELEASES = MethodSpec(
    "update.releases",
    Empty,
    UpdateReleases,
    "The recent releases with their notes (the timeline of the About & updates page).",
)
MACHINE_DISKS = MethodSpec(
    "machine.disks",
    Empty,
    MachineDisks,
    "This computer's disks with their links, the volumes on each and what scans tell of each volume.",
)
MACHINE_READ_HEALTH = MethodSpec(
    "machine.read_health",
    ReadHealthParams,
    ReadHealthResult,
    "Read the disks' own records that need administrator rights (Linux: the elevated helper, behind "
    "polkit's password prompt). Only when the user asks; the answer is the whole page again.",
)
PLUGIN_LIST = MethodSpec(
    "plugin.list", Empty, PluginListResult, "Installed plugins with their state (plugins.v1)."
)
PLUGIN_DESCRIBE = MethodSpec(
    "plugin.describe",
    PluginIdParams,
    PluginDescribeResult,
    "What enabling a plugin means: its permissions, what it adds, key conflicts and its current hash.",
)
PLUGIN_ENABLE = MethodSpec(
    "plugin.enable",
    PluginEnableParams,
    PluginInfo,
    "Enable a plugin the user confirmed: the hash and permissions must be those plugin.describe showed.",
)
PLUGIN_DISABLE = MethodSpec(
    "plugin.disable", PluginIdParams, PluginInfo, "Turn a plugin off (its settings and data stay)."
)
PLUGIN_RESTART = MethodSpec(
    "plugin.restart", PluginIdParams, PluginInfo, "Restart a plugin's process (also after it crashed)."
)
PLUGIN_INSTALL = MethodSpec(
    "plugin.install",
    PluginInstallParams,
    PluginInstallResult,
    "Copy a plugin folder or unpack a .zip into the plugins folder (never enables it).",
)
PLUGIN_UNINSTALL = MethodSpec(
    "plugin.uninstall",
    PluginUninstallParams,
    EmptyResult,
    "Turn a plugin off and move its folder (and with remove_data its data) to the recycle bin.",
)
PLUGIN_LINK = MethodSpec(
    "plugin.link",
    PluginLinkParams,
    PluginInfo,
    "Developer mode: use a plugin folder in place (no hash pinning; reloaded when its files change).",
)
EXTENSIONS_GET = MethodSpec(
    "extensions.get",
    Empty,
    ExtensionsState,
    "What enabled plugins add: reports, panels, node fields, cleanups, colour modes, sources, presets.",
)
PLUGIN_FIELDS = MethodSpec(
    "plugin.fields",
    PluginFieldsParams,
    PluginFieldsResult,
    "The plugin fields of a node for the details panel or the hover card (each plugin time-boxed).",
)
INSIGHT_NODE = MethodSpec(
    "insight.node",
    InsightNodeParams,
    InsightNodeResult,
    "Insight cards of a node: matching rules first, then what plugins say.",
)
INSIGHT_NODES = MethodSpec(
    "insight.nodes",
    InsightNodesParams,
    InsightNodesResult,
    "The insight rules' card of each of many nodes (no plugin code runs; for filters and plugins).",
)
EVENT_EXTENSIONS = EventSpec(
    "extensions.changed",
    ExtensionsChangedEvent,
    "Plugins came, went or changed what they add: fetch the listed kinds again (plugins.v1).",
)
EVENT_PLUGIN = EventSpec("plugin.changed", PluginChangedEvent, "A plugin's state changed (plugins.v1).")
EVENT_PLUGIN_TOAST = EventSpec("plugin.toast", PluginToastEvent, "A plugin shows a short message.")
EVENT_PLUGIN_CLIPBOARD = EventSpec(
    "plugin.clipboard", PluginClipboardEvent, "A plugin puts text on the clipboard (it has clipboard)."
)
EVENT_PLUGIN_PROGRESS = EventSpec(
    "plugin.progress", PluginProgressEvent, "Progress of a plugin call (reports, cleanups)."
)
EVENT_UPDATE = EventSpec(
    "update.changed",
    UpdateChangedEvent,
    "Update check result or download progress (sessions with updates.v1).",
)

METHODS: dict[str, MethodSpec[BaseModel, BaseModel]] = {
    spec.name: spec
    for spec in (
        SESSION_HELLO,
        SYSTEM_INFO,
        UPDATE_STATUS,
        UPDATE_CHECK,
        UPDATE_DISMISS,
        UPDATE_MUTE,
        UPDATE_DOWNLOAD,
        UPDATE_RELEASES,
        UPDATE_CANCEL,
        SYSTEM_PING,
        APP_SHUTDOWN,
        SUBSCRIBE,
        UNSUBSCRIBE,
        VOLUME_LIST,
        MACHINE_DISKS,
        MACHINE_READ_HEALTH,
        SCAN_CHECK_PATHS,
        SCAN_START,
        SCAN_CANCEL,
        SCAN_STATUS,
        SCAN_LIST,
        SCAN_CLOSE,
        SCAN_PAUSE,
        SCAN_RESUME,
        SCAN_USE_MFT,
        SCAN_SET_LOAD,
        SCAN_CONTINUE,
        SCAN_RESCAN,
        IO_STATUS,
        TREE_NODE,
        TREE_CHILDREN,
        TREE_FIND,
        TREE_SUMMARY,
        SNAPSHOT_LIST,
        SNAPSHOT_SAVE,
        SNAPSHOT_LOAD,
        FILTER_PARSE,
        FILTER_VOCAB,
        VIEW_OPEN,
        VIEW_CLOSE,
        VIEW_LIST,
        VIEW_RENDER,
        VIEW_ZOOM,
        VIEW_BACK,
        VIEW_FORWARD,
        VIEW_UP,
        VIEW_HOME,
        VIEW_SELECT,
        VIEW_SET,
        VIEW_FILTER,
        VIEW_TAG,
        VIEW_REVEAL,
        VIEW_RESULTS,
        VISUAL_LIST,
        COLORS_OF,
        SETTINGS_GET,
        SETTINGS_UPDATE,
        SETTINGS_RESET,
        PALETTE_COMMANDS,
        PALETTE_QUERY,
        PALETTE_CHECK,
        PALETTE_PATHS,
        PALETTE_PREVIEW,
        PALETTE_REMEMBER,
        PALETTE_CLEAR,
        PATH_FORMS,
        BASKET_LIST,
        BASKET_ADD,
        BASKET_REMOVE,
        BASKET_CLEAR,
        BASKET_EXECUTE,
        OPS_CANCEL,
        OPS_UNDO,
        COMPARE_SOURCES,
        COMPARE_OPEN,
        COMPARE_INFO,
        COMPARE_CLOSE,
        COMPARE_SUMMARY,
        COMPARE_NODE,
        CLASSES_GET,
        CLASSES_UPDATE,
        CLASSES_RESET,
        CLASSES_FOLDER,
        EXPORT_TARGETS,
        EXPORT_CHECK,
        EXPORT_RUN,
        SETTINGS_EXPORT,
        SETTINGS_IMPORT,
        SYSTEM_MAINTENANCE,
        SYSTEM_HELPER_STOP,
        SYSTEM_CACHE_CLEAR,
        TAGS_CLEAR_SAVED,
        PLUGIN_LIST,
        PLUGIN_DESCRIBE,
        PLUGIN_ENABLE,
        PLUGIN_DISABLE,
        PLUGIN_RESTART,
        PLUGIN_INSTALL,
        PLUGIN_UNINSTALL,
        PLUGIN_LINK,
        EXTENSIONS_GET,
        PLUGIN_FIELDS,
        INSIGHT_NODE,
        INSIGHT_NODES,
    )
}
NOTIFICATIONS: dict[str, NotificationSpec[BaseModel]] = {spec.name: spec for spec in (CANCEL,)}
EVENT_VIEW_FRAME = EventSpec(
    "view.frame", ViewFrameEvent, "A frame pushed without a request (scan progress, time)."
)

EVENT_IO_LOAD = EventSpec("io.load", IoStatusEvent, "Load state of a disk for an 'io' subscription (2 Hz).")

EVENT_SETTINGS_CHANGED = EventSpec(
    "settings.changed", SettingsChangedEvent, "Settings changed (any client, or the file was edited)."
)

EVENT_BASKET = EventSpec("basket.changed", BasketEvent, "The delete basket changed (every session).")
EVENT_OPS_PROGRESS = EventSpec("ops.progress", OpProgressEvent, "Progress and results of a file operation.")
EVENT_CLASSES = EventSpec("classes.changed", ClassesEvent, "The file class table changed (every session).")
EVENT_COMPARE_PROGRESS = EventSpec(
    "compare.progress", CompareProgressEvent, "A comparison is computing, ready (with its totals) or failed."
)

EVENTS: dict[str, EventSpec[BaseModel]] = {
    spec.name: spec
    for spec in (
        EVENT_LOG,
        EVENT_PROGRESS,
        EVENT_SCAN_PROGRESS,
        EVENT_VIEW_FRAME,
        EVENT_SETTINGS_CHANGED,
        EVENT_IO_LOAD,
        EVENT_BASKET,
        EVENT_OPS_PROGRESS,
        EVENT_COMPARE_PROGRESS,
        EVENT_CLASSES,
        EVENT_UPDATE,
        EVENT_EXTENSIONS,
        EVENT_PLUGIN,
        EVENT_PLUGIN_TOAST,
        EVENT_PLUGIN_CLIPBOARD,
        EVENT_PLUGIN_PROGRESS,
    )
}

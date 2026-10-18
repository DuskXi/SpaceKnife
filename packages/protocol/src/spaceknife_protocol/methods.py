"""Registry of every method, client notification and server event in protocol v1.

Both the backend router and every client import these specs, so a method's name and its
params/result models are defined exactly once.
"""

from __future__ import annotations

from dataclasses import dataclass

from pydantic import BaseModel

from .base import Empty, EmptyResult
from .filter import FilterParse, FilterParseParams, FilterVocab
from .io import IoStatusEvent, IoStatusList
from .palette import (
    PaletteClearParams,
    PaletteCommandList,
    PalettePaths,
    PalettePathsParams,
    PalettePreview,
    PalettePreviewParams,
    PaletteQueryParams,
    PaletteRememberParams,
    PaletteResult,
)
from .scan import (
    PathCheckParams,
    PathCheckResult,
    ScanIdParams,
    ScanListResult,
    ScanProgressEvent,
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
from .settings import SettingsChangedEvent, SettingsResetParams, SettingsState, SettingsUpdateParams
from .streams import (
    CancelParams,
    LogRecordEvent,
    ProgressEvent,
    SubscribeParams,
    SubscribeResult,
    UnsubscribeParams,
)
from .system import PingParams, PingResult, ShutdownResult, SystemInfo
from .view import (
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

CANCEL = NotificationSpec("$/cancel", CancelParams, "Cancel an in-flight request (answered with -32800).")

EVENT_LOG = EventSpec("log.record", LogRecordEvent, "A backend log record for a 'log' subscription.")
EVENT_PROGRESS = EventSpec("$/progress", ProgressEvent, "Progress of a long-running request.")
EVENT_SCAN_PROGRESS = EventSpec(
    "scan.progress", ScanProgressEvent, "Status of a scan for a 'scan' subscription."
)

METHODS: dict[str, MethodSpec[BaseModel, BaseModel]] = {
    spec.name: spec
    for spec in (
        SESSION_HELLO,
        SYSTEM_INFO,
        SYSTEM_PING,
        APP_SHUTDOWN,
        SUBSCRIBE,
        UNSUBSCRIBE,
        VOLUME_LIST,
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
        SETTINGS_GET,
        SETTINGS_UPDATE,
        SETTINGS_RESET,
        PALETTE_COMMANDS,
        PALETTE_QUERY,
        PALETTE_PATHS,
        PALETTE_PREVIEW,
        PALETTE_REMEMBER,
        PALETTE_CLEAR,
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

EVENTS: dict[str, EventSpec[BaseModel]] = {
    spec.name: spec
    for spec in (
        EVENT_LOG,
        EVENT_PROGRESS,
        EVENT_SCAN_PROGRESS,
        EVENT_VIEW_FRAME,
        EVENT_SETTINGS_CHANGED,
        EVENT_IO_LOAD,
    )
}

"""Plugins (docs/plugins.md): management, the extension registry as the frontends see it, actions,
report documents, node fields, insight cards and cleanups.

Everything here is part of the core schema. A plugin's own methods and events (``plugin.<id>.<name>``)
are not: each plugin's schema is generated on its own, so the core schema does not depend on what is
installed.

Names: a plugin id is two or more dotted lowercase words (``example.dupes``, ``acme.zip-source``); a
contribution's local name is one lowercase word with underscores (``find``, ``min_size``); its full id
is ``<plugin id>.<local name>``, its wire name ``plugin.<plugin id>.<local name>``.
"""

from __future__ import annotations

import re
from typing import Annotated, Any, Literal

from pydantic import Field

from .base import Params, Payload
from .settings import SettingValue
from .text import LocalStr


class ThemedColor(Payload):
    """A colour per theme (``{light = "#…", dark = "#…"}``); the app draws the one of its theme."""

    light: str
    dark: str


Tone = str | ThemedColor
"""A tone of a report block or field: accent, ok, warn, danger, muted, a design token, class:<id>,
palette:<colour mode id>#<index>, #RRGGBB, or a :class:`ThemedColor`."""

PLUGIN_ID_PATTERN = r"^[a-z][a-z0-9]*(?:-[a-z0-9]+)*(?:\.[a-z][a-z0-9]*(?:-[a-z0-9]+)*)+$"
LOCAL_NAME_PATTERN = r"^[a-z][a-z0-9]*(?:_[a-z0-9]+)*$"
PLUGIN_ID_MAX = 32
LOCAL_NAME_MAX = 24
FULL_ID_MAX = PLUGIN_ID_MAX + 1 + LOCAL_NAME_MAX  # 57
WIRE_PREFIX = "plugin."
# First segments a plugin id may not use: shipped plugins, the app, and every built-in namespace of
# commands and methods (a plugin command can never collide with a built-in one).
RESERVED_FIRST = frozenset(
    {
        "builtin",
        "sk",
        "spaceknife",
        "plugin",
        "plugins",
        "app",
        "view",
        "scan",
        "tab",
        "tag",
        "tags",
        "nav",
        "item",
        "basket",
        "snapshot",
        "report",
        "filter",
        "path",
        "update",
        "settings",
        "system",
        "tree",
        "ops",
        "compare",
        "classes",
        "export",
        "machine",
        "palette",
        "volume",
        "io",
        "visual",
        "session",
        "log",
        "insight",
        "extensions",
    }
)
_PLUGIN_ID_RE = re.compile(PLUGIN_ID_PATTERN)
_LOCAL_RE = re.compile(LOCAL_NAME_PATTERN)

PluginId = Annotated[str, Field(min_length=3, max_length=PLUGIN_ID_MAX, pattern=PLUGIN_ID_PATTERN)]
LocalName = Annotated[str, Field(min_length=1, max_length=LOCAL_NAME_MAX, pattern=LOCAL_NAME_PATTERN)]


def valid_plugin_id(text: str, *, builtin: bool = False) -> bool:
    """Whether ``text`` is a plugin id (``builtin.*`` only for shipped plugins)."""
    if len(text) > PLUGIN_ID_MAX or not _PLUGIN_ID_RE.fullmatch(text):
        return False
    first = text.split(".", 1)[0]
    return (first == "builtin") if builtin else first not in RESERVED_FIRST


def valid_local_name(text: str) -> bool:
    return len(text) <= LOCAL_NAME_MAX and _LOCAL_RE.fullmatch(text) is not None


def full_id(plugin: str, name: str) -> str:
    return f"{plugin}.{name}"


def wire_name(plugin: str, name: str) -> str:
    """``plugin.<plugin id>.<local name>``: a plugin method, event, state or stream on the socket."""
    return f"{WIRE_PREFIX}{plugin}.{name}"


def split_wire_name(name: str) -> tuple[str, str] | None:
    """``plugin.example.dupes.find`` -> ``("example.dupes", "find")``; None for anything else."""
    if not name.startswith(WIRE_PREFIX):
        return None
    plugin, dot, local = name[len(WIRE_PREFIX) :].rpartition(".")
    if not dot or not valid_local_name(local) or not _PLUGIN_ID_RE.fullmatch(plugin):
        return None
    return plugin, local


# --------------------------------------------------------------------------- permissions and states

Permission = Literal[
    "tree.read",  # names, paths, sizes and times of the scans
    "files.read",  # the contents of files inside the scanned folders
    "snapshots.read",  # saved snapshots
    "basket.propose",  # put files into the delete basket (deleting still needs the user)
    "net",  # connect to the hosts listed in the manifest
    "exec",  # run the programs listed in the manifest
    "host.open",  # open or reveal files, open a terminal or a web page
    "clipboard",  # write to the clipboard
    "notify",  # notices with a button; a system notification while the window is minimized
]
PERMISSIONS: tuple[Permission, ...] = (
    "tree.read",
    "files.read",
    "snapshots.read",
    "basket.propose",
    "net",
    "exec",
    "host.open",
    "clipboard",
    "notify",
)

PluginState = Literal[
    "enabled",
    "disabled",  # installed, turned off by the user
    "needs_confirm",  # never enabled: the user has not seen its permissions yet
    "changed",  # its files changed since the user confirmed it
    "incompatible",  # needs another plugin API or host version
    "error",  # its manifest or declarations are broken
    "crashed",  # its process crashed too often; waits for a restart
]
ProcessState = Literal["stopped", "starting", "running", "idle", "crashed"]
PluginRuntimeKind = Literal["declarative", "process", "inprocess"]


class PermissionInfo(Payload):
    id: Permission
    text: str = Field(description="What it allows, in the interface language")
    warn: bool = Field(default=False, description="Stands out: any host, any program")


class PluginIssue(Payload):
    code: str
    message: str
    file: str = ""
    line: int = 0
    severity: Literal["error", "warning"] = "error"


class PluginProcess(Payload):
    state: ProcessState
    pid: int | None = None
    memory: int = Field(default=0, description="Resident bytes at the last check (0: unknown)")
    restarts: int = 0
    last_exit: int | None = Field(default=None, description="Exit code of the last process that ended")
    stderr: list[str] = Field(default_factory=list, description="Last lines it wrote to stderr")


class PluginBrief(Payload):
    id: str
    name: str
    icon: str = ""
    state: PluginState
    version: str


class PluginInfo(Payload):
    """One installed plugin, for the plugins page."""

    id: str
    version: str
    name: str
    description: str = ""
    author: str = ""
    icon: str = ""
    homepage: str = ""
    license: str = ""
    state: PluginState
    builtin: bool = False
    dev: bool = Field(default=False, description="A linked development folder: no hash pinning")
    folder: str
    hash: str = Field(default="", description="sha256 of its files (what the user confirmed)")
    runtime: PluginRuntimeKind = "declarative"
    permissions: list[PermissionInfo] = Field(default_factory=list)
    net_hosts: list[str] = Field(default_factory=list)
    exec: list[str] = Field(default_factory=list)
    granted: list[Permission] = Field(default_factory=list)
    contributions: dict[str, int] = Field(default_factory=dict, description="Count per contribution kind")
    keys: list[str] = Field(default_factory=list, description="Keys its commands bind")
    issues: list[PluginIssue] = Field(default_factory=list)
    process: PluginProcess | None = None
    schema_hash: str = ""


class PluginListResult(Payload):
    plugins: list[PluginInfo]
    folder: str = Field(description="The plugins folder (data_dir/plugins)")
    dev_mode: bool = Field(description="Developer mode: in-process plugins and development links allowed")


class PluginIdParams(Params):
    id: PluginId


class ContributionGroup(Payload):
    """What a plugin adds of one kind, for the enable dialog (``plugin enable`` prints the same)."""

    kind: str
    title: str = Field(description="The kind, in the interface language")
    count: int
    items: list[str] = Field(default_factory=list, description="Each one's title (at most 50)")


class KeyInfo(Payload):
    key: str
    command: str = Field(description="The command's full id")
    title: str = Field(description="The command's title")
    conflict: str = Field(default="", description="The command that already has the key ('' = none)")


class PluginDescribeResult(Payload):
    """What the enable dialog shows before the user confirms."""

    plugin: PluginInfo
    added: list[Permission] = Field(
        default_factory=list, description="Permissions the confirmed version did not ask for (an update)"
    )
    conflicts: list[str] = Field(default_factory=list, description="Key conflicts, as text")
    summary: list[str] = Field(default_factory=list, description="What it adds, as text lines")
    groups: list[ContributionGroup] = Field(
        default_factory=list, description="What it adds, by kind (its code's contributions too)"
    )
    keys: list[KeyInfo] = Field(default_factory=list, description="The keys it binds")
    unsigned: bool = True


class PluginEnableParams(Params):
    id: PluginId
    hash: str = Field(default="", max_length=80, description="The hash the user saw (plugin.describe)")
    grant: list[Permission] = Field(default_factory=list, description="Every permission it asks for")


class PluginInstallParams(Params):
    path: str = Field(min_length=1, max_length=32767, description="A plugin folder or a .zip")
    replace: bool = False


class PluginInstallResult(Payload):
    id: str
    version: str
    installed: bool
    conflict: bool = Field(default=False, description="A plugin with this id is installed: confirm, replace")
    existing_version: str = ""


class PluginUninstallParams(Params):
    id: PluginId
    remove_data: bool = False


class PluginLinkParams(Params):
    path: str = Field(min_length=1, max_length=32767, description="A plugin folder under development")


# --------------------------------------------------------------------------- actions


class NodeRef(Payload):
    """A node of a scan as a plugin saw it; ``epoch`` tells whether the id still means the same node."""

    scan_id: str
    node: int
    epoch: int = 0
    path: str = ""


class _Action(Payload):
    pass


class CallAction(_Action):
    kind: Literal["call"] = "call"
    method: str = Field(description="Wire name of a plugin method (plugin.<id>.<name>)")
    args: dict[str, Any] = Field(default_factory=dict)


class OpenAction(_Action):
    kind: Literal["open"] = "open"
    target: str = Field(description="report:<full id> | panel:<full id> | settings:<plugin id>")
    args: dict[str, Any] = Field(
        default_factory=dict,
        description="ReportContext.args of the report, or the panel's sk.context.args (an open "
        "panel is told again: sk.onContext)",
    )


class CommandAction(_Action):
    kind: Literal["command"] = "command"
    command: str
    args: dict[str, Any] = Field(
        default_factory=dict,
        description="Given to the command's own action (added to its args); report.export: format "
        "(a format or template), scope (zoom | all | selection), options, use_filter",
    )


class ViewAction(_Action):
    kind: Literal["view"] = "view"
    settings: dict[str, Any] = Field(description="ViewSettingsParams fields")


class CycleAction(_Action):
    kind: Literal["cycle"] = "cycle"
    what: Literal["visual", "color"]
    step: int = 1


class ToggleAction(_Action):
    kind: Literal["toggle"] = "toggle"
    name: Literal["show_free", "show_unknown", "show_trash", "sort_by_size"]


class FilterAction(_Action):
    kind: Literal["filter"] = "filter"
    expr: str


class SettingAction(_Action):
    kind: Literal["setting"] = "setting"
    key: str
    value: SettingValue | None = None
    toggle: bool = False


class CleanupAction(_Action):
    kind: Literal["cleanup"] = "cleanup"
    cleanup: str = Field(description="Full id of a cleanup contribution")
    method: str = Field(description="Its wire name")
    args: dict[str, Any] = Field(default_factory=dict, description="The cleanup's ReportContext.args")


NodeOp = Literal["reveal", "open", "terminal", "copy_path", "select", "zoom", "basket"]


class NodeAction(_Action):
    kind: Literal["node"] = "node"
    op: NodeOp


class UrlAction(_Action):
    kind: Literal["url"] = "url"
    url: str = Field(pattern=r"^https://", max_length=2048)


class ScanSourceAction(_Action):
    kind: Literal["scan_source"] = "scan_source"
    source: str = Field(description="Full id of a scan source")
    uri: str = Field(default="$node", max_length=4096, description='What to list; "$node": the selected node')


class BuiltinAction(_Action):
    kind: Literal["builtin"] = "builtin"
    name: str = Field(description="A built-in frontend handler (by command id)")


Action = Annotated[
    CallAction
    | OpenAction
    | CommandAction
    | ViewAction
    | CycleAction
    | ToggleAction
    | FilterAction
    | SettingAction
    | CleanupAction
    | NodeAction
    | UrlAction
    | ScanSourceAction
    | BuiltinAction,
    Field(discriminator="kind"),
]
ACTION_KINDS = (
    "call",
    "open",
    "command",
    "view",
    "cycle",
    "toggle",
    "filter",
    "setting",
    "cleanup",
    "node",
    "url",
    "scan_source",
    "builtin",
)


class LabeledAction(Payload):
    label: LocalStr
    icon: str = ""
    action: Action


class ActionContext(Payload):
    """Where the user is when an action runs (sent as the params of a ``call`` action)."""

    scan_id: str | None = None
    view_id: str | None = None
    node: int | None = None
    selection: list[int] = Field(default_factory=list, max_length=1000)
    compare_id: str | None = None
    args: dict[str, Any] = Field(default_factory=dict)
    zoom_node: int | None = Field(default=None, description="The view's zoom root (where the user is)")
    filter: str = Field(default="", description="The view's filter expression ('' = not filtered)")


class ReportContext(Payload):
    """The params of a report, a cleanup and a code insight: what it is about."""

    scan_id: str | None = None
    view_id: str | None = None
    node: int | None = None
    selection: list[int] = Field(default_factory=list, max_length=1000)
    compare_id: str | None = None
    cursor: str | None = Field(default=None, description="A table's next page (Table.more)")
    args: dict[str, Any] = Field(default_factory=dict)
    zoom_node: int | None = Field(default=None, description="The view's zoom root (where the user is)")
    filter: str = Field(default="", description="The view's filter expression ('' = not filtered)")


# --------------------------------------------------------------------------- report documents

Unit = Literal["size", "count", "pct", "date", "ago", "duration", "bytes_per_s", "text", "path"]
Cell = LocalStr | int | float | bool | None  # a text cell may be T(…): written in the request's language
MAX_REPORT_BYTES = 2 << 20
MAX_TABLE_ROWS = 2000
MAX_BLOCKS = 200
MAX_DEPTH = 4


class StatItem(Payload):
    label: LocalStr
    value: Cell
    unit: Unit = "count"
    hint: LocalStr = ""
    tone: Tone | None = None
    delta: float | None = None


class KvItem(Payload):
    label: LocalStr
    value: Cell
    unit: Unit = "text"
    node: NodeRef | None = None


class TableColumn(Payload):
    key: str
    label: LocalStr
    unit: Unit = "text"
    align: Literal["start", "end", "center"] | None = None
    grow: float | None = None


class TableRow(Payload):
    cells: dict[str, Cell]
    node: NodeRef | None = None
    actions: list[LabeledAction] = Field(default_factory=list)


class TableMore(Payload):
    method: str = Field(description="Wire name of the report to call with cursor for the next page")
    cursor: str


class BarItem(Payload):
    label: LocalStr
    value: float
    unit: Unit = "size"
    node: NodeRef | None = None
    tone: Tone | None = None


class PartItem(Payload):
    label: LocalStr
    value: float
    tone: Tone | None = None


class NodeItem(Payload):
    node: NodeRef
    label: LocalStr = ""
    size: int | None = None
    reason: LocalStr = ""


class MdBlock(Payload):
    type: Literal["md"] = "md"
    text: LocalStr


class StatsBlock(Payload):
    type: Literal["stats"] = "stats"
    items: list[StatItem]


class KvBlock(Payload):
    type: Literal["kv"] = "kv"
    items: list[KvItem]


class TableBlock(Payload):
    type: Literal["table"] = "table"
    columns: list[TableColumn]
    rows: list[TableRow]
    sort: str | None = Field(default=None, description="Column key, '-key' for descending")
    page_size: int = Field(default=50, ge=1, le=500)
    select: bool = False
    more: TableMore | None = None


class BarsBlock(Payload):
    type: Literal["bars"] = "bars"
    items: list[BarItem]
    max: float | None = None


class StackBlock(Payload):
    type: Literal["stack"] = "stack"
    parts: list[PartItem]
    unit: Unit = "size"


class RingBlock(Payload):
    type: Literal["ring"] = "ring"
    parts: list[PartItem]
    unit: Unit = "size"
    center: LocalStr = ""


class MeterBlock(Payload):
    type: Literal["meter"] = "meter"
    value: float
    max: float
    label: LocalStr = ""
    unit: Unit = "size"
    tone: Tone | None = None


class SparkBlock(Payload):
    type: Literal["spark"] = "spark"
    values: list[float]
    labels: list[LocalStr] = Field(default_factory=list)
    unit: Unit = "size"


class NodesBlock(Payload):
    type: Literal["nodes"] = "nodes"
    items: list[NodeItem]
    actions: list[LabeledAction] = Field(default_factory=list)


class CalloutBlock(Payload):
    type: Literal["callout"] = "callout"
    tone: Literal["info", "ok", "warn", "danger"] = "info"
    text: LocalStr


class ProgressBlock(Payload):
    type: Literal["progress"] = "progress"
    done: float
    total: float
    text: LocalStr = ""


class SectionBlock(Payload):
    type: Literal["section"] = "section"
    title: LocalStr
    blocks: list[Block]
    collapsed: bool = False


Block = Annotated[
    MdBlock
    | StatsBlock
    | KvBlock
    | TableBlock
    | BarsBlock
    | StackBlock
    | RingBlock
    | MeterBlock
    | SparkBlock
    | NodesBlock
    | CalloutBlock
    | ProgressBlock
    | SectionBlock,
    Field(discriminator="type"),
]
BLOCK_TYPES = (
    "md",
    "stats",
    "kv",
    "table",
    "bars",
    "stack",
    "ring",
    "meter",
    "spark",
    "nodes",
    "callout",
    "progress",
    "section",
)
SectionBlock.model_rebuild()


class Report(Payload):
    """A report document: blocks the frontends draw with their own components."""

    type: Literal["report"] = "report"
    title: LocalStr = ""
    subtitle: LocalStr = ""
    blocks: list[Block] = Field(default_factory=list)
    actions: list[LabeledAction] = Field(default_factory=list)
    footnote: LocalStr = ""
    empty: LocalStr = Field(default="", description="Shown instead of the blocks when there are none")


class Toast(Payload):
    type: Literal["toast"] = "toast"
    text: LocalStr
    tone: Literal["info", "ok", "warn", "danger"] = "info"


class CompareOpen(Payload):
    """What a command's method may return to show a comparison (compare mode, W6): two snapshot
    files, or a snapshot and ``"now"`` (the live tree of a scan of the same folders). The window
    uses the tab that shows those folders; for ``"now"`` without one it scans them first, for two
    snapshots it opens ``target`` and compares ``base`` with it. Needs ``snapshots.read``."""

    type: Literal["compare"] = "compare"
    base: str = Field(min_length=1, max_length=32767, description="A snapshot file, or 'now'")
    target: str = Field(default="now", min_length=1, max_length=32767, description="Same as base")
    scan_id: str | None = Field(default=None, max_length=64, description="The scan whose tab compares")


# --------------------------------------------------------------------------- fields, insights, cleanups

FieldSlot = Literal["inspector", "hover"]


class FieldValue(Payload):
    """What a node field shows: a value with its unit, optionally a link or an action."""

    type: Literal["field"] = "field"
    value: Cell
    unit: Unit = "text"
    tone: Tone | None = None
    node: NodeRef | None = None
    action: LabeledAction | None = None


class PluginFieldsParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    node: int = Field(ge=0)
    slot: FieldSlot = "inspector"


class PluginField(Payload):
    id: str = Field(description="Full id of the field contribution")
    plugin: str
    title: str
    value: Cell
    unit: Unit = "text"
    tone: Tone | None = None
    node: NodeRef | None = None
    action: LabeledAction | None = None


class PluginFieldsResult(Payload):
    fields: list[PluginField]
    pending: bool = Field(default=False, description="Hover: some fields were not cached yet; ask again")


class InsightCard(Payload):
    source: Literal["rule", "plugin"]
    plugin: str | None = None
    rule: str = ""
    title: str
    reason: str
    kind: str = Field(description="Built-in kinds keep their icons; plugins may name others")
    cleanup: str = "review"
    app: str = ""
    note: str = ""
    command: str = Field(default="", description="Shown and copyable, never run")
    path: str = ""
    priority: int = Field(
        default=0, ge=-1000, le=1000, description="The rule's or the plugin's: cards are shown highest first"
    )


class InsightNodeParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    node: int = Field(ge=0)


class InsightNodeResult(Payload):
    cards: list[InsightCard]


class InsightNodesParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    nodes: list[int] = Field(max_length=20_000)


class InsightNodesResult(Payload):
    cards: list[InsightCard | None] = Field(description="The rules' card of each node (None: no rule)")


class CleanupItem(Payload):
    node: NodeRef
    name: str
    path: str
    alloc: int
    size: int
    reason: str = ""
    checked: bool = True


class CleanupResult(Payload):
    """Candidates of a cleanup: the review dialog adds the checked ones to the basket with origin."""

    type: Literal["cleanup"] = "cleanup"
    origin: str = Field(description="Full id of the cleanup (BasketAddParams.origin)")
    title: str = ""
    items: list[CleanupItem]
    freeable: int = 0
    dropped: int = Field(default=0, description="Candidates left out: unknown, gone or not on disk")


# --------------------------------------------------------------------------- the registry for frontends

ReportSlot = Literal["tab", "inspector", "dialog", "start", "compare", "status", "history"]
PanelSlot = Literal["tab", "inspector", "dialog"]


class ReportInfo(Payload):
    id: str
    plugin: str
    title: str
    icon: str = ""
    slot: ReportSlot = "tab"
    refresh: list[str] = Field(default_factory=list)
    when: list[str] = Field(default_factory=list)
    order: int = 0
    method: str = Field(description="Wire name to call with a ReportContext")
    static: bool = Field(default=False, description="Declared blocks only: the method just returns them")


class PanelInfo(Payload):
    id: str
    plugin: str
    title: str
    short_title: str = Field(default="", description="For tabs and narrow places ('' = the title)")
    icon: str = ""
    slot: PanelSlot = "tab"
    entry: str = "web/index.html"
    height: int = 320
    when: list[str] = Field(default_factory=list)
    permissions: list[Permission] = Field(default_factory=list, description="What its bridge may do")
    params: dict[str, str | int | float | bool] = Field(
        default_factory=dict, description="Given to the page in its context (sk.context().params)"
    )
    keep: bool = Field(default=True, description="A tab in the background keeps its page")


class FieldInfo(Payload):
    id: str
    plugin: str
    title: str
    slots: list[FieldSlot]
    order: int = 0


class CleanupInfo(Payload):
    id: str
    plugin: str
    title: str
    description: str = ""
    icon: str = ""
    when: list[str] = Field(default_factory=list)
    method: str


PaletteEntry = str | ThemedColor


class ColorModeInfo(Payload):
    id: str
    plugin: str | None = None
    title: str
    palette: list[PaletteEntry] = Field(
        default_factory=list, description="0: neutral; semantic name, token, class:<id>, #RRGGBB or themed"
    )
    legend: list[str] = Field(default_factory=list)
    legend_colors: list[PaletteEntry | None] = Field(
        default_factory=list, description="A colour per legend entry from the plugin (None: the palette's)"
    )
    folders: Literal["ring", "palette"] = "ring"
    layouts: list[str] = Field(default_factory=list, description="Layouts it applies to (empty: all)")


class SourceInfo(Payload):
    id: str
    plugin: str
    title: str
    icon: str = ""
    pick: Literal["folder", "file", "text"] = "text"
    extensions: list[str] = Field(default_factory=list)
    placeholder: str = ""
    on_disk: bool = False
    ops: bool = False
    watch: bool = False
    size: Literal["alloc", "size"] = Field(default="alloc", description="What its tree's sizes are")


class PresetInfo(Payload):
    id: str
    plugin: str
    title: str
    expr: str


class ExtensionsState(Payload):
    version: int = Field(description="Bumped by every change of the registry")
    plugins: list[PluginBrief]
    reports: list[ReportInfo] = Field(default_factory=list)
    panels: list[PanelInfo] = Field(default_factory=list)
    node_fields: list[FieldInfo] = Field(default_factory=list)
    cleanups: list[CleanupInfo] = Field(default_factory=list)
    color_modes: list[ColorModeInfo] = Field(default_factory=list)
    sources: list[SourceInfo] = Field(default_factory=list)
    filter_presets: list[PresetInfo] = Field(default_factory=list)
    icons: dict[str, str] = Field(default_factory=dict, description="<plugin>:<name> -> sanitized SVG")


class ExtensionsChangedEvent(Payload):
    version: int
    kinds: list[str]


class PluginChangedEvent(Payload):
    id: str
    state: PluginState
    process: ProcessState | None = None


class PluginToastEvent(Payload):
    plugin: str
    text: str
    tone: Literal["info", "ok", "warn", "danger"] = "info"


class PluginNoticeEvent(Payload):
    """A plugin's ``ctx.notify`` (it has ``notify``): a notice that stays a while, with a button
    running one of its commands; while the window is minimized the system shows it as well."""

    plugin: str
    id: str = Field(max_length=64, description="Unique per notice (one system notification each)")
    title: str = Field(max_length=120)
    body: str = Field(default="", max_length=1000)
    tone: Literal["info", "ok", "warn", "danger"] = "info"
    command: str | None = Field(default=None, max_length=128, description="One of the plugin's commands")
    button: str = Field(default="", max_length=40, description="The button's text (else the command's title)")
    args: dict[str, Any] = Field(default_factory=dict, description="Added to the command's action args")


class PluginClipboardEvent(Payload):
    """A plugin's ``ctx.clipboard.write_text`` (it has ``clipboard``): the frontend puts the text on
    the clipboard and says so. Only the session the call came from gets it."""

    plugin: str
    text: str = Field(max_length=1 << 20)


class PluginProgressEvent(Payload):
    """Progress of a running plugin call (reports, cleanups): ``request`` is the caller's request id."""

    plugin: str
    request: int | str | None = None
    done: float
    total: float
    text: str = ""


PluginTaskState = Literal["running", "restarting", "done", "failed", "cancelled"]


class PluginTaskInfo(Payload):
    """A plugin's background task (``@plugin.background``) as the host keeps track of it: running
    ones, and the last few that ended (how they ended). The ``plugin.task`` event sends one whenever
    it changes (progress at most a few times a second)."""

    id: str = Field(description="Unique while the backend runs")
    plugin: str
    name: str = Field(description="The task's local name")
    key: str = Field(default="", description="Which one of that name (a scan, a folder …), set by the plugin")
    title: str = Field(description="In the interface language")
    state: PluginTaskState
    done: float = 0.0
    total: float = Field(default=0.0, description="0: no measure of how far it is")
    text: str = Field(default="", description="What it is doing (ctx.progress), in the interface language")
    started: float = Field(description="Unix seconds")
    ended: float | None = None
    attempt: int = Field(default=1, description="2 and up: started again after the plugin crashed")
    error: str = Field(default="", description="Why it failed")


class PluginTaskListResult(Payload):
    tasks: list[PluginTaskInfo]


class PluginTaskCancelParams(Params):
    id: str = Field(min_length=1, max_length=160)


# --------------------------------------------------------------------------- documents

# What the frontends read from, or send to, a plugin's own methods (``plugin.<id>.<name>``): no core
# method carries these, so the schema lists them by themselves (``documents``) and the TypeScript
# types are generated like every other model. "input": the frontends send it (fields may be left out).
DOCUMENTS: dict[str, tuple[type[Payload], Literal["input", "output"]]] = {
    "report": (Report, "output"),
    "toast": (Toast, "output"),
    "compare": (CompareOpen, "output"),
    "cleanup": (CleanupResult, "output"),
    "action_context": (ActionContext, "input"),
    "report_context": (ReportContext, "input"),
}

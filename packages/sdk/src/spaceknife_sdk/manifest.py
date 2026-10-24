"""``plugin.toml`` and the declaration of every contribution kind.

One model per kind: the TOML table ``[[commands]]`` and the Python call ``plugin.command(...)`` build
the same :class:`CommandSpec`, so a contribution has one field list wherever it is written.
Code-only kinds (reports with code, fields, cleanups, …) are declared by decorators; their spec
models are the same either way.
"""

from __future__ import annotations

import difflib
import re
import tomllib
from pathlib import Path
from typing import Annotated, Any, Literal

from pydantic import (
    BaseModel,
    BeforeValidator,
    ConfigDict,
    Field,
    ValidationError,
    field_validator,
    model_validator,
)

from spaceknife_protocol.plugins import (
    PERMISSIONS,
    PLUGIN_ID_MAX,
    PLUGIN_ID_PATTERN,
    FieldSlot,
    PanelSlot,
    Permission,
    ReportSlot,
    ThemedColor,
    valid_local_name,
    valid_plugin_id,
)
from spaceknife_protocol.settings import SettingApplies, SettingControl, SettingValue
from spaceknife_protocol.text import Text, TextLike

from .actions import normalize_action
from .errors import Issue, ManifestError

PLUGIN_API = 1
MANIFEST = "plugin.toml"
MAX_PALETTE = 16
SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")
WHEN_WORDS = (
    "scan",
    "done",
    "scanning",
    "view",
    "selection",
    "scan_view",
    "compare",
    "file",
    "dir",
    "on_disk",
)
WHEN_PREFIXES = ("setting:", "cap:", "plugin:", "source:", "ext:")
MENUS = ("node", "inspector", "toolbar", "start")
REFRESH_WORDS = ("open", "selection", "scan", "frame", "compare", "manual")
HOOKS = ("start", "stop", "scan_done", "scan_closed", "settings", "lang")
COLOR_BY = ("ext", "class", "age", "size", "depth", "name", "set", "insight")
SIZE_METRICS = ("alloc", "size", "files", "items", "code")
BASE_VISUALS = ("treemap", "flame", "flame_polar")
ALL_LAYOUTS = ("drawlist.v1", "flame.v1", "polar.v1")


def _local(v: str) -> str:
    if not valid_local_name(v):
        raise ValueError(f"{v!r} is not a local name (lowercase letters, digits and '_', up to 24)")
    return v


LocalName = Annotated[str, BeforeValidator(_local)]


class Spec(BaseModel):
    """Base of every declaration: unknown fields are errors (a typo never goes unnoticed)."""

    model_config = ConfigDict(extra="forbid", frozen=True, populate_by_name=True)


def _when(values: list[str]) -> list[str]:
    for w in values:
        if w not in WHEN_WORDS and not w.startswith(WHEN_PREFIXES):
            raise ValueError(
                f"unknown condition {w!r}: one of {', '.join(WHEN_WORDS)} or "
                "setting:/cap:/plugin:/source:/ext:"
            )
    return values


When = Annotated[list[str], BeforeValidator(lambda v: _when(list(v)))]


# --------------------------------------------------------------------------- user interface


class StatusSpec(Spec):
    state: LocalName
    text: TextLike


class CommandSpec(Spec):
    name: LocalName
    title: TextLike
    icon: str = ""
    keys: list[str] = Field(default_factory=list, max_length=4)
    aliases: list[str] = Field(default_factory=list, max_length=16)
    when: When = Field(default_factory=list)
    menus: list[Literal["node", "inspector", "toolbar", "start"]] = Field(default_factory=list)
    action: dict[str, Any] | None = None
    status: StatusSpec | None = None
    typed_only: bool = False
    order: int = 0
    check: LocalName | None = Field(default=None, description="A method returning why it is unavailable")

    @field_validator("action", mode="before")
    @classmethod
    def _action(cls, v: Any) -> Any:
        return None if v is None else normalize_action(v)


class KeySpec(Spec):
    key: str = Field(min_length=1, max_length=40)
    command: str = Field(min_length=1, max_length=64, description="Local name or any command's full id")
    when: When | None = None
    override: bool = False


class SettingSpec(Spec):
    name: LocalName
    title: TextLike
    default: SettingValue
    control: SettingControl | None = None
    description: TextLike | None = None
    options: list[tuple[SettingValue, TextLike]] = Field(default_factory=list)
    min: float | None = None
    max: float | None = None
    step: float | None = None
    unit: TextLike | None = Field(default=None, description="After the value: 'MB', {zh='天', en='days'}")
    applies: SettingApplies = "now"
    synonyms: list[str] = Field(default_factory=list)
    depends: str | None = None
    group: TextLike | None = None
    placeholder: TextLike | None = Field(default=None, description="Shown in an empty text field")
    default_from: LocalName | None = Field(
        default=None,
        description="An export option: start from the current value of this setting of the plugin",
    )

    @model_validator(mode="after")
    def _control(self) -> SettingSpec:
        if self.control is None:
            control: SettingControl = (
                "switch"
                if isinstance(self.default, bool)
                else "segmented"
                if 0 < len(self.options) <= 4
                else "select"
                if self.options
                else "number"
                if isinstance(self.default, int | float)
                else "text"
            )
            object.__setattr__(self, "control", control)
        if self.control == "switch" and not isinstance(self.default, bool):
            raise ValueError("a switch needs a true/false default")
        if self.control in ("number", "slider") and (
            isinstance(self.default, bool) or not isinstance(self.default, int | float)
        ):
            raise ValueError(f"a {self.control} needs a number default")
        values = [o[0] for o in self.options]
        if self.control == "multi":
            if not self.options or not isinstance(self.default, str):
                raise ValueError('multi needs options and a text default: the chosen values joined by ","')
            if any(not isinstance(v, str) or "," in v for v in values):
                raise ValueError("the options of multi are texts without commas")
            if any(x not in values for x in self.default.split(",") if x):
                raise ValueError("each value of the default must be one of the options")
        elif self.options and self.default not in values:
            raise ValueError("the default must be one of the options")
        if self.control in ("segmented", "select") and not self.options:
            raise ValueError(f"a {self.control} needs options")
        return self


class ReportSpec(Spec):
    name: LocalName
    title: TextLike
    icon: str = ""
    slot: ReportSlot = "tab"
    refresh: list[str] | None = None
    when: When = Field(default_factory=list)
    order: int = 0
    blocks: list[dict[str, Any]] | None = Field(default=None, description="A static report (no code)")

    @field_validator("refresh")
    @classmethod
    def _refresh(cls, v: list[str] | None) -> list[str] | None:
        for r in v or ():
            if r in REFRESH_WORDS:
                continue
            if r.startswith("interval:"):
                if not r[9:].isdigit() or int(r[9:]) < 5:
                    raise ValueError("interval:<seconds> needs at least 5 seconds")
                continue
            if r.startswith("state:") and valid_local_name(r[6:]):
                continue
            raise ValueError(f"unknown refresh {r!r}")
        return v

    def refresh_list(self) -> list[str]:
        if self.refresh is not None:
            return list(self.refresh)
        return ["selection"] if self.slot == "inspector" else ["open"]


class FieldSpec(Spec):
    name: LocalName
    title: TextLike
    slots: list[FieldSlot] = Field(default=["inspector"], min_length=1)
    when: When = Field(default_factory=list)
    order: int = 0
    kinds: list[Literal["file", "dir"]] = Field(
        default_factory=list, description="Only nodes of these kinds are asked (none: every kind)"
    )
    exts: list[str] = Field(
        default_factory=list,
        max_length=512,
        description='Only files with these extensions are asked ("zip", no dot; none: any)',
    )
    globs: list[str] = Field(
        default_factory=list, max_length=64, description="Only nodes whose path matches one are asked"
    )

    @field_validator("exts")
    @classmethod
    def _exts(cls, v: list[str]) -> list[str]:
        return [x.lower().lstrip("*").lstrip(".") for x in v if x.strip(" .*")]


class InsightSpec(Spec):
    name: LocalName
    globs: list[str] = Field(
        default_factory=list, max_length=64, description="Only nodes whose path matches one are asked"
    )


class PanelSpec(Spec):
    name: LocalName
    title: TextLike
    short_title: TextLike | None = Field(default=None, description="For tabs and narrow places")
    icon: str = ""
    entry: str = "web/index.html"
    slot: PanelSlot = "tab"
    height: int = Field(default=320, ge=40, le=800)
    when: When = Field(default_factory=list)
    params: dict[str, str | int | float | bool] = Field(
        default_factory=dict, description="Given to the page in sk.context().params (one page, two places)"
    )
    keep: bool = Field(default=True, description="A tab in the background keeps its page (state and all)")

    @field_validator("entry")
    @classmethod
    def _entry(cls, v: str) -> str:
        p = Path(v)
        if p.is_absolute() or ".." in p.parts or not v.startswith("web/") or not v.endswith(".html"):
            raise ValueError("entry must be an .html file inside web/")
        return v


# --------------------------------------------------------------------------- visualizations and colours


class VisualizationSpec(Spec):
    """A built-in engine (``base``, with its wire layout) sized by another measure: ``size`` names
    one of the store's (``files``: file counts), or ``code`` — a function of the plugin gives each
    file a weight (``@plugin.visualization``); folders are the sums of their contents."""

    name: LocalName
    title: TextLike
    icon: str = ""
    order: int = 50
    base: Literal["treemap", "flame", "flame_polar"] = "flame"
    size: Literal["alloc", "size", "files", "items", "code"] = "alloc"
    options: list[LocalName] = Field(default_factory=list, description="Own settings passed to the code")
    compare: bool = False
    unit: Literal["bytes", "count", "weight"] | None = Field(
        default=None,
        description="What the areas count (default: bytes for alloc/size, weight for code, else count)",
    )
    default_color: str | None = Field(
        default=None,
        max_length=64,
        description="The colour mode a view starts with on it: class flat time branch, or a colour "
        "mode of this plugin (its name) or of another (its full id)",
    )

    def size_unit(self) -> Literal["bytes", "count", "weight"]:
        if self.unit is not None:
            return self.unit
        if self.size == "code":
            return "weight"
        return "bytes" if self.size in ("alloc", "size") else "count"


Palette = list[str | ThemedColor]
_BAND = re.compile(r"^\s*(\d+(?:\.\d+)?)\s*([a-z]+)\s*$")


def band_value(text: str, what: Literal["time", "size"]) -> int | None:
    """``"30d"`` -> seconds, ``"100mb"`` -> bytes (the filter's units); None when it is not one."""
    from spaceknife_protocol.filter_syntax import SIZE_UNITS, TIME_UNITS

    m = _BAND.match(text.lower())
    if m is None:
        return None
    units = TIME_UNITS if what == "time" else SIZE_UNITS
    factor = units.get(m.group(2))
    return None if factor is None else int(float(m.group(1)) * factor)


class ColorRule(Spec):
    glob: str = Field(min_length=1, max_length=256)
    index: int = Field(ge=0, lt=MAX_PALETTE)


class ColorModeSpec(Spec):
    name: LocalName
    title: TextLike
    palette: Palette = Field(min_length=1, max_length=MAX_PALETTE)
    legend: list[TextLike] = Field(default_factory=list, max_length=MAX_PALETTE)
    by: Literal["ext", "class", "age", "size", "depth", "name", "set", "insight"] | None = None
    map: dict[str, int] = Field(default_factory=dict)
    field: Literal["mtime", "ctime", "atime"] = "mtime"
    bands: list[str] = Field(default_factory=list, max_length=MAX_PALETTE)
    rules: list[ColorRule] = Field(default_factory=list)
    set: str | None = None
    folders: Literal["ring", "palette"] = "ring"
    layouts: list[Literal["drawlist.v1", "flame.v1", "polar.v1"]] = Field(default_factory=list)
    bands_setting: LocalName | None = Field(
        default=None,
        description="A text setting of the plugin with the bands ('7d, 30d, 1y'): they follow it",
    )

    @model_validator(mode="after")
    def _indices(self) -> ColorModeSpec:
        n = len(self.palette)
        for key, idx in self.map.items():
            if not 0 <= idx < n:
                raise ValueError(f"map[{key!r}] = {idx}: no such palette entry (0–{n - 1})")
        for rule in self.rules:
            if rule.index >= n:
                raise ValueError(f"rule {rule.glob!r}: no palette entry {rule.index}")
        if self.legend and len(self.legend) != n:
            raise ValueError("legend needs one text per palette entry")
        if self.by in ("age", "size"):
            if not self.bands:
                raise ValueError(f"by = {self.by!r} needs bands, e.g. ['7d', '30d', '1y'] or ['1mb', '1gb']")
            if len(self.bands) + 2 > n:
                raise ValueError("bands: k thresholds need k + 2 palette entries (0 is neutral)")
            for b in self.bands:
                if band_value(b, "time" if self.by == "age" else "size") is None:
                    raise ValueError(
                        f"band {b!r}: a number with a {'time' if self.by == 'age' else 'size'} unit"
                    )
        if self.by == "depth" and n < 2:
            raise ValueError("by = 'depth' needs at least two palette entries")
        if self.by == "set" and not self.set:
            raise ValueError("by = 'set' needs set = '<node set>'")
        return self


# --------------------------------------------------------------------------- data


class ExportFormatSpec(Spec):
    name: LocalName
    title: TextLike
    description: TextLike | None = None
    extension: str = Field(pattern=r"^\.?[A-Za-z0-9]{1,16}$")
    media_type: str = "text/plain"
    binary: bool = False
    bom: bool = False
    options: list[SettingSpec] = Field(
        default_factory=list, max_length=12, description="Fields of the export dialog (as settings)"
    )

    @field_validator("extension")
    @classmethod
    def _ext(cls, v: str) -> str:
        return v.lstrip(".")  # one form everywhere: without the dot


class ExportTemplateSpec(Spec):
    name: LocalName
    file: str = Field(description="A .sktpl file in templates/")
    title: TextLike
    description: TextLike | None = None
    file_name: str = ""
    spacesniffer: bool = Field(
        default=False, description="It uses only SpaceSniffer's template language (works there too)"
    )

    @field_validator("file")
    @classmethod
    def _file(cls, v: str) -> str:
        p = Path(v)
        if p.is_absolute() or ".." in p.parts or not v.startswith("templates/"):
            raise ValueError("file must be inside templates/")
        return v


class InsightRulesSpec(Spec):
    file: str = Field(description="A rules file in insights/ (every insights/*.toml is loaded anyway)")


class FileClassSpec(Spec):
    name: LocalName
    title: TextLike
    aliases: list[str] = Field(default_factory=list)
    exts: list[str] = Field(default_factory=list, max_length=512)
    color: str | ThemedColor | None = None

    @field_validator("exts")
    @classmethod
    def _exts(cls, v: list[str]) -> list[str]:
        out = []
        for e in v:
            e = e.strip().lstrip(".").lower()
            if not e or len(e) > 32 or "/" in e or "\\" in e:
                raise ValueError(f"bad extension {e!r}")
            out.append(e)
        return out


class FolderRuleSpec(Spec):
    glob: str = Field(min_length=1, max_length=512, description="'**/' any depth; {a,b} either")
    cls: str = Field(alias="class", min_length=1, max_length=64)
    mode: Literal["swallow", "fallback"] = "swallow"
    name: LocalName | None = Field(default=None, description="Default: made from the glob")
    priority: int = Field(default=0, ge=-100, le=100, description="Higher first; then declaration order")
    exclude: list[str] = Field(default_factory=list, max_length=32, description="Globs it does not apply to")


class NodeSetSpec(Spec):
    name: LocalName
    title: TextLike
    aliases: list[str] = Field(default_factory=list)


class FilterPresetSpec(Spec):
    name: LocalName
    title: TextLike
    expr: str = Field(min_length=1, max_length=4096)


# --------------------------------------------------------------------------- operations, sources, history


class CleanupSpec(Spec):
    name: LocalName
    title: TextLike
    description: TextLike | None = None
    icon: str = "basket"
    when: When = Field(default_factory=list)


class ScanSourceSpec(Spec):
    name: LocalName
    title: TextLike
    icon: str = "drive"
    pick: Literal["folder", "file", "text"] = "text"
    extensions: list[str] = Field(default_factory=list)
    placeholder: TextLike | None = None
    on_disk: bool = False
    ops: bool = False
    watch: bool = False
    size: Literal["alloc", "size"] = Field(
        default="alloc",
        description="What the tree shows as a file's size: alloc (as listed) or size (an archive's "
        "unpacked size, for one)",
    )

    @model_validator(mode="after")
    def _ops(self) -> ScanSourceSpec:
        if self.ops and not self.on_disk:
            raise ValueError("ops (deleting) needs on_disk")
        return self


class WatcherSpec(Spec):
    name: LocalName
    source: LocalName


class AnnotatorSpec(Spec):
    name: LocalName


class HookSpec(Spec):
    name: LocalName
    event: Literal["start", "stop", "scan_done", "scan_closed", "settings", "lang"]


class IconSpec(Spec):
    name: LocalName
    file: str


class MethodDecl(Spec):
    """A plugin method as the host routes it (filled from the code)."""

    name: LocalName
    kind: str = "method"
    timeout: float = Field(default=30.0, gt=0, le=600)
    latest: bool = False
    public: bool = True
    params: dict[str, Any] | None = Field(default=None, description="JSON Schema of the params")
    result: dict[str, Any] | None = Field(default=None, description="JSON Schema of the result")
    doc: str = Field(default="", max_length=400, description="The first paragraph of its docstring")


class EventDecl(Spec):
    name: LocalName
    schema_: dict[str, Any] = Field(default_factory=dict, alias="schema")


class StateDecl(Spec):
    name: LocalName
    schema_: dict[str, Any] = Field(default_factory=dict, alias="schema")
    default: dict[str, Any] = Field(default_factory=dict)


class StreamDecl(Spec):
    name: LocalName
    params: dict[str, Any] | None = None


# kind -> (spec model, TOML table name or None for code-only kinds)
KINDS: dict[str, tuple[type[Spec], str | None]] = {
    "command": (CommandSpec, "commands"),
    "keybinding": (KeySpec, "keys"),
    "setting": (SettingSpec, "settings"),
    "report": (ReportSpec, "reports"),
    "node_field": (FieldSpec, None),
    "insight": (InsightSpec, None),
    "panel": (PanelSpec, "panels"),
    "visualization": (VisualizationSpec, "visualizations"),
    "color_mode": (ColorModeSpec, "color_modes"),
    "export_format": (ExportFormatSpec, None),
    "export_template": (ExportTemplateSpec, "export_templates"),
    "insight_rules": (InsightRulesSpec, "insight_rules"),
    "file_class": (FileClassSpec, "classes"),
    "folder_rule": (FolderRuleSpec, "folder_rules"),
    "node_set": (NodeSetSpec, None),
    "filter_preset": (FilterPresetSpec, "filters"),
    "cleanup": (CleanupSpec, None),
    "scan_source": (ScanSourceSpec, None),
    "watcher": (WatcherSpec, None),
    "change_annotator": (AnnotatorSpec, None),
    "hook": (HookSpec, None),
    "icon": (IconSpec, "icons"),
}
# kinds whose contributions run plugin code (a code entry is needed)
CODE_KINDS = frozenset(
    {
        "node_field",
        "insight",
        "export_format",
        "node_set",
        "cleanup",
        "scan_source",
        "watcher",
        "change_annotator",
        "hook",
    }
)
TABLES = {table: kind for kind, (_, table) in KINDS.items() if table is not None}


def spec_key(kind: str, spec: Spec) -> str:
    """The name a contribution is known by inside its plugin (unique per kind)."""
    if isinstance(spec, KeySpec):
        return spec.key
    if isinstance(spec, FolderRuleSpec):
        return spec.name or spec.glob
    if isinstance(spec, InsightRulesSpec):
        return spec.file
    return str(getattr(spec, "name", ""))


# --------------------------------------------------------------------------- the manifest


class Limits(Spec):
    memory_mb: int = Field(default=1024, ge=64, le=4096)
    timeout_s: int = Field(default=60, ge=1, le=600)


class Requires(Spec):
    host: str = Field(default="", description="Host version range, e.g. '>=0.2'")


class Manifest(Spec):
    id: str = Field(min_length=3, max_length=PLUGIN_ID_MAX, pattern=PLUGIN_ID_PATTERN)
    version: str
    api: int = PLUGIN_API
    name: TextLike
    description: TextLike | None = None
    author: str = ""
    icon: str = ""
    entry: str | None = Field(default=None, description="plugin.py, or a package folder; none: declarative")
    permissions: list[Permission] = Field(default_factory=list)
    runtime: Literal["process", "inprocess"] = "process"
    activation: Literal["lazy", "startup"] = "lazy"
    net_hosts: list[str] = Field(default_factory=list)
    exec: list[str] = Field(default_factory=list)
    requires: Requires = Field(default_factory=Requires)
    homepage: str = ""
    license: str = ""
    limits: Limits = Field(default_factory=Limits)
    main: str | None = Field(default=None, description="Reserved: Electron main-process plugins")
    commands: list[CommandSpec] = Field(default_factory=list)
    keys: list[KeySpec] = Field(default_factory=list)
    settings: list[SettingSpec] = Field(default_factory=list)
    reports: list[ReportSpec] = Field(default_factory=list)
    panels: list[PanelSpec] = Field(default_factory=list)
    visualizations: list[VisualizationSpec] = Field(default_factory=list)
    color_modes: list[ColorModeSpec] = Field(default_factory=list)
    export_templates: list[ExportTemplateSpec] = Field(default_factory=list)
    insight_rules: list[InsightRulesSpec] = Field(default_factory=list)
    classes: list[FileClassSpec] = Field(default_factory=list)
    folder_rules: list[FolderRuleSpec] = Field(default_factory=list)
    filters: list[FilterPresetSpec] = Field(default_factory=list)
    icons: list[IconSpec] = Field(default_factory=list)

    @field_validator("version")
    @classmethod
    def _semver(cls, v: str) -> str:
        if not SEMVER.fullmatch(v):
            raise ValueError("version must be semver, e.g. 0.1.0")
        return v

    @field_validator("permissions")
    @classmethod
    def _perms(cls, v: list[Permission]) -> list[Permission]:
        return [p for p in PERMISSIONS if p in v]  # canonical order, no duplicates

    @field_validator("entry")
    @classmethod
    def _entry(cls, v: str | None) -> str | None:
        if v is None:
            return v
        p = Path(v)
        if p.is_absolute() or ".." in p.parts or not v:
            raise ValueError("entry must be a file or folder inside the plugin folder")
        return v

    @model_validator(mode="after")
    def _consistent(self) -> Manifest:
        builtin = self.id.startswith("builtin.")
        if not valid_plugin_id(self.id, builtin=builtin):
            first = self.id.split(".", 1)[0]
            raise ValueError(f"the plugin id may not start with {first!r} (reserved)")
        if self.main is not None:
            raise ValueError("Electron main-process plugins (main = …) are not supported by this version")
        if "net" in self.permissions and not self.net_hosts:
            raise ValueError("the net permission needs net_hosts (the hosts it connects to, or '*')")
        if self.net_hosts and "net" not in self.permissions:
            raise ValueError("net_hosts without the net permission")
        if "exec" in self.permissions and not self.exec:
            raise ValueError("the exec permission needs exec (the programs it runs)")
        if self.exec and "exec" not in self.permissions:
            raise ValueError("exec programs without the exec permission")
        return self

    @property
    def builtin(self) -> bool:
        return self.id.startswith("builtin.")

    def declared(self) -> list[tuple[str, Spec]]:
        """Every contribution written in the TOML, as (kind, spec)."""
        out: list[tuple[str, Spec]] = []
        for table, kind in TABLES.items():
            for spec in getattr(self, table):
                out.append((kind, spec))
        return out


def _closest(word: str, options: list[str]) -> str:
    hit = difflib.get_close_matches(word, options, n=1, cutoff=0.6)
    return hit[0] if hit else ""


def _fields_of(model: type[BaseModel]) -> list[str]:
    out = []
    for name, info in model.model_fields.items():
        out.append(info.alias or name)
    return out


def _line_of(text: str, loc: tuple[Any, ...]) -> int:
    """Best effort: the line of the first key of ``loc`` in the TOML text."""
    keys = [str(k) for k in loc if isinstance(k, str)]
    if not keys:
        return 0
    want = keys[-1]
    table = keys[0] if len(keys) > 1 else None
    index = next((k for k in loc if isinstance(k, int)), None)
    lines = text.splitlines()
    start = 0
    if table is not None and index is not None:
        seen = -1
        for i, line in enumerate(lines):
            if line.strip() == f"[[{table}]]":
                seen += 1
                if seen == index:
                    start = i
                    break
    for i in range(start, len(lines)):
        if re.match(rf"\s*{re.escape(want)}\s*=", lines[i]):
            return i + 1
    return start + 1 if start else 0


def validation_issues(
    exc: ValidationError, *, file: str, text: str = "", model: type[BaseModel] = Manifest
) -> list[Issue]:
    """pydantic's errors as plugin issues, with the closest valid field name for unknown ones."""
    issues: list[Issue] = []
    for err in exc.errors(include_url=False):
        loc = tuple(err.get("loc", ()))
        where = ".".join(str(p) for p in loc)
        if err["type"] == "extra_forbidden" and loc:
            bad = str(loc[-1])
            sub = _owner_model(model, loc[:-1])
            closest = _closest(bad, _fields_of(sub)) if sub is not None else ""
            issues.append(
                Issue(
                    "unknown_field",
                    f"unknown field {where!r}",
                    file=file,
                    line=_line_of(text, loc),
                    fix=f"did you mean {closest!r}?"
                    if closest
                    else "see `spaceknife plugin schema --manifest`",
                )
            )
            continue
        issues.append(
            Issue(
                "invalid",
                f"{where}: {err['msg']}" if where else err["msg"],
                file=file,
                line=_line_of(text, loc),
            )
        )
    return issues


def _owner_model(model: type[BaseModel], loc: tuple[Any, ...]) -> type[BaseModel] | None:
    current: type[BaseModel] = model
    for part in loc:
        if isinstance(part, int):
            continue
        info = current.model_fields.get(str(part))
        if info is None:
            info = next((f for f in current.model_fields.values() if f.alias == part), None)
        if info is None:
            return None
        ann = info.annotation
        inner = _model_in(ann)
        if inner is None:
            return None
        current = inner
    return current


def _model_in(ann: Any) -> type[BaseModel] | None:
    if isinstance(ann, type) and issubclass(ann, BaseModel):
        return ann
    for arg in getattr(ann, "__args__", ()) or ():
        found = _model_in(arg)
        if found is not None:
            return found
    return None


def load_manifest(folder: Path) -> Manifest:
    """Read and validate ``<folder>/plugin.toml``; ManifestError lists every problem."""
    path = folder / MANIFEST
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise ManifestError([Issue("no_manifest", f"{MANIFEST} is missing", file=MANIFEST)]) from None
    except (OSError, UnicodeDecodeError) as exc:
        raise ManifestError([Issue("unreadable", f"{MANIFEST}: {exc}", file=MANIFEST)]) from None
    return parse_manifest(text)


def parse_manifest(text: str) -> Manifest:
    try:
        raw = tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        raise ManifestError([Issue("toml", str(exc), file=MANIFEST)]) from None
    try:
        return Manifest.model_validate(raw)
    except ValidationError as exc:
        raise ManifestError(validation_issues(exc, file=MANIFEST, text=text)) from None


def text_of(value: Text | str | None) -> Text | None:
    if value is None or isinstance(value, Text):
        return value
    return Text(zh=value)


__all__ = [
    "BASE_VISUALS",
    "CODE_KINDS",
    "COLOR_BY",
    "HOOKS",
    "KINDS",
    "MANIFEST",
    "MENUS",
    "PLUGIN_API",
    "SIZE_METRICS",
    "TABLES",
    "WHEN_WORDS",
    "AnnotatorSpec",
    "CleanupSpec",
    "ColorModeSpec",
    "ColorRule",
    "CommandSpec",
    "EventDecl",
    "ExportFormatSpec",
    "ExportTemplateSpec",
    "FieldSpec",
    "FileClassSpec",
    "FilterPresetSpec",
    "FolderRuleSpec",
    "HookSpec",
    "IconSpec",
    "InsightRulesSpec",
    "InsightSpec",
    "KeySpec",
    "Limits",
    "Manifest",
    "MethodDecl",
    "NodeSetSpec",
    "PanelSpec",
    "ReportSpec",
    "ScanSourceSpec",
    "SettingSpec",
    "Spec",
    "StateDecl",
    "StatusSpec",
    "StreamDecl",
    "VisualizationSpec",
    "WatcherSpec",
    "load_manifest",
    "parse_manifest",
    "spec_key",
    "validation_issues",
]

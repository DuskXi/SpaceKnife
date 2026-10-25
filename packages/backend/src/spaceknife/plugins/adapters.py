"""Turning a plugin's description into contributions: each kind's host side (``Contribution.impl``).

The host validates every declaration again with the SDK's models (a plugin process is not trusted
to have done it), resolves local names to full ids and wire names, and builds what the subsystem
needs: a palette command with its conditions, a settings spec, a visualization, a colour painter, an
export format that streams from the plugin, a compiled rule file …
"""

from __future__ import annotations

import dataclasses
import json
import re
from collections.abc import Callable
from typing import TYPE_CHECKING, Any

import numpy as np
from pydantic import ValidationError

from spaceknife_protocol import CAP_COLORS
from spaceknife_protocol.filter import FilterClassInfo
from spaceknife_protocol.filter_syntax import parse
from spaceknife_protocol.text import Text
from spaceknife_sdk.actions import ActionError, resolve_action
from spaceknife_sdk.errors import Issue
from spaceknife_sdk.manifest import (
    CODE_KINDS,
    KINDS,
    ColorModeSpec,
    CommandSpec,
    ExportFormatSpec,
    ExportTemplateSpec,
    FileClassSpec,
    FilterPresetSpec,
    FolderRuleSpec,
    IconSpec,
    InsightRulesSpec,
    KeySpec,
    PanelSpec,
    ReportSpec,
    SettingSpec,
    Spec,
    VisualizationSpec,
    spec_key,
)
from spaceknife_sdk.plugin import Description, Plugin

from ..extensions.colors import RulePainter
from ..extensions.registry import Contribution
from ..i18n import _, tr
from ..i18n import language as i18n_language
from ..layout import registry as visuals
from ..layout.api import ENGINE_API, ColumnSizes, Visualization
from ..palette import commands as pal
from ..settings import Spec as SettingsSpec
from .icons import sanitize_svg

if TYPE_CHECKING:
    from .host import PluginHost
    from .store import PluginRecord

BASE_ENGINES = {"treemap": "treemap", "flame": "flame", "flame_polar": "flame_polar"}


class AdapterError(ValueError):
    def __init__(self, issues: list[Issue]) -> None:
        self.issues = issues
        super().__init__("; ".join(i.text() for i in issues))


def specs_of(desc: Description) -> tuple[list[tuple[str, Spec]], list[Issue]]:
    """The description's contributions, validated with the SDK's models."""
    out: list[tuple[str, Spec]] = []
    issues: list[Issue] = []
    for item in desc.contributions:
        kind = str(item.get("kind", ""))
        entry = KINDS.get(kind)
        if entry is None:
            issues.append(Issue("unknown_kind", f"unknown contribution kind {kind!r}"))
            continue
        try:
            out.append((kind, entry[0].model_validate(item.get("spec") or {})))
        except ValidationError as exc:
            issues.append(Issue("invalid", f"{kind}: {exc.errors(include_url=False)[0]['msg']}"))
    handlers = {(k, n) for k, n in desc.handlers}
    code_kind = {
        "node_field": "field",
        "insight": "insight",
        "export_format": "export",
        "node_set": "set",
        "cleanup": "cleanup",
        "scan_source": "source",
        "watcher": "watch",
        "change_annotator": "annotate",
        "hook": "hook",
        "background": "task",
    }
    for kind, spec in out:
        if kind in CODE_KINDS and (code_kind[kind], spec_key(kind, spec)) not in handlers:
            issues.append(Issue("no_code", f"{kind} {spec_key(kind, spec)!r} has no function"))
        if (
            kind == "report"
            and isinstance(spec, ReportSpec)
            and spec.blocks is None
            and ("report", spec.name) not in handlers
        ):
            issues.append(Issue("no_code", f"report {spec.name!r} has neither blocks nor a function"))
        if (
            kind == "visualization"
            and isinstance(spec, VisualizationSpec)
            and spec.size == "code"
            and ("visual", spec.name) not in handlers
        ):
            issues.append(Issue("no_code", f"visualization {spec.name!r} has no function"))
        if (
            kind == "visualization"
            and isinstance(spec, VisualizationSpec)
            and spec.base == "shapes"
            and ("layout", spec.name) not in handlers
        ):
            issues.append(
                Issue("no_code", f"visualization {spec.name!r} has no layout function (@plugin.layout)")
            )
        if (
            kind == "color_mode"
            and isinstance(spec, ColorModeSpec)
            and spec.by is None
            and ("color", spec.name) not in handlers
        ):
            issues.append(Issue("no_code", f"colour mode {spec.name!r} has neither by = … nor a function"))
    return out, issues


# --------------------------------------------------------------------------- the kinds


def _needs(host: PluginHost, rec: PluginRecord, when: list[str]) -> tuple[pal.Need, ...]:
    """``when`` conditions as palette needs (each returns the reason it is not met)."""
    out: list[pal.Need] = []
    table: dict[str, pal.Need] = {
        "scan": pal._scan,
        "done": pal._done,
        "scanning": pal._running,
        "view": pal._view,
        "selection": pal._selection,
        "scan_view": pal._scan_view,
        "compare": lambda c: (
            pal._view(c)
            or (None if c.view is not None and c.view.compare is not None else _("只在对比中可用"))
        ),
        "file": lambda c: pal._selection(c) or (None if _selected_kind(c) == "file" else _("只用于文件")),
        "dir": lambda c: pal._selection(c) or (None if _selected_kind(c) == "dir" else _("只用于文件夹")),
        "on_disk": lambda c: _("这次扫描的条目不在磁盘上") if c.scan_source or _selected_virtual(c) else None,
    }
    for w in when:
        if w in table:
            out.append(table[w])
        elif w.startswith("setting:"):
            key = w[8:]
            full = key if "." in key else f"plugin.{rec.id}.{key}"

            def setting_on(c: pal.Context, full: str = full) -> str | None:
                st = c.settings
                try:
                    return None if st is not None and bool(st.get_value(full)) else _("需要先打开相应的设置")
                except KeyError:
                    return _("需要先打开相应的设置")

            out.append(setting_on)
        elif w.startswith("cap:"):

            def has_cap(c: pal.Context, cap: str = w[4:]) -> str | None:
                return None if cap in c.caps else _("当前界面不支持")

            out.append(has_cap)
        elif w.startswith("plugin:"):

            def has_plugin(c: pal.Context, other: str = w[7:]) -> str | None:
                return None if other in c.plugins else tr("需要启用插件 {id}", id=other)

            out.append(has_plugin)
        elif w.startswith("ext:"):
            exts = {e.strip().lower().lstrip(".") for e in w[4:].split(",") if e.strip()}

            def has_ext(c: pal.Context, exts: set[str] = exts) -> str | None:
                why = pal._selection(c)
                if why is not None:
                    return why
                node, view = c.selected_node, c.view
                if node is None or view is None:
                    return _("没有选中项")
                name = view.store.name(node)
                ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
                return None if _selected_kind(c) == "file" and ext in exts else _("只用于这几种文件")

            out.append(has_ext)
        elif w.startswith("source:"):
            src = w[7:]

            def from_source(c: pal.Context, s: str = src if "." in src else f"{rec.id}.{src}") -> str | None:
                return None if c.scan_source == s else _("当前扫描不是来自这个来源")

            out.append(from_source)
    return tuple(out)


def _selected_kind(c: pal.Context) -> str:
    """The selected node: "file", "dir" or "" (none, or not a real node)."""
    node = c.selected_node
    if node is None or c.view is None:
        return ""
    from ..store.nodes import Kind

    kind = int(c.view.store.cols["kind"][node])
    return "dir" if kind == Kind.DIR else "file" if kind == Kind.FILE else ""


def _selected_virtual(c: pal.Context) -> bool:
    """The selected node (or one above it) is not on this disk (MFT pseudo folders, a source's)."""
    node = c.selected_node
    if node is None or c.view is None:
        return False
    from ..store.nodes import Flag

    store = c.view.store
    flags = store.cols["flags"]
    return any(int(flags[n]) & int(Flag.VIRTUAL) for n in (node, *store.ancestors(node)))


def _pinyin(rec: PluginRecord) -> dict[str, list[str]]:
    path = rec.folder / "generated" / "pinyin.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        return {str(k): [str(x) for x in v] for k, v in data.items()}
    except (OSError, ValueError, AttributeError):
        return {}


def _command(
    host: PluginHost, rec: PluginRecord, spec: CommandSpec, pinyin: dict[str, list[str]]
) -> pal.Command:
    action = resolve_action(spec.action, rec.id) if spec.action is not None else None
    title: Text = spec.title
    status: Callable[[pal.Context], str] | None = None
    if spec.status is not None:
        state_name, text = spec.status.state, spec.status.text

        def status_fn(c: pal.Context, state_name: str = state_name, text: Text = text) -> str:
            from spaceknife_protocol.text import UNIT_BASE

            values = rec.states.get(state_name, {})
            st = c.settings
            token = UNIT_BASE.set(st.unit_base() if st is not None and hasattr(st, "unit_base") else 1024)
            try:
                return text.format(**values).get(i18n_language())
            except (KeyError, IndexError, ValueError, TypeError):
                return ""
            finally:
                UNIT_BASE.reset(token)

        status = status_fn
    needs = list(_needs(host, rec, spec.when))
    if spec.check is not None:
        method = spec.check

        def check(c: pal.Context, method: str = method) -> str | None:
            return host.check_command(rec.id, method, c)

        needs.append(check)
    syl = pinyin.get(title.zh)
    return pal.Command(
        id=f"{rec.id}.{spec.name}",
        name=title.zh,
        icon=spec.icon or (rec.manifest.icon if rec.manifest else "") or "plugin",
        keys=tuple(spec.keys),
        aliases=tuple(spec.aliases),
        needs=tuple(needs),
        status=status,
        typed_only=spec.typed_only,
        action=action,
        menus=tuple(spec.menus),
        owner=rec.id,
        text=title,
        pinyin=tuple(syl) if syl is not None and len(syl) == len(title.zh) else None,
    )


def _setting(rec: PluginRecord, spec: SettingSpec, *, key: str = "") -> SettingsSpec:
    """A plugin's setting as the settings service has it; ``key``: a field of a list's entries."""
    kind: type = (
        list
        if spec.control == "list"
        else bool
        if isinstance(spec.default, bool)
        else int
        if isinstance(spec.default, int)
        else float
        if isinstance(spec.default, float)
        else str
    )
    texts: list[tuple[str, Text]] = [("name", spec.title)]
    if spec.description is not None:
        texts.append(("description", spec.description))
    group = spec.group or (rec.manifest.name if rec.manifest is not None else Text(zh=rec.id))
    texts.append(("group", group))
    for i, (_v, label) in enumerate(spec.options):
        texts.append((f"option:{i}", label))
    if spec.placeholder is not None:
        texts.append(("placeholder", spec.placeholder))
    if spec.unit is not None:
        texts.append(("unit", spec.unit))
    depends = spec.depends
    if depends and "." not in depends.split("=", 1)[0]:
        depends = f"plugin.{rec.id}.{depends}"
    return SettingsSpec(
        key=key or f"plugin.{rec.id}.{spec.name}",
        category="plugins",
        group=group.zh,
        name=spec.title.zh,
        description=spec.description.zh if spec.description is not None else "",
        control=str(spec.control),
        default=spec.default,
        options=tuple((v, label.zh) for v, label in spec.options),
        min=spec.min,
        max=spec.max,
        step=spec.step,
        unit=spec.unit.zh if spec.unit is not None else "",
        applies=spec.applies,
        synonyms=tuple(spec.synonyms),
        depends=depends,
        kind=kind,
        plugin=rec.id,
        texts=tuple(texts),
        max_length=spec.text_limit if spec.control in ("text", "textarea") else None,
        fields=tuple(_setting(rec, f, key=f.name) for f in spec.fields),
        max_items=spec.items_limit if spec.control == "list" else None,
    )


def _visualization(host: PluginHost, rec: PluginRecord, spec: VisualizationSpec) -> Visualization:
    if spec.base == "shapes":
        return _shapes_visualization(host, rec, spec)
    base = visuals.get(BASE_ENGINES[spec.base])
    vid = f"{rec.id}.{spec.name}"
    sizes: Callable[[Any], Any] | None
    if spec.size == "alloc":
        sizes = None
    elif spec.size == "code":
        sizes = host.remote_sizes(rec.id, spec.name, [(o, f"plugin.{rec.id}.{o}") for o in spec.options])
    else:
        metric = spec.size

        def sizes(basis: Any, metric: str = metric) -> ColumnSizes:
            return ColumnSizes(basis, metric)

    return Visualization(
        id=vid,
        api=ENGINE_API,
        layout=base.layout,
        capability=base.capability,
        title_id=spec.title.zh,
        icon=spec.icon or base.icon,
        order=100 + spec.order,
        engine=base.engine,
        default_color=_color_id(rec, spec.default_color) or base.default_color,
        compare=spec.compare and spec.size == "alloc",
        free_space=base.free_space and spec.size == "alloc",
        options=tuple((o, f"plugin.{rec.id}.{o}") for o in spec.options)
        + (base.options if spec.base == "flame" else ()),
        aliases=(spec.title.zh, *([spec.title.en] if spec.title.en else [])),
        size_metric=spec.size,
        size_unit=spec.size_unit(),
        sizes=sizes,
        plugin=rec.id,
        text=spec.title,
        needs=_needs(host, rec, spec.when),
        when=tuple(spec.when),
    )


def _shapes_visualization(host: PluginHost, rec: PluginRecord, spec: VisualizationSpec) -> Visualization:
    """A visualization whose blocks the plugin places itself (``shapes.v1``, layout/shapes.py)."""
    from spaceknife_protocol import CAP_LAYOUT_SHAPES
    from spaceknife_protocol import view as pv

    from ..layout.shapes import ShapesEngine

    vid = f"{rec.id}.{spec.name}"
    options = [(o, f"plugin.{rec.id}.{o}") for o in spec.options]
    sizes: Callable[[Any], Any] | None = None
    if spec.size != "alloc":

        def sizes(basis: Any, metric: str = spec.size) -> ColumnSizes:
            return ColumnSizes(basis, metric)

    def engine(basis: Any) -> ShapesEngine:
        place, key = host.remote_layout(rec.id, spec.name, options, basis.store)
        return ShapesEngine(
            basis, vid, place, fill=spec.fill, max_rows=spec.max_rows, nested=spec.nested, key=key
        )

    return Visualization(
        id=vid,
        api=ENGINE_API,
        layout=pv.SHAPES_LAYOUT,
        capability=CAP_LAYOUT_SHAPES,
        title_id=spec.title.zh,
        icon=spec.icon or "layers",
        order=100 + spec.order,
        engine=engine,
        default_color=_color_id(rec, spec.default_color) or "class",
        compare=spec.compare and spec.size == "alloc",
        free_space=spec.size == "alloc",
        options=tuple(options),
        aliases=(spec.title.zh, *([spec.title.en] if spec.title.en else [])),
        size_metric=spec.size,
        size_unit=spec.size_unit(),
        sizes=sizes,
        plugin=rec.id,
        text=spec.title,
        needs=_needs(host, rec, spec.when),
        when=tuple(spec.when),
    )


def _color_id(rec: PluginRecord, name: str | None) -> str | None:
    """A colour mode a contribution names: a built-in one, this plugin's by name, any by full id."""
    if not name:
        return None
    if name in ("class", "flat", "time", "branch") or "." in name:
        return name
    return f"{rec.id}.{name}"


def _export_template(rec: PluginRecord, spec: ExportTemplateSpec) -> Any:
    from ..export import parse_template

    path = rec.folder / spec.file
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise AdapterError([Issue("template", f"{spec.file}: {exc}", file=spec.file)]) from exc
    tpl = parse_template(source, spec.name)
    errors = [i for i in tpl.issues if i.severity == "error"]
    if errors:
        raise AdapterError(
            [
                Issue("template", f"{spec.file}:{e.line}: {e.message}", file=spec.file, line=e.line)
                for e in errors
            ]
        )
    if spec.file_name:
        tpl = dataclasses.replace(tpl, file_name=spec.file_name)
    return tpl


def _hex(v: str) -> str:
    return v.upper() if re.fullmatch(r"#[0-9A-Fa-f]{6}", v) else ""


def _class(rec: PluginRecord, spec: FileClassSpec) -> FilterClassInfo:
    """A plugin's class: its name in both languages (``name`` is the Chinese one, the English one is
    shown in English), its colour for the app's theme (dark; ``color_light`` for a light one)."""
    color = light = ""
    if isinstance(spec.color, str):
        color = light = _hex(spec.color)
    elif spec.color is not None:
        color, light = _hex(spec.color.dark), _hex(spec.color.light)
    aliases = list(spec.aliases)
    if spec.title.en and spec.title.en not in aliases:
        aliases.append(spec.title.en)
    return FilterClassInfo(
        id=f"{rec.id}.{spec.name}",
        name=spec.title.zh,
        aliases=aliases,
        extensions=list(spec.exts),
        color=color,
        name_en=spec.title.en,
        color_light=light,
    )


def build(
    host: PluginHost | None, rec: PluginRecord, desc: Description, *, local: Plugin | None = None
) -> list[Contribution]:
    """Every contribution of ``rec``; AdapterError lists what is wrong (nothing is registered then).

    ``local``: the plugin's own declarations, loaded in this process (the plugins shipped with the
    app, :mod:`.builtins`): its code contributions call its functions directly, without a host."""
    specs, issues = specs_of(desc)
    pid = rec.id
    out: list[Contribution] = []
    pinyin = _pinyin(rec)
    seen: set[tuple[str, str]] = set()
    names = {spec_key(k, s) for k, s in specs if k == "command"}
    own_classes = [_class(rec, s) for _k, s in specs if isinstance(s, FileClassSpec)]
    rule_ids = rule_names(specs)
    for kind, spec in specs:
        key = spec_key(kind, spec)
        if (kind, key) in seen:
            issues.append(Issue("duplicate", f"{kind} {key!r} is declared twice"))
            continue
        seen.add((kind, key))
        auto = [rule_ids[id(spec)]] if isinstance(spec, FolderRuleSpec) and not spec.name else None
        try:
            out += _one(host, rec, kind, spec, pinyin, names, local, own_classes, auto)
        except AdapterError as exc:
            issues += exc.issues
        except (ActionError, ValueError, KeyError) as exc:
            issues.append(Issue("invalid", f"{kind} {key!r}: {exc}"))
    # a code plugin's rule files by convention (insights/*.toml), unless the manifest lists them
    listed = {s.file for k, s in specs if isinstance(s, InsightRulesSpec)}
    for f in sorted((rec.folder / "insights").glob("*.toml")) if (rec.folder / "insights").is_dir() else []:
        rel = f"insights/{f.name}"
        if rel in listed:
            continue
        try:
            out.append(
                Contribution(
                    "insight_rules",
                    f"{pid}.{_slug(f.stem)}",
                    pid,
                    None,
                    (_source(rec, rel), f.read_text("utf-8")),
                )
            )
        except (OSError, UnicodeDecodeError) as exc:
            issues.append(Issue("rules", f"{rel}: {exc}", file=rel))
    for f in sorted((rec.folder / "icons").glob("*.svg")) if (rec.folder / "icons").is_dir() else []:
        name = _slug(f.stem)
        if ("icon", name) in seen:
            continue
        try:
            out.append(Contribution("icon", f"{pid}.{name}", pid, None, sanitize_svg(f.read_text("utf-8"))))
        except (OSError, UnicodeDecodeError, ValueError) as exc:
            issues.append(Issue("icon", f"icons/{f.name}: {exc}", file=f"icons/{f.name}"))
    if issues:
        raise AdapterError(issues)
    return out


def rule_names(specs: list[tuple[str, Spec]]) -> dict[int, str]:
    """The names of folder rules declared without one: ``rule_`` and the glob's letters and digits
    (``**/.venv`` -> ``rule_venv``), with ``_2``, ``_3`` … when two globs give the same name."""
    taken = {s.name for _k, s in specs if isinstance(s, FolderRuleSpec) and s.name}
    out: dict[int, str] = {}
    for _k, s in specs:
        if not isinstance(s, FolderRuleSpec) or s.name:
            continue
        base = f"rule_{_slug(s.glob)}"[:21].rstrip("_")
        name, n = base, 1
        while name in taken:
            n += 1
            name = f"{base}_{n}"
        taken.add(name)
        out[id(s)] = name
    return out


def _source(rec: PluginRecord, rel: str) -> str:
    """A rule file's source name (in rule problems and logs): ``<plugin id>:<path>``; the shipped
    plugins' files by their names, as before they were plugins (``builtin.toml``)."""
    return rel.rsplit("/", 1)[-1] if rec.builtin else f"{rec.id}:{rel}"


def _local_format(rec: PluginRecord, spec: ExportFormatSpec, plugin: Plugin) -> Any:
    """A shipped plugin's export format: its function runs on the export's thread, with a tree view
    of the store (no host, no plugin process). Its key is the local name (``csv``), as before."""
    from spaceknife_sdk.models import ExportInfo

    from ..export import ExportFormat
    from .local import local_tree

    handler = plugin.handlers[("export", spec.name)]
    fn = handler.fn

    def chunks(store: Any, root: int, *, match: np.ndarray | None = None, filter_text: str = "") -> Any:
        tree = local_tree(store)
        if handler.arity >= 4:
            return fn(tree, int(root), match, ExportInfo(filter=filter_text))
        return fn(tree, int(root), match)

    return ExportFormat(
        key=spec.name,
        title_id=spec.title.zh,
        extension="." + spec.extension.lstrip("."),
        media_type=spec.media_type,
        bom=spec.bom,
        chunks=chunks,
        text=spec.title,
        description=spec.description,
        plugin=rec.id,
        shipped=True,
        options=tuple(spec.options),
    )


def _slug(text: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "_", text.lower()).strip("_")
    return (s or "x")[:24].rstrip("_") or "x"


def _one(
    host: PluginHost | None,
    rec: PluginRecord,
    kind: str,
    spec: Spec,
    pinyin: dict[str, list[str]],
    commands: set[str],
    local: Plugin | None = None,
    own_classes: list[FilterClassInfo] | None = None,
    auto_rule_names: list[str] | None = None,
) -> list[Contribution]:
    pid = rec.id
    auto_names = list(auto_rule_names or [])
    hostless = kind in (
        "export_template",
        "insight_rules",
        "file_class",
        "folder_rule",
        "icon",
        "filter_preset",
    )
    if host is None and not hostless and not (kind == "export_format" and local is not None):
        raise AdapterError([Issue("builtin_kind", f"{kind} needs the plugin host")])
    cid = (
        f"{pid}.{spec_key(kind, spec)}" if kind not in ("keybinding", "folder_rule", "insight_rules") else ""
    )
    if kind == "command":
        assert isinstance(spec, CommandSpec)
        assert host is not None
        return [Contribution("command", cid, pid, spec, _command(host, rec, spec, pinyin), spec.order)]
    if kind == "keybinding":
        assert isinstance(spec, KeySpec)
        return [
            Contribution("keybinding", f"{pid}.key_{_slug(spec.key)}_{_slug(spec.command)}", pid, spec, None)
        ]
    if kind == "setting":
        assert isinstance(spec, SettingSpec)
        return [Contribution("setting", f"{pid}.{spec.name}", pid, spec, _setting(rec, spec))]
    if kind == "visualization":
        assert isinstance(spec, VisualizationSpec)
        assert host is not None
        return [Contribution("visualization", cid, pid, spec, _visualization(host, rec, spec), spec.order)]
    if kind == "color_mode":
        assert isinstance(spec, ColorModeSpec)
        assert host is not None
        if spec.by is not None:
            bands_of = None
            if spec.bands_setting is not None:
                key = f"plugin.{pid}.{spec.bands_setting}"

                def bands_of(key: str = key) -> Any:
                    try:
                        return host.server.settings.get_value(key)
                    except KeyError:
                        return None

            painter: Any = RulePainter(spec, set_of=host.set_mask_for_view, bands_of=bands_of)
        else:
            painter = host.remote_painter(pid, spec.name, len(spec.palette))
        return [Contribution("color_mode", cid, pid, spec, painter)]
    if kind == "export_format":
        assert isinstance(spec, ExportFormatSpec)
        if local is not None:
            return [Contribution("export_format", cid, pid, spec, _local_format(rec, spec, local))]
        assert host is not None
        return [Contribution("export_format", cid, pid, spec, host.remote_format(rec, spec))]
    if kind == "export_template":
        assert isinstance(spec, ExportTemplateSpec)
        return [Contribution("export_template", cid, pid, spec, _export_template(rec, spec))]
    if kind == "insight_rules":
        assert isinstance(spec, InsightRulesSpec)
        path = rec.folder / spec.file
        try:
            text = path.read_text("utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            raise AdapterError([Issue("rules", f"{spec.file}: {exc}")]) from exc
        return [
            Contribution(
                "insight_rules", f"{pid}.{_slug(path.stem)}", pid, spec, (_source(rec, spec.file), text)
            )
        ]
    if kind == "file_class":
        assert isinstance(spec, FileClassSpec)
        return [Contribution("file_class", cid, pid, spec, _class(rec, spec))]
    if kind == "folder_rule":
        assert isinstance(spec, FolderRuleSpec)
        cls = spec.cls if "." in spec.cls or spec.cls in _builtin_classes() else f"{pid}.{spec.cls}"
        rid = f"{pid}.{spec.name}" if spec.name else f"{pid}.{auto_names.pop(0) if auto_names else 'rule'}"
        return [
            Contribution(
                "folder_rule",
                rid,
                pid,
                spec,
                (
                    spec.glob,
                    cls,
                    spec.mode == "fallback",
                    spec.priority,
                    tuple(spec.exclude),
                    tuple(spec.contains),
                    tuple(spec.beside),
                    rid,
                ),
            )
        ]
    if kind == "filter_preset":
        assert isinstance(spec, FilterPresetSpec)
        parsed = parse(spec.expr, None, preset_vocab(own_classes or []))
        if parsed.normal is None and not any(d.code == "unknown_set" for d in parsed.diagnostics):
            raise AdapterError([Issue("filter", f"filter {spec.name!r}: {spec.expr!r} does not parse")])
        if spec.name in commands:
            raise AdapterError([Issue("duplicate", f"filter {spec.name!r} has the name of a command")])
        title = Text(
            zh=pal.NAMED.raw("filter").format(name=spec.title.zh),
            en=f"Filter: {spec.title.en}" if spec.title.en else "",
        )  # i18n: ignore
        syl = pinyin.get(title.zh)
        cmd = pal.Command(
            id=cid,
            name=title.zh,
            icon="filter",
            aliases=("filter", spec.name),
            needs=(pal._view,),
            action=resolve_action({"kind": "filter", "expr": spec.expr}, pid),
            owner=pid,
            text=title,
            pinyin=tuple(syl) if syl is not None and len(syl) == len(title.zh) else None,
        )
        return [
            Contribution("filter_preset", cid, pid, spec, None),
            Contribution("command", cid, pid, None, cmd),
        ]
    if kind == "panel":
        assert isinstance(spec, PanelSpec)
        entry = rec.folder / spec.entry
        if not entry.is_file():
            raise AdapterError(
                [Issue("panel", f"panel {spec.name!r}: {spec.entry} does not exist", file=spec.entry)]
            )
        return [Contribution("panel", cid, pid, spec, None)]
    if kind == "icon":
        assert isinstance(spec, IconSpec)
        path = rec.folder / spec.file
        try:
            svg = sanitize_svg(path.read_text("utf-8"))
        except (OSError, UnicodeDecodeError, ValueError) as exc:
            raise AdapterError([Issue("icon", f"{spec.file}: {exc}", file=spec.file)]) from exc
        return [Contribution("icon", cid, pid, spec, svg)]
    # reports, fields, insights, cleanups, node sets, sources, watchers, annotators, hooks: the host
    # calls the plugin by kind and name when it needs them
    return [Contribution(kind, cid, pid, spec, (pid, spec_key(kind, spec)), getattr(spec, "order", 0))]


def preset_vocab(own_classes: list[FilterClassInfo]) -> Any:
    """The filter vocabulary a plugin's presets are parsed with: the app's, plus the classes the
    same plugin declares (they are registered together with the preset)."""
    from ..filter.vocab import platform_vocab

    vocab = platform_vocab()
    if not own_classes:
        return vocab
    known = {c.id for c in vocab.classes}
    return vocab.model_copy(
        update={"classes": [*vocab.classes, *(c for c in own_classes if c.id not in known)]}
    )


def _builtin_classes() -> set[str]:
    from spaceknife_protocol.classes import BUILTIN_CLASSES

    return {c.id for c in BUILTIN_CLASSES}


def colors_ok(caps: set[str]) -> bool:
    return CAP_COLORS in caps


def pal_frame(arr: np.ndarray, rows: int) -> np.ndarray:
    out = np.zeros(rows, dtype=np.uint8)
    m = min(rows, len(arr))
    out[:m] = arr[:m]
    return out


__all__ = ["AdapterError", "build", "specs_of"]

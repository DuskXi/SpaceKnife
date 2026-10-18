"""Settings files in and out (W7 「高级」, UI-W7-36): SpaceKnife's own export, SpaceSniffer's config.

* Export: ``{"format": "spaceknife-settings", "version": 1, "values": {...}, "classes": [...]}`` —
  the settings that differ from the defaults, and the file class table when the user edited it.
* Import reads such a file, or a ``SpaceSnifferConfig.xml`` (SpaceSniffer 2.0.5.18 writes one
  element per setting, colours as ``0x00BBGGRR`` and export templates as base64 UTF-16LE), and
  returns a *plan*: the changes it would make, what it cannot map, for the user to confirm.

SpaceSniffer mapping, reasonably (docs/filter-syntax.md 「合理兼容」): settings with a SpaceKnife
counterpart are taken over, scaled where the scales differ; its theme colours (drive, folder, file,
free, unknown) have none — SpaceKnife's palette is fixed — and are listed as skipped. File groups
become file classes with SpaceSniffer's names and extension lists; a group known to SpaceKnife keeps
its id (and its old names as filter aliases); SpaceSniffer's default primary colours give way to
the theme's, a colour the user picked there is kept. SpaceKnife classes SpaceSniffer has no group
for (文档) stay. Export templates equal to the built-in ones need nothing; custom ones are listed.
"""

from __future__ import annotations

import json
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal

from spaceknife_protocol.classes import BUILTIN_CLASSES, OTHER, localized
from spaceknife_protocol.filter import FilterClassInfo
from spaceknife_protocol.settings import SettingValue

from . import classes as class_table
from .i18n import Texts, _, language
from .settings import BY_KEY, DEFAULTS, SettingsStore, validate

FORMAT = "spaceknife-settings"
MAX_BYTES = 4 << 20

# User-facing text, translated when read (M7.5).
TEXT = Texts(
    {
        "too_big": "文件太大，不像是设置文件",
        "unreadable": "读不了 {path}：{reason}",
        "unknown_format": "不认识这个文件：既不是 SpaceKnife 导出的设置，"
        "也不是 SpaceSniffer 的 SpaceSnifferConfig.xml",
        "bad_json": "设置文件不是有效的 JSON：{reason}",
        "bad_xml": "SpaceSniffer 配置不是有效的 XML：{reason}",
        "newer": "这份设置来自更新版本的 SpaceKnife（格式 {version}），无法导入",
        "unknown_key": "未知设置 {key}（来自其他版本），已跳过",
        "invalid": "设置值不合法，已跳过：{key} = {value}",
        "locked": "当前版本不可修改，已跳过：{name}",
        "ss_colors": "SpaceSniffer 的配色（磁盘 / 文件夹 / 文件 / 可用 / 未知空间颜色）："
        "SpaceKnife 的树图配色固定，不导入",
        "ss_refresh": "刷新间隔 refreshDelayMs：SpaceKnife 自动调节刷新，不需要",
        "ss_zoom": "缩放时长 zoomTimeMs：单位与 SpaceKnife 不同，不导入（可在「树图 › 缩放动画时长」里调）",
        "ss_realzoom": "实时缩放 useRealZoom（DynaZoom）：还没有实现",
        "ss_proportions": "元素比例 proportions：两边的刻度不同，不导入（可在「树图 › 元素比例」里调）",
        "ss_template": "自定义导出模板「{name}」：SpaceKnife 还不能保存自定义模板",
        "ss_kept_class": "保留 SpaceKnife 的「{name}」类别（SpaceSniffer 没有这一类）",
    }
)

# SpaceSniffer element -> (SpaceKnife key, converter)
_BOOL: dict[str, str] = {
    "showFreeSpace": "treemap.show_free",
    "showUnknownSpace": "treemap.show_unknown",
    "showFileTooltipDateTime": "general.hover_dates",
    "showFileTooltipAge": "general.hover_age",
    "useDynaSmooth": "treemap.dynasmooth",
    "useSorting": "treemap.sort_by_size",
    "scanADS": "scan.ads",
    "watchDog": "scan.monitor",
    "logPopup": "general.log_auto_open",
    "logWarning": "general.log_scan_warnings",
    "flashElement": "treemap.flash_changes",
    "flashWindow": "general.flash_on_done",
    "keepTooltip": "general.hover_keep_open",
    "mouseHover": "appearance.hover_highlight",
    "mouseTrail": "appearance.mouse_trail",
    "hilightShadow": "appearance.selection_shadow",
}
SS_DEFAULT_COLORS = {"0x000000ff", "0x0000ffff", "0x00ff0000", "0x0000ff00", "0x00c080ff"}
SS_FONT_SIZE = 7  # SpaceSniffer's default label size (pt): SpaceKnife's 100 %
SS_BORDER = 30  # SpaceSniffer's default border contrast: SpaceKnife's 10


@dataclass(slots=True)
class ImportPlan:
    source: Literal["spaceknife", "spacesniffer"]
    values: dict[str, SettingValue] = field(default_factory=dict)  # only what changes
    classes: tuple[FilterClassInfo, ...] | None = None  # a new class table, if it changes
    skipped: list[str] = field(default_factory=list)  # notes, in the language of the moment


class ImportError_(ValueError):
    """A file that cannot be imported (the message is in the current language)."""


# --------------------------------------------------------------------------- export


def export_document(store: SettingsStore) -> dict[str, Any]:
    table = class_table.active()
    return {
        "format": FORMAT,
        "version": 1,
        "values": {k: v for k, v in sorted(store.values.items()) if v != DEFAULTS[k]},
        "classes": [c.model_dump(mode="json") for c in table.classes] if table.custom else None,
    }


def export_to(store: SettingsStore, path: Path) -> None:
    text = json.dumps(export_document(store), ensure_ascii=False, indent=2)
    tmp = path.with_name(f".{path.name}.tmp")
    tmp.write_text(text, encoding="utf-8")
    tmp.replace(path)


# --------------------------------------------------------------------------- import


def read_plan(path: Path, current: dict[str, SettingValue]) -> ImportPlan:
    try:
        if path.stat().st_size > MAX_BYTES:
            raise ImportError_(TEXT["too_big"])
        raw = path.read_bytes()
    except OSError as exc:
        raise ImportError_(TEXT["unreadable"].format(path=path, reason=exc.strerror or exc)) from exc
    head = raw.lstrip(b"\xef\xbb\xbf \t\r\n")[:64]
    if head.startswith(b"{"):
        return _plan_spaceknife(raw, current)
    if head.startswith(b"<"):
        return _plan_spacesniffer(raw, current)
    raise ImportError_(TEXT["unknown_format"])


def _finish(plan: ImportPlan, wanted: dict[str, Any], current: dict[str, SettingValue]) -> ImportPlan:
    """Keep the values that are valid, settable and different from now."""
    for key, value in wanted.items():
        meta = BY_KEY.get(key)
        if meta is None:
            plan.skipped.append(TEXT["unknown_key"].format(key=key))
            continue
        if meta.reason:
            if value != current.get(key):
                plan.skipped.append(TEXT["locked"].format(name=_(meta.name)))
            continue
        try:
            checked = validate({**current, key: value})[key]
        except ValueError:
            plan.skipped.append(TEXT["invalid"].format(key=key, value=value))
            continue
        if checked != current.get(key):
            plan.values[key] = checked
    return plan


def _plan_spaceknife(raw: bytes, current: dict[str, SettingValue]) -> ImportPlan:
    try:
        doc = json.loads(raw.decode("utf-8-sig"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise ImportError_(TEXT["bad_json"].format(reason=exc)) from exc
    if not isinstance(doc, dict) or doc.get("format") != FORMAT:
        raise ImportError_(TEXT["unknown_format"])
    if int(doc.get("version", 1)) > 1:
        raise ImportError_(TEXT["newer"].format(version=doc.get("version")))
    plan = ImportPlan("spaceknife")
    values = doc.get("values") or {}
    # an export lists only non-defaults: everything else goes back to its default
    wanted = {k: values.get(k, DEFAULTS[k]) for k in DEFAULTS}
    wanted.update({k: v for k, v in values.items() if k not in DEFAULTS})
    _finish(plan, wanted, current)
    raw_classes = doc.get("classes")
    table = (
        class_table.ClassTable(tuple(FilterClassInfo.model_validate(c) for c in raw_classes))
        if raw_classes
        else class_table.DEFAULT_CLASSES
    )
    if table.classes != class_table.active().classes:
        plan.classes = table.classes
    return plan


def _text(el: ET.Element | None, tag: str) -> str | None:
    child = el.find(tag) if el is not None else None
    return child.text.strip() if child is not None and child.text is not None else None


def _plan_spacesniffer(raw: bytes, current: dict[str, SettingValue]) -> ImportPlan:
    try:
        root = ET.fromstring(raw)
    except ET.ParseError as exc:
        raise ImportError_(TEXT["bad_xml"].format(reason=exc)) from exc
    if root.tag != "SpaceSnifferConfig":
        raise ImportError_(TEXT["unknown_format"])
    plan = ImportPlan("spacesniffer")
    colors = root.find("colors")
    wanted: dict[str, Any] = {}

    def flag(tag: str, where: ET.Element | None = None) -> bool | None:
        v = _text(where if where is not None else root, tag)
        return None if v is None else v.lower() == "true"

    def number(tag: str, where: ET.Element | None = None) -> int | None:
        v = _text(where if where is not None else root, tag)
        try:
            return int(v) if v is not None else None
        except ValueError:
            return None

    for tag, key in _BOOL.items():
        v = flag(tag) if root.find(tag) is not None else flag(tag, colors)
        if v is not None:
            wanted[key] = v
    if (n := number("minElementSizePx")) is not None:
        wanted["treemap.min_element"] = n
    if (n := number("initialDetailLevel")) is not None:
        wanted["treemap.initial_detail"] = n
    if (n := number("fontSize")) is not None:
        wanted["treemap.font_scale"] = _clamp(round(n / SS_FONT_SIZE * 100), "treemap.font_scale")
    style = _text(root, "initialStyle")
    if style in ("flat", "fileClasses"):
        wanted["appearance.default_color_mode"] = "flat" if style == "flat" else "class"
    if (n := number("levelContrast", colors)) is not None:
        wanted["appearance.level_contrast"] = _clamp(n, "appearance.level_contrast")
    if (n := number("borderContrast", colors)) is not None:
        wanted["appearance.border_contrast"] = _clamp(round(n * 10 / SS_BORDER), "appearance.border_contrast")
    if (n := number("hilightHaloLevels", colors)) is not None:
        wanted["appearance.halo_levels"] = _clamp(n, "appearance.halo_levels")
    _finish(plan, wanted, current)
    if colors is not None and any(
        colors.find(t) is not None for t in ("driveColor", "folderColor", "fileColor")
    ):
        plan.skipped.append(TEXT["ss_colors"])
    for tag, note in (
        ("refreshDelayMs", "ss_refresh"),
        ("zoomTimeMs", "ss_zoom"),
        ("useRealZoom", "ss_realzoom"),
        ("proportions", "ss_proportions"),
    ):
        if root.find(tag) is not None:
            plan.skipped.append(TEXT[note])
    groups = root.find("fileGroups")
    if groups is not None:
        table = _classes_from(groups, plan)
        if table.classes != class_table.active().classes:
            plan.classes = table.classes
    from .export import BUILTIN_TEMPLATES

    for cfg in root.iter("exportConfig"):
        name = _text(cfg, "description") or ""
        if name and name not in BUILTIN_TEMPLATES:
            plan.skipped.append(TEXT["ss_template"].format(name=name))
    return plan


def _clamp(v: int, key: str) -> int:
    meta = BY_KEY[key]
    lo = int(meta.min) if meta.min is not None else v
    hi = int(meta.max) if meta.max is not None else v
    return max(lo, min(hi, v))


def _ss_color(text: str | None) -> str:
    """``0x00BBGGRR`` (a Delphi TColor) -> ``#RRGGBB``; SpaceSniffer's own defaults -> theme ("")."""
    if not text or text.strip().lower() in SS_DEFAULT_COLORS:
        return ""
    try:
        v = int(text, 16) if text.lower().startswith("0x") else int(text)
    except ValueError:
        return ""
    r, g, b = v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF
    return f"#{r:02X}{g:02X}{b:02X}"


def _classes_from(groups: ET.Element, plan: ImportPlan) -> class_table.ClassTable:
    from spaceknife_protocol.classes import ClassEdit

    by_alias: dict[str, FilterClassInfo] = {}
    for c in BUILTIN_CLASSES:
        for spelling in (c.id, c.name, *c.aliases):
            by_alias[spelling.casefold()] = c
    edits: list[ClassEdit] = []
    used: set[str] = set()
    for g in groups.iter("fileGroup"):
        name = (_text(g, "name") or "").strip()
        if not name:
            continue
        known = by_alias.get(name.casefold())
        cid = known.id if known is not None and known.id not in used and known.id != OTHER else None
        if cid:
            used.add(cid)
        edits.append(
            ClassEdit(
                id=cid,
                name=name,
                extensions=[_text(g, "extensions") or ""],
                color=_ss_color(_text(g, "color")),
            )
        )
    for c in BUILTIN_CLASSES:  # SpaceKnife-only classes stay; 「其他」 last
        if c.id not in used and c.id != OTHER:
            edits.append(ClassEdit(id=c.id, name=c.name, extensions=c.extensions))
            plan.skipped.append(TEXT["ss_kept_class"].format(name=localized(c, language()).name))
    other = next(c for c in BUILTIN_CLASSES if c.id == OTHER)
    edits.append(ClassEdit(id=OTHER, name=other.name))
    return class_table.build(edits)

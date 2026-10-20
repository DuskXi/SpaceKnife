"""Write the reference tables of the user documentation from the registries.

Source of truth (hand-edited):
    packages/backend/src/spaceknife/palette/commands.py   palette commands, their names and keys
    packages/backend/src/spaceknife/settings.py           settings: pages, groups, options, defaults

Generated (never edit by hand; the tests run ``--check``), between
``<!-- generated:<name>:start -->`` and ``<!-- generated:<name>:end -->``:
    site/src/content/docs/{docs,zh/docs}/shortcuts.md     ``shortcuts``: the palette commands
    site/src/content/docs/{docs,zh/docs}/settings.md      ``settings``: every setting, page by page

Both registries are read once per platform (Windows, macOS, Linux) with the platform switched in
the modules, so the output is the same on every machine: text that differs by platform is shown
side by side, availability as "Windows only" and the like. English comes from the backend's
catalog (``locales/en``), page names from the Electron UI's catalog.

Usage::

    uv run python tools/gen_docs.py          # rewrite the generated blocks
    uv run python tools/gen_docs.py --check  # exit 1 if any block is out of date
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import types
from collections.abc import Callable, Iterator, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from spaceknife import i18n, settings
from spaceknife.palette import commands
from spaceknife.palette.commands import Command
from spaceknife.settings import Spec

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "site" / "src" / "content" / "docs"
RENDERER = ROOT / "apps" / "electron" / "src" / "renderer" / "src"
FRONTEND_LOCALES = RENDERER / "locales" / "en"
SETTINGS_PAGE = RENDERER / "pages" / "SettingsPage.tsx"
APP_TSX = RENDERER / "App.tsx"

Lang = Literal["en", "zh"]
LANGS: tuple[Lang, ...] = ("en", "zh")
FOLDERS: dict[Lang, Path] = {"en": DOCS / "docs", "zh": DOCS / "zh" / "docs"}

PLATFORMS: tuple[tuple[str, str], ...] = (("win32", "Windows"), ("darwin", "macOS"), ("linux", "Linux"))

# The settings tab's pages in its order, with the subtitle each page shows: (category, name, note).
# Copied from PAGES in apps/electron/src/renderer/src/pages/SettingsPage.tsx (the tests compare).
PAGES: tuple[tuple[str, str, str], ...] = (
    ("general", "常规", "语言、悬停提示、窗口提醒与日志"),
    ("appearance", "外观", "主题、强调色、动画与可视化外观；更改立即生效"),
    (
        "treemap",
        "可视化",
        "几何、火焰图方向与动画；修改后已打开的视图立即重排，「新视图…」类只影响之后新开的视图",
    ),
    ("scan", "扫描与性能", "扫描方式、扫描范围与硬链接计算从下一次扫描起生效，其余更改立即生效"),
    ("classes", "文件类别", "按扩展名归类，用于树图着色、:class: 过滤和类别统计"),
    ("tags", "标签", "四个固定标签：红▲、黄■、绿●、蓝◆，颜色与形状不可修改"),
    ("keys", "快捷键", "全部命令与当前快捷键，与命令面板同一份命令清单"),
    ("advanced", "高级", "设置文件、日志、缓存与快照"),
)

# Keys the palette lists that the Electron window does not handle yet (App.tsx has no handler for
# them): the table marks them. Each comes with what a handler in App.tsx would contain, so the
# tests notice when one is wired up and this entry has to go.
NOT_WIRED: dict[str, str] = {"F5": '"F5"', "Shift+F5": '"F5"', "Ctrl+S": '"s"'}

# Text that stays as it is in every language (an option named in its own language).
SAME_IN_EVERY_LANGUAGE = frozenset({"简体中文"})

ARROWS = {"Left": "←", "Right": "→", "Up": "↑", "Down": "↓"}
CJK = re.compile(r"[㐀-鿿豈-﫿]")

# The documentation's own words around the registry text.
WORDS: dict[Lang, dict[str, str]] = {
    "zh": {
        "setting": "设置",
        "default": "默认值",
        "values": "可选值",
        "description": "说明",
        "key": "设置键",
        "on": "开",
        "off": "关",
        "empty": "空",
        "range": "{lo} 至 {hi}{unit}",
        "step": "，步长 {step}",
        "text": "文字，最多 64 个字符",
        "next_scan": "从下一次扫描起生效。",
        "new_views": "只影响之后新开的视图。",
        "depends_value": "「{name}」选「{value}」时才能修改。",
        "depends_on": "「{name}」打开时才能修改。",
        "unavailable": "暂不可用（界面上显示为灰色）。",
        "only": "仅 {platforms} 可用。",
        "option_unavailable": "（暂不可用）",
        "option_only": "（仅 {platforms}）",
        "platform_text": "{platforms}：{text}",
        "and": "、",
        "no_settings": "这一页没有开关类设置，是文件类别的编辑器，见[标签与文件类别](../tags-classes/)。",
        "command": "命令",
        "keys": "按键",
        "search": "也能用这些词找到",
        "notes": "备注",
        "pinned_head": "### 空输入时列出的命令",
        "pinned_intro": "打开面板、还没有输入时，「命令」组按这个顺序列出这几条：",
        "typed_head": "### 输入后才列出的命令",
        "typed_intro": "其余命令要输入名称、拼音或下表的词才会出现：",
        "typed_only_head": "### 只按名称列出的命令",
        "typed_only_intro": "这几条直接选定一种配色或可视化，只在输入时出现，不进设置里的键位表：",
        "planned": "计划中",
        "not_wired": "{keys} 暂未生效",
        "none": "—",
        "variant": "{text}（{platforms}）",
        "sep": " · ",
    },
    "en": {
        "setting": "Setting",
        "default": "Default",
        "values": "Values",
        "description": "Description",
        "key": "Key",
        "on": "On",
        "off": "Off",
        "empty": "Empty",
        "range": "{lo} to {hi}{unit}",
        "step": ", step {step}",
        "text": "Text, up to 64 characters",
        "next_scan": "Applies from the next scan.",
        "new_views": "Only affects views opened later.",
        "depends_value": 'Can be changed when "{name}" is set to "{value}".',
        "depends_on": 'Can be changed when "{name}" is on.',
        "unavailable": "Not available yet (shown grayed out).",
        "only": "{platforms} only.",
        "option_unavailable": " (not available yet)",
        "option_only": " ({platforms} only)",
        "platform_text": "{platforms}: {text}",
        "and": ", ",
        "no_settings": "This page has no switches or choices: it is the file class editor. "
        "See [Tags and file classes](../tags-classes/).",
        "command": "Command",
        "keys": "Keys",
        "search": "Also found by",
        "notes": "Notes",
        "pinned_head": "### Listed when the input is empty",
        "pinned_intro": "When you open the palette and type nothing, the Commands group lists these, "
        "in this order:",
        "typed_head": "### Listed once you type",
        "typed_intro": "Every other command shows up when you type its name or one of the words below:",
        "typed_only_head": "### Listed only by name",
        "typed_only_intro": "These pick one coloring or visualization directly. They show up only for a "
        "typed query and are not in the key map in Settings:",
        "planned": "Planned",
        "not_wired": "{keys} not active yet",
        "none": "—",
        "variant": "{text} ({platforms})",
        "sep": " · ",
    },
}


class GenError(RuntimeError):
    pass


# ------------------------------------------------------------------ platforms


@contextmanager
def platform(name: str) -> Iterator[None]:
    """The registries as they are built on ``name`` (``win32`` | ``darwin`` | ``linux``)."""
    space = vars(commands)  # commands reads sys.platform when the registry is built
    saved = settings.IS_WIN, settings.IS_MAC, space["sys"]
    settings.IS_WIN, settings.IS_MAC = name == "win32", name == "darwin"
    space["sys"] = types.SimpleNamespace(platform=name)
    try:
        yield
    finally:
        settings.IS_WIN, settings.IS_MAC, space["sys"] = saved


def per_platform[T](build: Callable[[], Sequence[T]], ident: Callable[[T], str]) -> list[tuple[T, ...]]:
    """Each entry of a registry as built on every platform of :data:`PLATFORMS`, in that order."""
    runs: list[tuple[T, ...]] = []
    for name, _label in PLATFORMS:
        with platform(name):
            runs.append(tuple(build()))
    ids = [[ident(x) for x in run] for run in runs]
    if any(i != ids[0] for i in ids):
        raise GenError("a registry lists different entries on different platforms")
    return list(zip(*runs, strict=True))


def command_variants() -> list[tuple[Command, ...]]:
    return per_platform(commands._registry, lambda c: c.id)


def setting_variants() -> list[tuple[Spec, ...]]:
    return per_platform(settings._registry, lambda s: s.key)


# ------------------------------------------------------------------ language


@dataclass
class Words:
    """Text in one language: the registries' Chinese message ids translated, the docs' own words."""

    lang: Lang
    missing: set[str]

    @staticmethod
    def frontend() -> dict[str, str]:
        out: dict[str, str] = {}
        for f in sorted(FRONTEND_LOCALES.glob("*.json")):
            out.update({str(k): str(v) for k, v in json.loads(f.read_text(encoding="utf-8")).items() if v})
        return out

    def __post_init__(self) -> None:
        self._backend = i18n.catalog("en")
        self._frontend = self.frontend()

    def tr(self, text: str, *, ui: bool = False) -> str:
        """``text`` in this language; ``ui``: the Electron UI's catalog first (page names)."""
        if self.lang == "zh" or not text or text.isascii() or text in SAME_IN_EVERY_LANGUAGE:
            return text
        first, second = (self._frontend, self._backend) if ui else (self._backend, self._frontend)
        found = first.get(text) or second.get(text)
        if found is None:
            self.missing.add(text)
            return text
        return found

    def __getitem__(self, key: str) -> str:
        return WORDS[self.lang][key]

    def sentence(self, text: str) -> str:
        text = text.strip()
        if not text:
            return ""
        if self.lang == "zh":
            return text if text[-1] in "。！？" else text + "。"
        return text if text[-1] in ".!?" else text + "."

    def join(self, *parts: str) -> str:
        parts = tuple(p for p in parts if p)
        return ("" if self.lang == "zh" else " ").join(parts)

    def platforms(self, names: Sequence[str]) -> str:
        return self["and"].join(names)


# ------------------------------------------------------------------ Markdown


def cell(text: str) -> str:
    """Plain text for a table cell: Markdown punctuation and the pipe escaped."""
    out = []
    for ch in text:
        if ch in "\\`*_|[]":
            out.append("\\" + ch)
        elif ch == "<":
            out.append("&lt;")
        else:
            out.append(ch)
    return "".join(out).replace("\n", " ")


def code(value: object) -> str:
    """A value as it is written in settings.json, as inline code."""
    return f"`{json.dumps(value, ensure_ascii=False)}`"


def kbd(keys: str) -> str:
    """Neutral key notation (``Ctrl+Shift+F5``) as ``<kbd>`` keys, arrows as arrows."""
    parts = re.split(r"\+(?=.)", keys)
    out = []
    for p in parts:
        p = ARROWS.get(p, p)
        out.append("<kbd>" + p.replace("&", "&amp;").replace("<", "&lt;").replace("`", "&#96;") + "</kbd>")
    return "+".join(out)


def table(head: Sequence[str], rows: Sequence[Sequence[str]]) -> list[str]:
    lines = ["| " + " | ".join(head) + " |", "|" + "|".join(" --- " for _ in head) + "|"]
    lines += ["| " + " | ".join(r) + " |" for r in rows]
    return lines


def varying[T](w: Words, variants: Sequence[T], show: Callable[[T], str]) -> str:
    """One text when every platform has the same, else each text with its platforms."""
    shown = [show(v) for v in variants]
    if len(set(shown)) == 1:
        return shown[0]
    groups: dict[str, list[str]] = {}
    for (_name, label), text in zip(PLATFORMS, shown, strict=True):
        groups.setdefault(text, []).append(label)
    return w.join(
        *(w["platform_text"].format(platforms=w.platforms(p), text=text) for text, p in groups.items())
    )


def availability(w: Words, reasons: Sequence[str]) -> tuple[str, list[str]]:
    """("always" | "never" | "some", the platforms it is available on) from per-platform reasons."""
    on = [label for (_name, label), reason in zip(PLATFORMS, reasons, strict=True) if not reason]
    if len(on) == len(PLATFORMS):
        return "always", on
    return ("never" if not on else "some"), on


# ------------------------------------------------------------------ settings


def _number(v: float | None) -> str:
    if v is None:
        return ""
    return str(int(v)) if float(v).is_integer() else str(v)


def _unit(w: Words, spec: Spec) -> str:
    unit = w.tr(spec.unit) if spec.unit else ""
    return f" {unit}" if unit else ""


def _option_label(w: Words, label: str, value: object) -> str:
    text = w.tr(label)
    return code(value) if text == str(value) else f"{cell(text)} {code(value)}"


def _option_reason(option: tuple[object, ...]) -> str:
    """Why an option can't be picked here ("" when it can)."""
    return str(option[2]) if len(option) > 2 else ""


def setting_values(w: Words, variants: Sequence[Spec]) -> str:
    spec = variants[0]
    if spec.control == "switch":
        return f"{w['on']} {code(True)}{w['sep']}{w['off']} {code(False)}"
    if spec.control == "text":
        return w["text"]
    if spec.control in ("slider", "number"):
        out = w["range"].format(lo=_number(spec.min), hi=_number(spec.max), unit=_unit(w, spec))
        if spec.step not in (None, 1):
            out += w["step"].format(step=_number(spec.step))
        return cell(out)
    items = []
    for i, option in enumerate(spec.options):
        value, label = option[0], option[1]
        text = code(value) if spec.control == "color" else _option_label(w, label, value)
        reasons = [_option_reason(v.options[i]) for v in variants]
        kind, on = availability(w, reasons)
        if kind == "never":
            text += w["option_unavailable"]
        elif kind == "some":
            text += w["option_only"].format(platforms=w.platforms(on))
        items.append(text)
    return w["sep"].join(items)


def setting_default(w: Words, spec: Spec) -> str:
    v = spec.default
    if spec.control == "switch":
        return w["on"] if v else w["off"]
    if spec.control == "text":
        return w["empty"] if v == "" else cell(str(v))
    if spec.control in ("slider", "number"):
        return cell(f"{_number(float(v))}{_unit(w, spec)}")
    if spec.control == "color":
        return code(v)
    for option in spec.options:
        if option[0] == v:
            return cell(w.tr(option[1]))
    raise GenError(f"{spec.key}: the default {v!r} is not one of the options")


def setting_description(w: Words, variants: Sequence[Spec], by_key: dict[str, Spec]) -> str:
    spec = variants[0]
    text = varying(w, variants, lambda s: w.sentence(w.tr(s.description)))
    notes = []
    if spec.applies == "next_scan":
        notes.append(w["next_scan"])
    elif spec.applies == "new_views":
        notes.append(w["new_views"])
    if spec.depends:
        key, _sep, want = spec.depends.partition("=")
        other = by_key[key]
        if want:
            label = next(o[1] for o in other.options if str(o[0]) == want)
            notes.append(w["depends_value"].format(name=w.tr(other.name), value=w.tr(label)))
        else:
            notes.append(w["depends_on"].format(name=w.tr(other.name)))
    kind, on = availability(w, [v.reason for v in variants])
    if kind == "never":
        notes.append(w["unavailable"])
    elif kind == "some":
        notes.append(w["only"].format(platforms=w.platforms(on)))
    return cell(w.join(text, *notes))


def settings_block(w: Words) -> str:
    variants = setting_variants()
    by_key = {v[0].key: v[0] for v in variants}
    known = {p[0] for p in PAGES}
    stray = sorted({v[0].category for v in variants} - known)
    if stray:
        raise GenError(f"settings on pages gen_docs.PAGES does not list: {', '.join(stray)}")
    head = [w["setting"], w["default"], w["values"], w["description"], w["key"]]
    lines: list[str] = []
    for category, name, note in PAGES:
        lines += ["", f"### {w.tr(name, ui=True)}", "", w.sentence(w.tr(note, ui=True))]
        page = [v for v in variants if v[0].category == category]
        if not page:
            lines += ["", w["no_settings"]]
            continue
        groups: dict[str, list[tuple[Spec, ...]]] = {}
        for v in page:
            groups.setdefault(v[0].group, []).append(v)
        for group, members in groups.items():
            lines += ["", f"#### {w.tr(group)}"]
            group_note = members[0][0].group_note
            if group_note:
                lines += ["", w.sentence(w.tr(group_note))]
            rows = [
                [
                    cell(w.tr(v[0].name)),
                    setting_default(w, v[0]),
                    setting_values(w, v),
                    setting_description(w, v, by_key),
                    f"`{v[0].key}`",
                ]
                for v in members
            ]
            lines += ["", *table(head, rows)]
    return "\n".join(lines[1:])


# ------------------------------------------------------------------ commands


def command_row(w: Words, variants: Sequence[Command]) -> list[str]:
    cmd = variants[0]
    names = [w.tr(c.name) for c in variants]
    if len(set(names)) == 1:
        name = cell(names[0])
    else:
        groups: dict[str, list[str]] = {}
        for (_n, label), text in zip(PLATFORMS, names, strict=True):
            groups.setdefault(text, []).append(label)
        name = " / ".join(
            cell(w["variant"].format(text=text, platforms=w.platforms(p))) for text, p in groups.items()
        )
    keys = w["sep"].join(kbd(k) for k in cmd.keys) or w["none"]
    search = w["sep"].join(f"`{a}`" for a in cmd.aliases) or w["none"]
    notes = []
    if cmd.milestone:
        notes.append(w["planned"])
    idle = [k for k in cmd.keys if k in NOT_WIRED]
    if idle:
        notes.append(w["not_wired"].format(keys=w["and"].join(kbd(k) for k in idle)))
    return [name, keys, search, w["sep"].join(notes) or w["none"]]


def shortcuts_block(w: Words) -> str:
    variants = command_variants()
    head = [w["command"], w["keys"], w["search"], w["notes"]]
    pinned = sorted((v for v in variants if v[0].pinned), key=lambda v: v[0].pinned)
    typed = [v for v in variants if not v[0].pinned and not v[0].typed_only]
    typed_only = [v for v in variants if v[0].typed_only]
    lines: list[str] = []
    for title, intro, members in (
        ("pinned_head", "pinned_intro", pinned),
        ("typed_head", "typed_intro", typed),
        ("typed_only_head", "typed_only_intro", typed_only),
    ):
        if not members:
            continue
        lines += ["", w[title], "", w[intro], "", *table(head, [command_row(w, v) for v in members])]
    return "\n".join(lines[1:])


# ------------------------------------------------------------------ files

BLOCKS: dict[str, tuple[str, Callable[[Words], str]]] = {
    "shortcuts": ("shortcuts.md", shortcuts_block),
    "settings": ("settings.md", settings_block),
}


def markers(name: str) -> tuple[str, str]:
    return f"<!-- generated:{name}:start -->", f"<!-- generated:{name}:end -->"


def replace_block(text: str, name: str, body: str) -> str:
    """``text`` with the lines between the block's two markers replaced by ``body``."""
    start, end = markers(name)
    a = text.find(start)
    b = text.find(end)
    if a < 0 or b < 0 or b < a or text.count(start) != 1 or text.count(end) != 1:
        raise GenError(f"expected one {start} … {end} pair")
    return text[: a + len(start)] + "\n\n" + body.strip("\n") + "\n\n" + text[b:]


def outputs() -> dict[Path, str]:
    """Every generated file as it should read now."""
    out: dict[Path, str] = {}
    for lang in LANGS:
        w = Words(lang, set())
        for name, (filename, build) in BLOCKS.items():
            path = FOLDERS[lang] / filename
            text = path.read_text(encoding="utf-8").replace("\r\n", "\n")
            try:
                out[path] = replace_block(text, name, build(w))
            except GenError as exc:
                raise GenError(f"{path.relative_to(ROOT)}: {exc}") from exc
        if w.missing:
            raise GenError("no English text for: " + ", ".join(sorted(w.missing)) + " (add it to locales/en)")
    return out


def stale(wanted: dict[Path, str]) -> list[Path]:
    return [p for p, text in wanted.items() if p.read_text(encoding="utf-8").replace("\r\n", "\n") != text]


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0] if __doc__ else None)
    parser.add_argument("--check", action="store_true", help="exit 1 if a generated block is out of date")
    args = parser.parse_args(argv)
    try:
        wanted = outputs()
    except GenError as exc:
        print(f"gen_docs: {exc}", file=sys.stderr)
        return 1
    old = stale(wanted)
    if args.check:
        for p in old:
            print(f"out of date: {p.relative_to(ROOT).as_posix()}", file=sys.stderr)
        if old:
            print("run: uv run python tools/gen_docs.py", file=sys.stderr)
        return 1 if old else 0
    for p in old:
        p.write_text(wanted[p], encoding="utf-8", newline="\n")
        print(f"wrote {p.relative_to(ROOT).as_posix()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

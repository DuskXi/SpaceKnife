"""The command registry (UI-W4-12…25): names, aliases, default keys, availability and status texts.

Every frontend runs a command by its ``id``. Names match the labels used on menus and buttons;
aliases add English words, and the full pinyin and pinyin initials of the name are generated so
that ``qhps`` finds 切换配色 (W4 待定 6): read from ``pinyin.json`` (``tools/gen_pinyin.py``), so
the backend does not import pypinyin (~50 MB of dictionaries) for a fixed set of names. Keys use
a neutral notation (``Ctrl+Shift+F5``) that frontends render per platform; the keymap becomes
editable with W7 (M6).

M7.5: names, statuses and reasons are Chinese message ids, translated when the palette answers
(:func:`label`, :func:`evaluate`); in the English interface the English name is searched first and
the hits light up on it, the Chinese name, its pinyin and the aliases still find the command.
"""

from __future__ import annotations

import json
import sys
from collections.abc import Callable
from dataclasses import dataclass, field
from functools import cache
from pathlib import Path
from typing import Any

from spaceknife_protocol import CAP_COLORS, CAP_VISUALS
from spaceknife_protocol.plugins import CycleAction, ToggleAction, ViewAction
from spaceknife_protocol.text import Text

from ..i18n import SOURCE, Texts, _, language, plural, tr
from ..layout import registry as visuals
from ..view.state import View, showable
from .fuzzy import score

COLOR_NAMES = Texts({"class": "文件类别", "flat": "扁平", "time": "修改时间", "branch": "顶层分支"})
VISUAL_NAMES = Texts({"treemap": "树图", "flame": "火焰图", "flame_polar": "火焰-极坐标图"})


@dataclass(frozen=True, slots=True)
class Context:
    tab: str  # scan | start | settings
    scanning: bool = False
    paused: bool = False
    stopped: bool = False  # a hard scan was stopped with folders left to read (scan.continue)
    method: str = "hard"  # hard | mft | snapshot: how the tree is being read
    has_scan: bool = False
    source: str = "scan"  # scan | snapshot
    elapsed_s: float | None = None
    view: View | None = None
    snapshots: int = 0
    basket: int = 0
    caps: frozenset[str] = frozenset()  # what the session announced: visualizations, branch colours
    app_version: str = ""  # the running app's version (update.status)
    update_version: str = ""  # a newer release, "" when there is none
    update_checking: bool = False
    settings: Any = None  # the SettingsStore (commands available while a setting is on)
    plugins: frozenset[str] = frozenset()  # enabled plugins
    scan_source: str = ""  # the scan source of the scan in front (a plugin's full id), "" for disks
    node: int | None = None  # what the command is about when not the selection (a context menu's block)

    @property
    def selected_node(self) -> int | None:
        v = self.view
        chosen = self.node if self.node is not None else v.selected if v is not None else None
        if v is None or chosen is None or not 0 < chosen < len(v.store):
            return None
        return chosen


Need = Callable[[Context], str | None]  # None when satisfied, else the reason shown in place of the status


def _scan(c: Context) -> str | None:
    return None if c.has_scan else _("需要先完成扫描")


def _done(c: Context) -> str | None:
    return _scan(c) or (_("扫描进行中") if c.scanning else None)


def _running(c: Context) -> str | None:
    return None if c.scanning else _("没有进行中的扫描")


def _pausable(c: Context) -> str | None:
    if not c.scanning:
        return _("没有进行中的扫描")
    if c.method == "mft":
        return _("MFT 快速扫描几秒内完成，无需暂停")
    return _("已暂停") if c.paused else None


def _stopped(c: Context) -> str | None:
    return None if c.stopped else _("没有可以继续的扫描")


def _paused(c: Context) -> str | None:
    return _running(c) or (None if c.paused else _("扫描没有暂停"))


def _view(c: Context) -> str | None:
    return None if c.view is not None else _("需要先完成扫描")


def _trash_view(c: Context) -> str | None:
    if c.view is None:
        return _("需要先完成扫描")
    return None if c.view.has_trash else _("这次扫描里没有回收站")


def _scan_view(c: Context) -> str | None:
    """A view of the scan itself: a comparison's view shows changes, its folders are not the scan's."""
    return _view(c) or (
        _("对比时不可用，先退出对比") if c.view is not None and c.view.compare is not None else None
    )


def _selection(c: Context) -> str | None:
    return _view(c) or (None if c.selected_node is not None else _("没有选中项"))


def _back(c: Context) -> str | None:
    return _view(c) or (None if c.view and c.view.back else _("没有可后退的位置"))


def _forward(c: Context) -> str | None:
    return _view(c) or (None if c.view and c.view.forward else _("没有可前进的位置"))


def _up(c: Context) -> str | None:
    return _view(c) or (None if c.view and c.view.can_go_up() else _("已经在根"))


def _visuals(c: Context) -> str | None:
    """Cycling needs a second visualization this window can show."""
    return _view(c) or (None if len(_shown(c)) > 1 else _("这种可视化在当前界面不可用"))


def _visual_ok(visual: str) -> Need:
    return lambda c: _view(c) or (None if visual in _shown(c) else _("这种可视化在当前界面不可用"))


def _branch_ok(c: Context) -> str | None:
    return _view(c) or (None if CAP_VISUALS in c.caps else _("这种配色在当前界面不可用"))


def _shown(c: Context) -> list[str]:
    """The visualizations the view can switch to in this window."""
    compare = c.view is not None and c.view.compare is not None
    out = []
    for v in visuals.all_visuals():
        vis = showable(v.id, c.caps)
        if vis is not None and (vis.compare or not compare):
            out.append(v.id)
    return out


def _snapshots(c: Context) -> str | None:
    return None if c.snapshots else _("没有快照")


def _tab(c: Context) -> str | None:
    return None if c.tab == "scan" else _("需要先完成扫描")


def _update(c: Context) -> str | None:
    return None if c.update_version else _("没有可用的更新")


@dataclass(frozen=True, slots=True)
class Command:
    id: str
    name: str
    icon: str
    keys: tuple[str, ...] = ()
    aliases: tuple[str, ...] = ()
    pinned: int = 0  # 1–9: position in the empty-input list
    needs: tuple[Need, ...] = ()
    status: Callable[[Context], str] | None = None
    milestone: str = ""  # not wired up yet: disabled until then
    typed_only: bool = False  # only listed for a typed query (W4 待定 3: colour choices)
    search: tuple[tuple[str, tuple[int, ...] | None], ...] = field(default=(), compare=False)
    action: Any = None  # plugins.v1: what running it does (a protocol Action); None: the frontend's own
    menus: tuple[str, ...] = ()  # where else it is offered: node, inspector, toolbar, start
    owner: str = "builtin"  # or the plugin that adds it
    text: Text | None = None  # a plugin's name in both languages (``name`` is its Chinese text)
    pinyin: tuple[str, ...] | None = None  # a plugin's pinyin of the name (its generated/pinyin.json)

    @property
    def more(self) -> bool:
        return self.name.endswith("…")


def _rescan_status(c: Context) -> str:
    method = _("MFT 快速扫描") if c.method == "mft" else _("硬扫描")
    if c.source != "scan" or c.elapsed_s is None:
        return method
    return tr("{method} · 约 {seconds} 秒", method=method, seconds=max(1, round(c.elapsed_s)))


def color_name(mode: str) -> str:
    """A colour mode's name in the current language (a plugin's from its declaration)."""
    name: str | None = COLOR_NAMES.get(mode)
    if name is not None:
        return name
    from ..extensions import EXTENSIONS

    found = EXTENSIONS.get("color_mode", mode)
    return found.spec.title.get(language()) if found is not None else mode


def _color(c: Context) -> str:
    return tr("当前：{mode}", mode=color_name(c.view.color_mode)) if c.view else ""


def _visual_name(visual: str) -> str:
    name: str | None = VISUAL_NAMES.get(visual)
    if name is None:
        try:
            name = visuals.get(visual).title
        except KeyError:
            name = visual
    return name


def _visual(c: Context) -> str:
    return tr("当前：{name}", name=_visual_name(c.view.visual)) if c.view else ""


def _free(c: Context) -> str:
    return (_("当前：显示") if c.view.show_free else _("当前：隐藏")) if c.view else ""


def _unknown(c: Context) -> str:
    return (_("当前：显示") if c.view.show_unknown else _("当前：隐藏")) if c.view else ""


def _trash(c: Context) -> str:
    return (_("当前：显示") if c.view.show_trash else _("当前：隐藏")) if c.view else ""


def _basket(c: Context) -> str:
    return plural(c.basket, "{count} 项", "1 item", count=f"{c.basket:,}") if c.basket else _("空")


def _snapshot_count(c: Context) -> str:
    return plural(c.snapshots, "{count} 个快照", "1 snapshot", count=c.snapshots)


def _update_check(c: Context) -> str:
    if c.update_checking:
        return _("正在检查…")
    if c.update_version:
        return tr("有新版本 {version}", version=c.update_version)
    return tr("当前版本 {version}", version=c.app_version) if c.app_version else ""


def _color_pick(mode: str) -> Callable[[Context], str]:
    return lambda c: _("当前") if c.view is not None and c.view.color_mode == mode else ""


def _visual_pick(visual: str) -> Callable[[Context], str]:
    return lambda c: _("当前") if c.view is not None and c.view.visual == visual else ""


def _registry() -> tuple[Command, ...]:
    """The built-in commands (tools/gen_docs.py and gen_pinyin.py read exactly these)."""
    return (
        *_static(),
        *_color_commands(),
        *_visual_commands([v for v in visuals.all_visuals() if v.plugin is None]),
    )


def _static() -> tuple[Command, ...]:
    C = Command
    return (
        # the empty-input list, in this order (UI-W4-12)
        C(
            "view.rescan",
            "重新扫描当前视图",
            "refresh",
            ("F5",),
            ("rescan", "refresh"),
            1,
            (_tab, _done, _scan_view),
        ),
        C(
            "scan.rescan_all",
            "全盘重新扫描",
            "drive",
            ("Shift+F5",),
            ("rescan all", "full rescan"),
            2,
            (_tab, _done),
            _rescan_status,
        ),
        C(
            "view.cycle_color",
            "切换配色",
            "palette",
            ("C",),
            ("color", "colour", "theme"),
            3,
            (_view,),
            _color,
            action=CycleAction(what="color"),
        ),
        C(
            "view.toggle_free",
            "显示/隐藏可用空间",
            "eye",
            ("F",),
            ("free space",),
            4,
            (_view,),
            _free,
            action=ToggleAction(name="show_free"),
        ),
        C(
            "basket.open",
            "打开删除篮",
            "basket",
            (),
            ("basket", "trash", "delete"),
            5,
            (),
            _basket,
        ),
        C("snapshot.save", "保存快照", "snapshot", ("Ctrl+S",), ("save snapshot",), 6, (_tab, _done)),
        C(
            "snapshot.compare",
            "与快照对比…",
            "diff",
            (),
            ("compare", "diff"),
            7,
            (_tab, _done, _snapshots),
            _snapshot_count,
        ),
        C(
            "report.export",
            "导出报告…",
            "upload",
            (),
            ("export", "report", "csv", "json", "html"),
            8,
            (_tab, _scan),
            lambda c: "CSV · JSON · HTML",
        ),
        C("app.settings", "设置", "settings", ("Ctrl+,",), ("settings", "preferences", "options"), 9),
        # everything else, found by typing (UI-W4-23)
        C("nav.back", "后退", "back", ("Backspace", "Alt+Left"), ("back",), needs=(_back,)),
        C("nav.forward", "前进", "forward", ("Alt+Right",), ("forward",), needs=(_forward,)),
        C("nav.up", "上一级", "up", ("Alt+Up",), ("up", "parent"), needs=(_up,)),
        C("nav.home", "根", "home", ("Ctrl+Home",), ("home", "root"), needs=(_up,)),
        C("view.detail_up", "提高细节层级", "plus", ("Ctrl+=",), ("more detail", "deeper"), needs=(_view,)),
        C("view.detail_down", "降低细节层级", "minus", ("Ctrl+-",), ("less detail",), needs=(_view,)),
        C(
            "view.cycle_visual",
            "切换可视化",
            "vizFlamePolar",
            ("V",),
            ("visualization", "chart", "treemap", "flame", "icicle", "flame_polar", "radial", "rings"),
            needs=(_visuals,),
            status=_visual,
            action=CycleAction(what="visual"),
        ),
        C("filter.focus", "聚焦过滤", "filter", ("/", "Ctrl+F"), ("filter", "find"), needs=(_view,)),
        C("path.edit", "编辑路径", "search", ("Ctrl+L",), ("path", "go to", "address"), needs=(_view,)),
        C("view.zoom_in", "放大进入", "layers", ("Enter",), ("zoom", "enter"), needs=(_selection,)),
        C(
            "basket.add",
            "加入删除篮",
            "basket",
            ("Delete",),
            ("delete", "remove", "trash"),
            needs=(_selection,),
        ),
        C(
            "item.preview",
            "预览",
            "eye",
            ("Space",),
            ("preview", "quick look"),
            needs=(_selection,),
            milestone="TODO",
        ),
        C("item.copy_path", "复制路径", "copy", ("Ctrl+C",), ("copy path",), needs=(_selection,)),
        C(
            "item.reveal",
            "在访达中显示"
            if sys.platform == "darwin"
            else "在资源管理器中显示"
            if sys.platform == "win32"
            else "在文件管理器中显示",
            "reveal",
            ("Ctrl+E",),
            ("reveal", "explorer", "finder", "show in folder"),
            needs=(_selection,),
        ),
        C(
            "item.terminal",
            "在此打开终端",
            "terminal",
            ("Ctrl+`",),
            ("terminal", "shell", "console"),
            needs=(_selection,),
        ),
        C("tab.new", "新建标签", "plus", ("Ctrl+T",), ("new tab",)),
        C("tab.close", "关闭标签", "close", ("Ctrl+W",), ("close tab",)),
        C("tab.move_left", "标签左移", "back", ("Ctrl+Shift+PageUp",), ("move tab left", "reorder tabs")),
        C(
            "tab.move_right",
            "标签右移",
            "forward",
            ("Ctrl+Shift+PageDown",),
            ("move tab right", "reorder tabs"),
        ),
        C("tag.red", "红色标签", "tag", ("1", "Ctrl+1"), ("red tag",), needs=(_selection,)),
        C("tag.yellow", "黄色标签", "tag", ("2", "Ctrl+2"), ("yellow tag",), needs=(_selection,)),
        C("tag.green", "绿色标签", "tag", ("3", "Ctrl+3"), ("green tag",), needs=(_selection,)),
        C("tag.blue", "蓝色标签", "tag", ("4", "Ctrl+4"), ("blue tag",), needs=(_selection,)),
        C("tag.clear", "清除标签", "tag", ("0", "Ctrl+0"), ("clear tag", "untag"), needs=(_selection,)),
        C(
            "view.toggle_unknown",
            "显示/隐藏未知空间",
            "eye",
            ("U",),
            ("unknown space",),
            needs=(_view,),
            status=_unknown,
            action=ToggleAction(name="show_unknown"),
        ),
        C(
            "view.toggle_trash",
            "显示/隐藏回收站",
            "basket",
            ("R",),
            ("trash", "recycle bin"),
            needs=(_trash_view,),
            status=_trash,
            action=ToggleAction(name="show_trash"),
        ),
        C("view.split", "分屏", "split", (), ("split", "side by side"), needs=(_view,)),
        C("scan.pause", "暂停扫描", "pause", (), ("pause",), needs=(_pausable,)),
        C("scan.resume", "继续扫描", "play", (), ("resume", "continue"), needs=(_paused,)),
        C("scan.stop", "停止扫描", "stop", (), ("stop", "cancel"), needs=(_running,)),
        C("scan.continue", "继续已停止的扫描", "play", (), ("continue", "go on"), needs=(_stopped,)),
        C("scan.new", "新扫描", "drive", (), ("new scan", "scan")),
        C("snapshot.open", "打开快照文件…", "snapshot", (), ("open snapshot", "load")),
        C("app.log", "日志面板", "log", ("Ctrl+J",), ("log", "logs", "console")),
        # the documentation, bundled with the app (Electron: pages/HelpPage, opened by F1 as well)
        C("app.help", "帮助", "info", ("F1",), ("help", "docs", "documentation", "manual", "guide")),
        C(
            "app.check_updates",
            "检查更新",
            "refresh",
            (),
            ("check for updates", "update", "upgrade", "new version"),
            status=_update_check,
        ),
        C(
            "app.show_update",
            "查看更新",
            "info",
            (),
            ("update", "release notes", "what's new", "download update"),
            needs=(_update,),
            status=lambda c: c.update_version,
        ),
        # every physical disk with its interface and link, and the volumes on it
        C(
            "app.disks",
            "本机磁盘",
            "diskGauge",
            (),
            ("disks", "drives", "hardware", "ssd", "nvme", "sata", "usb", "pcie"),
        ),
        # the settings page with the version, the update settings and the release timeline
        C(
            "app.about",
            "关于与更新",
            "info",
            (),
            ("about", "version", "release notes", "changelog", "timeline"),
            status=lambda c: c.app_version,
        ),
    )


# the built-in colour modes' commands: (mode, name, aliases); 「顶层分支」 needs visuals.v1
# names of the generated commands: their message id is the filled-in Chinese text
NAMED = Texts({"visual": "可视化：{name}", "color": "配色：{name}", "filter": "过滤：{name}"})
COLOR_COMMAND_NAMES = Texts(
    {"class": "配色：文件类别", "flat": "配色：扁平", "time": "配色：修改时间", "branch": "配色：顶层分支"}
)
BUILTIN_COLORS: tuple[tuple[str, str, tuple[str, ...]], ...] = tuple(
    (mode, COLOR_COMMAND_NAMES.raw(mode), aliases)
    for mode, aliases in (
        ("class", ("color class",)),
        ("flat", ("color flat",)),
        ("time", ("color time", "age")),
        ("branch", ("color branch", "top level")),
    )
)


def _color_ok(mode: str) -> Need:
    def need(c: Context) -> str | None:
        reason = _view(c)
        if reason:
            return reason
        if CAP_COLORS not in c.caps:
            return _("这种配色在当前界面不可用")
        return None

    return need


def _color_commands() -> tuple[Command, ...]:
    out = []
    for mode, name, aliases in BUILTIN_COLORS:
        out.append(
            Command(
                f"view.color.{mode}",
                name,
                "palette",
                (),
                aliases,
                needs=(_branch_ok if mode == "branch" else _view,),
                status=_color_pick(mode),
                typed_only=True,
                action=ViewAction(settings={"color_mode": mode}),
            )
        )
    return tuple(out)


def _visual_commands(vis: list[Any]) -> tuple[Command, ...]:
    """「可视化：…」 for each visualization of the registry (built-ins: their message ids)."""
    out = []
    for v in vis:
        text = None
        if v.text is not None:
            text = Text(
                zh=NAMED.raw("visual").format(name=v.text.zh),
                en=f"Visualization: {v.text.en}" if v.text.en else "",
            )  # i18n: ignore
        out.append(
            Command(
                f"view.visual.{v.id}",
                NAMED.raw("visual").format(name=v.title_id if v.text is None else v.text.zh),
                v.icon,
                (),
                v.aliases,
                needs=(_visual_ok(v.id), *v.needs),
                status=_visual_pick(v.id),
                typed_only=True,
                action=ViewAction(settings={"visual": v.id}),
                owner=v.plugin or "builtin",
                text=text,
            )
        )
    return tuple(out)


def each_char(text: str) -> Any:
    return list(text)  # one entry per non-hanzi character, so entries line up with the name


@cache
def _pinyin_table() -> dict[str, list[str]]:
    text = Path(__file__).with_name("pinyin.json").read_text(encoding="utf-8")
    table: dict[str, list[str]] = json.loads(text)
    return table


def _syllables(name: str) -> list[str]:
    """The pinyin of a command name (one entry per character), from the generated table."""
    known = _pinyin_table().get(name)
    if known is not None:
        return known
    from pypinyin import lazy_pinyin  # a name the table misses: run tools/gen_pinyin.py

    return lazy_pinyin(name, errors=each_char)


def _search_keys(cmd: Command) -> tuple[tuple[str, tuple[int, ...] | None], ...]:
    """(text, map from text index to name index or None) for the name, its pinyin and its aliases.
    A plugin's command uses the pinyin its plugin ships (``plugin build``), or none: the backend never
    loads pypinyin for names it does not know beforehand."""
    name = cmd.name
    if cmd.owner != "builtin":
        syllables = list(cmd.pinyin) if cmd.pinyin is not None else list(name)
    else:
        syllables = _syllables(name)
    full: list[str] = []
    full_map: list[int] = []
    initials: list[str] = []
    init_map: list[int] = []
    if len(syllables) == len(name):
        for i, (ch, py) in enumerate(zip(name, syllables, strict=True)):
            if py == ch:  # not a hanzi
                if ch.isalnum():
                    full.append(ch.lower())
                    full_map.append(i)
                    initials.append(ch.lower())
                    init_map.append(i)
                continue
            full.extend(py)
            full_map.extend([i] * len(py))
            initials.append(py[0])
            init_map.append(i)
    keys: list[tuple[str, tuple[int, ...] | None]] = [(name, tuple(range(len(name))))]
    if full:
        keys.append(("".join(full), tuple(full_map)))
        keys.append(("".join(initials), tuple(init_map)))
    keys.extend((a, None) for a in cmd.aliases)
    return tuple(keys)


_cache: tuple[tuple[int, tuple[str, ...]], tuple[Command, ...]] | None = None


def registry() -> tuple[Command, ...]:
    """Every command: the built-in ones, then the plugins' (with the effective keys of the single key
    table, :func:`.keymap.apply`). Built again when the extension registry changes."""
    global _cache
    from dataclasses import replace

    from ..extensions import EXTENSIONS
    from .keymap import apply

    all_vis = visuals.all_visuals()
    key = (EXTENSIONS.version, tuple(v.id for v in all_vis))
    hit = _cache
    if hit is not None and hit[0] == key:
        return hit[1]
    cmds = [*_static(), *_color_commands(), *_visual_commands([v for v in all_vis if v.plugin is None])]
    cmds += plugin_commands([v for v in all_vis if v.plugin is not None])
    built = tuple(replace(c, search=_search_keys(c)) for c in apply(cmds))
    _cache = (key, built)
    return built


def plugin_commands(plugin_visuals: list[Any]) -> list[Command]:
    """The plugins' commands, filter presets, and the generated visualization / colour commands."""
    from ..extensions import EXTENSIONS

    out: list[Command] = list(_visual_commands(plugin_visuals))
    for c in EXTENSIONS.of("color_mode"):
        if c.builtin:
            continue
        title: Text = c.spec.title
        out.append(
            Command(
                f"view.color.{c.id}",
                NAMED.raw("color").format(name=title.zh),
                "palette",
                (),
                ("color", c.local),
                needs=(_color_ok(c.id),),
                status=_color_pick(c.id),
                typed_only=True,
                action=ViewAction(settings={"color_mode": c.id}),
                owner=c.owner,
                text=Text(
                    zh=NAMED.raw("color").format(name=title.zh),
                    en=f"Colours: {title.en}" if title.en else "",
                ),  # i18n: ignore
            )
        )
    for c in EXTENSIONS.of("command"):
        if isinstance(c.impl, Command):
            out.append(c.impl)
    return out


def label(cmd: Command) -> str:
    """The command's name in the current language (what :func:`match` hits refer to)."""
    if cmd.text is not None:
        return cmd.text.get(language())
    return _(cmd.name)


def evaluate(cmd: Command, ctx: Context) -> tuple[bool, str]:
    """(enabled, status text or the reason it is disabled), in the current language."""
    status = cmd.status(ctx) if cmd.status is not None else ""
    for need in cmd.needs:
        reason = need(ctx)
        if reason:
            return False, reason
    if cmd.milestone:
        if cmd.milestone == "TODO":
            later = _("还没有实现（见 TODO）")
        else:
            later = tr("{milestone} 接入", milestone=cmd.milestone)
        return False, f"{status} · {later}" if status else later
    return True, status


def _keys(cmd: Command) -> tuple[tuple[str, tuple[int, ...] | None], ...]:
    """The search keys in the current language: in a translated interface the shown name comes
    first (hits map onto it), then the Chinese name, its pinyin and the aliases (no hits)."""
    if language() == SOURCE:
        return cmd.search
    shown = label(cmd)
    if shown == cmd.name:
        return cmd.search
    return ((shown, tuple(range(len(shown)))), *((text, None) for text, _map in cmd.search))


def match(cmd: Command, query: str) -> tuple[int, list[tuple[int, int]]] | None:
    """Best fuzzy score over the name, its pinyin and its aliases (and the shown name, see
    :func:`_keys`); hits are mapped onto the shown name (:func:`label`)."""
    best: tuple[int, list[tuple[int, int]]] | None = None
    for k, (text, mapping) in enumerate(_keys(cmd)):
        got = score(query, text)
        if got is None:
            continue
        s, spans = got
        s -= 4 * k if k else 0  # prefer the visible name, then pinyin, then aliases
        if mapping is None:
            spans = []
        elif k:
            idx = sorted({mapping[i] for a, b in spans for i in range(a, b)})
            spans = []
            for i in idx:
                if spans and spans[-1][1] == i:
                    spans[-1] = (spans[-1][0], i + 1)
                else:
                    spans.append((i, i + 1))
        if best is None or s > best[0]:
            best = (s, spans)
    return best


__all__ = [
    "BUILTIN_COLORS",
    "COLOR_NAMES",
    "VISUAL_NAMES",
    "Command",
    "Context",
    "Need",
    "color_name",
    "evaluate",
    "label",
    "match",
    "registry",
]

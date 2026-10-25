"""Settings service (W7): the registry, validation, atomic persistence and change notification.

The registry below is the single source of truth for every frontend: names, descriptions,
options, ranges, defaults, which page and group a setting lives on, when it takes effect, and
whether it can be changed on this platform / in this build (UI-W7-27). Values are validated with
a pydantic model generated from the registry and stored as JSON in the config directory
(W7 待定 24). External edits of the file are picked up by :meth:`SettingsStore.poll`.
"""

from __future__ import annotations

import json
import os
import sys
import threading
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal

from loguru import logger
from pydantic import BaseModel, ConfigDict, Field, ValidationError, create_model

from spaceknife_protocol.settings import (
    TEXT_MAX,
    SettingEntry,
    SettingField,
    SettingMeta,
    SettingOption,
    SettingScalar,
    SettingsState,
    SettingValue,
)
from spaceknife_protocol.text import Text

from . import i18n, paths
from .i18n import Texts, _

FILE_VERSION = 1
LIST_ITEMS = 50  # a list setting's most entries when it does not say (the SDK's default too)
IS_WIN = sys.platform == "win32"
IS_MAC = sys.platform == "darwin"


@dataclass(frozen=True, slots=True)
class Spec:
    key: str
    category: str
    group: str
    name: str
    description: str
    control: str
    default: SettingValue
    options: tuple[tuple[SettingScalar, str] | tuple[SettingScalar, str, str], ...] = ()
    min: float | None = None
    max: float | None = None
    step: float | None = None
    unit: str = ""
    applies: str = "now"
    synonyms: tuple[str, ...] = ()
    reason: str = ""  # non-empty: shown disabled with this reason
    depends: str | None = None
    group_note: str = ""
    kind: type = field(default=str)
    # options worked out when asked for ("visualizations", "color_modes": plugins add some)
    options_from: str | None = None
    plugin: str | None = None  # a plugin's setting (key plugin.<id>.<name>, category plugins)
    texts: tuple[tuple[str, Text], ...] = ()  # a plugin's texts by field (name, description, group …)
    max_length: int | None = None  # text, textarea: the longest value (None: TEXT_MAX)
    fields: tuple[Spec, ...] = ()  # list: the fields of each entry (key: the field's name)
    max_items: int | None = None  # list: the most entries (None: LIST_ITEMS)

    def text(self, name: str, source: str) -> str:
        """A text field in the interface language: a plugin's own pair, else the message id."""
        for key, value in self.texts:
            if key == name:
                return value.get(i18n.language())
        return _(source) if source else source


# User-facing text, translated when shown (M7.5).
TEXT = Texts(
    {
        "invalid_value": "「{name}」的值不合法：{msg}",
        "invalid_entry": "「{name}」第 {n} 项：{msg}",
        "not_list": "「{name}」应是一个列表",
        "too_many": "「{name}」最多 {limit} 项",
        "unknown_field": "没有「{field}」这一栏",
        "not_entry": "每一项应是一组栏目的值",
        "unreadable": "无法读取设置文件：{reason}",
        "invalid_file": "设置文件不合法，继续使用上一次的有效设置：{reason}",
        "unknown": "未知设置：{keys}",
        "invalid_items": "设置文件里这些值不合法，已改用默认值：{reason}",
        "locked": "这些设置在当前版本不可修改：{names}",
        "conflict": "设置已被其他窗口修改（版本 {version}）",
    }
)
# the load levels' labels (message ids: settings.meta() translates the options when shown)
LOAD_LABELS = Texts(
    {
        "max": "极速",
        "balanced": "均衡 70%",
        "quiet": "安静 35%",
        "idle": "空闲",
        "custom": "自定义",
    }
)
LOAD_LEVELS: tuple[tuple[SettingScalar, str], ...] = tuple((k, LOAD_LABELS.raw(k)) for k in LOAD_LABELS)
# the dark theme's presets, which also name the slot: the light theme has its own colour for each
# (design/tokens color.accent.*), so the stored value stays the same when the theme changes
ACCENTS = ("#8B9CFF", "#D4FF3A", "#5EC8B8", "#F2A65A")


def _registry() -> tuple[Spec, ...]:
    S = Spec
    return (
        # ------------------------------------------------------------ 常规
        S(
            "general.language",
            "general",
            "语言",
            "界面语言",
            "切换后全部文字、日期与数字格式立即更换",
            "segmented",
            "system",
            (
                ("system", "跟随系统"),
                ("zh-CN", "简体中文"),  # named in its own language  # i18n: ignore
                ("en", "English"),
            ),
            synonyms=("language", "locale", "中文", "english"),  # i18n: ignore
            kind=str,
        ),
        S(
            "general.hover_keep_open",
            "general",
            "悬停提示",
            "切换元素时保持打开",
            "鼠标从一个块移到另一个块时，提示卡直接换成新内容",
            "switch",
            True,
            synonyms=("tooltip", "hover"),
            kind=bool,
        ),
        S(
            "general.hover_dates",
            "general",
            "悬停提示",
            "显示日期与时间",
            "提示卡显示创建、修改、访问时间",
            "switch",
            True,
            synonyms=("tooltip", "date", "time"),
            kind=bool,
        ),
        S(
            "general.hover_age",
            "general",
            "悬停提示",
            "显示文件年龄",
            "用「2 个月前」这样的相对时间显示",
            "switch",
            True,
            synonyms=("tooltip", "age", "relative"),
            kind=bool,
        ),
        S(
            "general.flash_on_done",
            "general",
            "窗口",
            "扫描结束时若窗口在后台则闪烁提醒",
            "Windows 任务栏闪烁、macOS 程序坞图标弹跳、Linux 窗口紧急提示",
            "switch",
            True,
            synonyms=("notify", "flash", "taskbar"),
            kind=bool,
        ),
        S(
            "general.log_auto_open",
            "general",
            "日志",
            "有新事件时自动打开日志面板",
            "收到警告或错误时自动展开日志面板；与日志面板里的开关是同一个设置",
            "switch",
            True,
            synonyms=("log", "console", "popup"),
            kind=bool,
        ),
        S(
            "general.log_scan_warnings",
            "general",
            "日志",
            "记录扫描警告",
            "例如「无法访问 …：拒绝访问」",
            "switch",
            True,
            synonyms=("log", "denied", "warning"),
            kind=bool,
        ),
        # ------------------------------------------------------------ 关于与更新 (the keys stay general.*)
        S(
            "general.update_check",
            "about",
            "更新",
            "自动检查更新",
            "每天最多一次，只读取 GitHub 上本项目的发布列表，不发送任何使用数据；小版本只在右上角提示",
            "switch",
            True,
            synonyms=("update", "upgrade", "release", "version", "升级", "新版本"),  # i18n: ignore
            kind=bool,
        ),
        S(
            "general.update_download",
            "about",
            "更新",
            "自动下载更新",
            "发现新版本后在后台下载并校验；只有你在更新卡片里点了「无缝更新」或安装方式才会安装，从不自己安装",
            "switch",
            True,
            depends="general.update_check",
            synonyms=("update", "download", "auto", "下载"),  # i18n: ignore
            kind=bool,
        ),
        S(
            "general.update_channel",
            "about",
            "更新",
            "提示哪些版本",
            "预览版是 GitHub 上标为预发布（Pre-release）的版本",
            "segmented",
            "preview",
            (("preview", "包括预览版"), ("stable", "只要正式版")),
            synonyms=("update", "channel", "beta", "prerelease", "预发布"),  # i18n: ignore
            kind=str,
        ),
        # ------------------------------------------------------------ 外观
        S(
            "appearance.theme",
            "appearance",
            "主题",
            "主题",
            "跟随系统时按系统的浅色 / 深色设置切换",
            "segmented",
            "system",
            (("system", "跟随系统"), ("light", "浅色"), ("dark", "深色")),
            synonyms=("theme", "dark", "light"),
            kind=str,
        ),
        S(
            "appearance.accent",
            "appearance",
            "主题",
            "强调色",
            "主要按钮、当前导航项、开关、滑块和焦点环",
            "color",
            ACCENTS[0],
            tuple((c, c) for c in ACCENTS),
            synonyms=("accent", "color", "colour"),
            kind=str,
        ),
        S(
            "appearance.reduce_motion",
            "appearance",
            "动画",
            "减少动画",
            "开启后缩放改为瞬时切换，扫描中的条纹不滚动",
            "segmented",
            "system",
            (("system", "跟随系统"), ("on", "开"), ("off", "关")),
            synonyms=("motion", "animation", "accessibility"),
            kind=str,
        ),
        S(
            "appearance.level_contrast",
            "appearance",
            "树图视觉",
            "层级对比",
            "相邻层级底色的差别（平 ↔ 深）",
            "slider",
            10,
            min=0,
            max=50,
            step=1,
            synonyms=("contrast", "depth"),
            kind=int,
        ),
        S(
            "appearance.border_contrast",
            "appearance",
            "树图视觉",
            "边框对比",
            "块边框的强弱（柔和 ↔ 锐利）",
            "slider",
            10,
            min=0,
            max=50,
            step=1,
            synonyms=("border",),
            kind=int,
        ),
        S(
            "appearance.halo_levels",
            "appearance",
            "树图视觉",
            "高亮光晕层数",
            "悬停或选中时点亮几层祖先标题栏（克制 ↔ 绚烂）",
            "slider",
            3,
            min=0,
            max=10,
            step=1,
            synonyms=("halo", "glow", "highlight"),
            kind=int,
        ),
        S(
            "appearance.hover_highlight",
            "appearance",
            "树图视觉",
            "悬停高亮",
            "鼠标下的元素加一圈内描边（深色主题白色，浅色主题近黑色）",
            "switch",
            True,
            synonyms=("hover", "highlight"),
            kind=bool,
        ),
        S(
            "appearance.mouse_trail",
            "appearance",
            "树图视觉",
            "鼠标拖尾",
            "鼠标划过的元素渐隐",
            "switch",
            False,
            synonyms=("trail",),
            kind=bool,
        ),
        S(
            "appearance.trash_effect",
            "appearance",
            "树图视觉",
            "回收站特效",
            "回收站 / 废纸篓里的东西蒙上一层暗纹、围上虚线，回收站本身标出清空后能释放多少；"
            "「动态」时虚线流动、偶尔扫过一道光（「减少动画」时不动）",
            "segmented",
            "animated",
            (("animated", "动态"), ("static", "静态"), ("off", "关")),
            synonyms=("trash", "recycle bin", "回收站", "废纸篓"),  # i18n: ignore
            kind=str,
        ),
        S(
            "appearance.selection_shadow",
            "appearance",
            "树图视觉",
            "选中投影",
            "选中元素带投影，更容易追踪",
            "switch",
            True,
            synonyms=("shadow", "selection"),
            kind=bool,
        ),
        S(
            "appearance.default_color_mode",
            "appearance",
            "配色样式",
            "树图默认配色",
            "与工具栏的「类别｜扁平｜时间｜分支」对应",
            "segmented",
            "class",
            (("class", "文件类别"), ("flat", "扁平"), ("time", "修改时间"), ("branch", "顶层分支")),
            applies="new_views",
            synonyms=("color", "flat", "classes", "branch"),
            kind=str,
            options_from="color_modes",
        ),
        S(
            "appearance.default_visual",
            "appearance",
            "可视化",
            "新视图默认可视化",
            "与工具栏的「树图｜火焰图｜火焰-极坐标图」对应",
            "segmented",
            "treemap",
            (("treemap", "树图"), ("flame", "火焰图"), ("flame_polar", "火焰-极坐标图")),
            applies="new_views",
            synonyms=("visualization", "treemap", "flame", "flame_polar", "polar"),
            kind=str,
            options_from="visualizations",
        ),
        S(
            "appearance.partition_color_mode",
            "appearance",
            "可视化",
            "火焰图和火焰-极坐标图默认配色",
            "新视图切到火焰图或火焰-极坐标图时使用",
            "segmented",
            "branch",
            (("class", "文件类别"), ("flat", "扁平"), ("time", "修改时间"), ("branch", "顶层分支")),
            applies="new_views",
            synonyms=("color", "flame", "polar", "branch"),
            kind=str,
            options_from="color_modes",
        ),
        # ------------------------------------------------------------ 树图
        S(
            "treemap.font_scale",
            "treemap",
            "几何",
            "元素字号",
            "标题栏、条和环的高度以及标签显示阈值随字号等比缩放",
            "slider",
            100,
            min=80,
            max=140,
            step=5,
            unit="%",
            synonyms=("font", "size"),
            kind=int,
        ),
        S(
            "treemap.min_element",
            "treemap",
            "几何",
            "最小元素尺寸",
            "小于这个尺寸的元素并入「N 个小项」聚合块",
            "slider",
            10,
            min=3,
            max=20,
            step=1,
            unit="px",
            synonyms=("minimum", "small items", "geometry"),
            kind=int,
        ),
        S(
            "treemap.aspect",
            "treemap",
            "几何",
            "元素比例",
            "只影响树图：偏高 ↔ 偏宽；0 表示尽量接近正方形",
            "slider",
            0,
            min=-60,
            max=60,
            step=5,
            synonyms=("aspect", "ratio"),
            kind=int,
        ),
        S(
            "treemap.initial_detail",
            "treemap",
            "几何",
            "初始细节层级",
            "新视图的细节层级，与工具栏「层级」一致",
            "slider",
            5,
            min=1,
            max=15,
            step=1,
            applies="new_views",
            synonyms=("detail", "depth", "level"),
            kind=int,
        ),
        S(
            "treemap.show_free",
            "treemap",
            "几何",
            "新视图显示可用空间",
            "整卷视图里显示可用空间块",
            "switch",
            True,
            applies="new_views",
            synonyms=("free space",),
            kind=bool,
        ),
        S(
            "treemap.show_unknown",
            "treemap",
            "几何",
            "新视图显示未知空间",
            "整卷视图里显示未知空间块",
            "switch",
            True,
            applies="new_views",
            synonyms=("unknown space",),
            kind=bool,
        ),
        S(
            "treemap.show_trash",
            "treemap",
            "几何",
            "新视图显示回收站",
            "扫描里有回收站 / 废纸篓时显示它；关掉后回收站不占画面，其余的块放大填满",
            "switch",
            True,
            applies="new_views",
            synonyms=("trash", "recycle bin", "回收站", "废纸篓"),  # i18n: ignore
            kind=bool,
        ),
        S(
            "treemap.sort_by_size",
            "treemap",
            "几何",
            "按大小排序",
            "同一文件夹内大块排在左上",
            "switch",
            True,
            synonyms=("sort", "order"),
            kind=bool,
        ),
        S(
            "treemap.flame_origin",
            "treemap",
            "火焰图",
            "火焰图方向",
            "根所在的一侧",
            "segmented",
            "bottom",
            (("top", "根在上"), ("bottom", "根在下")),
            synonyms=("flame", "direction", "orientation", "root", "icicle"),
            kind=str,
        ),
        S(
            "treemap.zoom_ms",
            "treemap",
            "动画",
            "缩放动画时长",
            "双击放大或返回时的过渡时长；0 表示无动画",
            "slider",
            250,
            min=0,
            max=600,
            step=25,
            unit="ms",
            synonyms=("animation", "zoom", "duration"),
            kind=int,
        ),
        S(
            "treemap.fps",
            "treemap",
            "动画",
            "动画帧率",
            "动画重绘的频率",
            "segmented",
            "display",
            (("display", "跟随显示器"), ("60", "60 fps"), ("30", "30 fps")),
            synonyms=("fps", "frame rate"),
            kind=str,
        ),
        S(
            "treemap.dynazoom",
            "treemap",
            "动画",
            "缩放时实时重排（DynaZoom）",
            "缩放过程中元素实时重新排列",
            "switch",
            False,
            reason="DynaZoom 还没有实现（见 TODO）",
            synonyms=("dynazoom",),
            kind=bool,
        ),
        S(
            "treemap.dynasmooth",
            "treemap",
            "动画",
            "按交互调节帧率（DynaSmooth）",
            "只在操作时使用高帧率，更省电",
            "switch",
            True,
            synonyms=("dynasmooth",),
            kind=bool,
        ),
        S(
            "treemap.flash_changes",
            "treemap",
            "变化提示",
            "元素变化闪烁",
            "扫描中或文件变化时元素闪烁提示",
            "switch",
            True,
            synonyms=("flash", "blink", "changes"),
            kind=bool,
        ),
        # ------------------------------------------------------------ 扫描与性能
        S(
            "scan.method",
            "scan",
            "扫描方式",
            "默认扫描方式",
            "自动与快速 MFT：整个 NTFS 卷用 MFT 快速扫描（需要时请求一次管理员授权，拒绝则改用硬扫描），"
            "其他位置硬扫描",
            "segmented",
            "auto",
            (
                ("auto", "自动"),
                ("mft", "快速 MFT", "" if IS_WIN else "只在 Windows 的 NTFS 卷上可用"),
                ("hard", "硬扫描"),
            ),
            applies="next_scan",
            synonyms=("mft", "fast", "hard", "method"),
            kind=str,
        ),
        S(
            "scan.elevate",
            "scan",
            "扫描方式",
            "需要时请求管理员权限",
            "仅启动只读扫描助手，主程序不提权；关闭后没有管理员权限时直接改用硬扫描",
            "switch",
            True,
            reason="" if IS_WIN else "只在 Windows 的 NTFS 卷上可用",
            synonyms=("admin", "uac", "elevate", "helper"),
            kind=bool,
        ),
        S(
            "scan.load_manual",
            "scan",
            "磁盘负载上限与并发",
            "手动扫描默认",
            "从启动页开始的扫描与重新扫描",
            "segmented",
            "balanced",
            LOAD_LEVELS,
            applies="next_scan",
            group_note="限制扫描造成的磁盘读取占用",
            synonyms=("load", "limit", "throttle", "io"),
            kind=str,
        ),
        S(
            "scan.load_incremental",
            "scan",
            "磁盘负载上限与并发",
            "增量重扫默认",
            "实时监控发现变化后的定点重扫",
            "segmented",
            "quiet",
            LOAD_LEVELS,
            group_note="限制扫描造成的磁盘读取占用",
            # nothing reads this yet: the monitor's rescans wait for the running scan's governor only
            reason="还没有接入：实时监控的定点重扫不按这一档限速",
            synonyms=("load", "incremental"),
            kind=str,
        ),
        S(
            "scan.custom_limit",
            "scan",
            "磁盘负载上限与并发",
            "自定义上限",
            "选择「自定义」档位时使用",
            "slider",
            70,
            min=5,
            max=100,
            step=5,
            unit="%",
            applies="next_scan",
            group_note="限制扫描造成的磁盘读取占用",
            synonyms=("custom", "limit"),
            kind=int,
        ),
        S(
            "scan.idle_low_io",
            "scan",
            "磁盘负载上限与并发",
            "空闲时使用系统低 I/O 优先级",
            "「空闲」档位使用系统低 I/O 优先级，不与前台程序抢磁盘",
            "switch",
            True,
            applies="next_scan",
            group_note="限制扫描造成的磁盘读取占用",
            synonyms=("io priority", "idle", "background"),
            kind=bool,
        ),
        S(
            "scan.yield_when_busy",
            "scan",
            "磁盘负载上限与并发",
            "整盘繁忙时让路",
            "其他程序让整块磁盘超过上限时，扫描降到最低进度",
            "switch",
            True,
            applies="next_scan",
            group_note="限制扫描造成的磁盘读取占用",
            synonyms=("yield", "busy"),
            kind=bool,
        ),
        S(
            "scan.concurrency",
            "scan",
            "磁盘负载上限与并发",
            "最大并发",
            "自动：按处理器核数选择；手动：按下面的数值",
            "segmented",
            "auto",
            (("auto", "自动"), ("manual", "手动")),
            applies="next_scan",
            group_note="限制扫描造成的磁盘读取占用",
            synonyms=("concurrency", "threads", "workers", "parallel"),
            kind=str,
        ),
        S(
            "scan.workers",
            "scan",
            "磁盘负载上限与并发",
            "手动并发数",
            "手动时最多同时列出几个文件夹（没有原生扩展时是工作进程数）",
            "number",
            4,
            min=1,
            max=64,
            step=1,
            applies="next_scan",
            depends="scan.concurrency=manual",
            group_note="限制扫描造成的磁盘读取占用",
            synonyms=("concurrency", "workers"),
            kind=int,
        ),
        S(
            "scan.ads",
            "scan",
            "扫描范围",
            "扫描 NTFS 备用数据流",
            "MFT 快速扫描时把附加在文件上的隐藏数据流计入大小（硬扫描不读取数据流）",
            "switch",
            False,
            applies="next_scan",
            reason="只在 Windows 上可用" if not IS_WIN else "",
            synonyms=("ads", "alternate data stream", "stream"),
            kind=bool,
        ),
        S(
            "scan.cross_mounts",
            "scan",
            "扫描范围",
            "跨越挂载点",
            "进入挂载在文件夹下的其他卷；关闭时只统计当前卷",
            "switch",
            False,
            applies="next_scan",
            synonyms=("mount", "mount point", "volume"),
            kind=bool,
        ),
        S(
            "scan.protected_places",
            "scan",
            "扫描范围",
            "受保护的位置",
            "没有「完全磁盘访问权限」时，macOS 会为桌面、文稿、下载、云盘、照片图库和"
            "其他应用的数据逐个弹窗询问",
            "segmented",
            "ask",
            options=(
                ("ask", "扫描前询问"),
                ("skip", "跳过，不弹窗"),
                ("allow", "照常访问"),
            ),
            applies="next_scan",
            reason="" if IS_MAC else "只在 macOS 上需要",
            synonyms=("full disk access", "privacy", "tcc", "permission", "prompt", "完全磁盘访问"),
            kind=str,
        ),
        S(
            "scan.hardlinks",
            "scan",
            "统计口径",
            "硬链接计算",
            "只计一次时，合计与磁盘实际占用一致",
            "segmented",
            "once",
            (("once", "只计一次"), ("each", "每个链接都计")),
            applies="next_scan",
            synonyms=("hardlink", "hard link", "inode"),
            kind=str,
        ),
        S(
            "scan.unit_base",
            "scan",
            "统计口径",
            "大小单位",
            "按 1000 计时，778.6 GB 显示为 836.0 GB",
            "segmented",
            "system",
            (("system", "跟随系统"), ("1024", "1024"), ("1000", "1000")),
            synonyms=("unit", "1024", "1000", "kib", "gib", "base"),
            kind=str,
        ),
        S(
            "scan.monitor",
            "scan",
            "实时监控",
            "实时监控",
            (
                "使用 USN 日志 + ReadDirectoryChangesEx，变化的文件会闪烁提示"
                if IS_WIN
                else "使用 FSEvents，变化的文件会闪烁提示"
                if IS_MAC
                else "使用 inotify，变化的文件会闪烁提示"
            ),
            "switch",
            True,
            synonyms=("monitor", "watch", "usn", "inotify", "fsevents", "hooks"),
            kind=bool,
        ),
        S(
            "scan.coalesce_ms",
            "scan",
            "实时监控",
            "事件合并间隔",
            "此时间内的连续变化合并为一次刷新",
            "number",
            150,
            min=50,
            max=5000,
            step=10,
            unit="ms",
            depends="scan.monitor",
            synonyms=("debounce", "coalesce", "interval"),
            kind=int,
        ),
        # ------------------------------------------------------------ 标签
        S(
            "tags.name_red",
            "tags",
            "显示名",
            "红色标签 ▲",
            "显示名出现在详情面板和悬停卡里；过滤语法仍写 :red",
            "text",
            "",
            synonyms=("tag", "red", "label"),
            kind=str,
        ),
        S(
            "tags.name_yellow",
            "tags",
            "显示名",
            "黄色标签 ■",
            "过滤语法仍写 :yellow",
            "text",
            "",
            synonyms=("tag", "yellow"),
            kind=str,
        ),
        S(
            "tags.name_green",
            "tags",
            "显示名",
            "绿色标签 ●",
            "过滤语法仍写 :green",
            "text",
            "",
            synonyms=("tag", "green"),
            kind=str,
        ),
        S(
            "tags.name_blue",
            "tags",
            "显示名",
            "蓝色标签 ◆",
            "过滤语法仍写 :blue",
            "text",
            "",
            synonyms=("tag", "blue"),
            kind=str,
        ),
        S(
            "tags.omit_ctrl",
            "tags",
            "快捷键",
            "树图有焦点时可省略 Ctrl",
            "直接按 1–4 打标签、按 0 清除",
            "switch",
            True,
            synonyms=("shortcut", "ctrl", "tag key"),
            kind=bool,
        ),
        S(
            "tags.remember",
            "tags",
            "保存",
            "记住标签",
            "按扫描根路径保存标签，重新扫描或打开快照后恢复",
            "switch",
            True,
            synonyms=("persist", "save tags"),
            kind=bool,
        ),
        # ------------------------------------------------------------ 快捷键
        S(
            "keys.preset",
            "keys",
            "键位预设",
            "键位预设",
            "改了任意一个键后显示为「自定义」",
            "segmented",
            "modern",
            (("modern", "现代"), ("classic", "SpaceSniffer 经典")),
            reason="改键还没有实现（见 TODO）；下表列出当前键位",
            synonyms=("keymap", "shortcuts", "hotkeys", "classic"),
            kind=str,
        ),
        # ------------------------------------------------------------ 高级
        S(
            "advanced.log_level",
            "advanced",
            "日志",
            "日志文件级别",
            "写入日志文件的最低级别",
            "segmented",
            "INFO",
            (("INFO", "信息"), ("DEBUG", "调试")),
            synonyms=("log", "debug", "verbose"),
            kind=str,
        ),
        S(
            "advanced.auto_snapshot",
            "advanced",
            "缓存与快照",
            "扫描后自动保存快照",
            "供启动页的最近记录和快照对比使用",
            "switch",
            True,
            applies="next_scan",
            synonyms=("snapshot", "cache", "autosave"),
            kind=bool,
        ),
        S(
            "advanced.snapshot_keep",
            "advanced",
            "缓存与快照",
            "保留最近快照数",
            "更早的快照每周只保留一份",
            "number",
            10,
            min=1,
            max=100,
            step=1,
            synonyms=("snapshot", "retention", "keep"),
            kind=int,
        ),
        S(
            "advanced.map_large_scans",
            "advanced",
            "内存",
            "大扫描存进映射文件",
            "开启后，节点表超过 256 MB（约 200 万项）的扫描存进磁盘映射文件，不占私有内存；"
            "代价是扫完后每项约写盘 200 字节",
            "switch",
            False,  # the writes: docs/memory.md §3
            applies="next_scan",
            synonyms=("memory", "ram", "mmap", "large", "内存映射"),  # i18n: ignore
            kind=bool,
        ),
        S(
            "advanced.plugin_dev_mode",
            "advanced",
            "插件",
            "开发者模式",
            "插件代码可以在后端进程内运行（没有隔离），也可以链接开发中的插件文件夹；只在开发插件时打开",
            "switch",
            False,
            applies="now",
            synonyms=("plugin", "developer", "debug", "dev mode", "插件开发"),  # i18n: ignore
            kind=bool,
        ),
        S(
            "advanced.memory_budget",
            "advanced",
            "内存",
            "扫描数据内存上限",
            "扫描数据超过物理内存的这个比例后，新的节点表存进磁盘映射文件：不占私有提交，常用部分仍由系统留在内存里",
            "number",
            25,
            min=5,
            max=90,
            step=5,
            unit="%",
            applies="next_scan",
            synonyms=("memory", "ram", "mmap", "budget", "swap", "内存映射"),  # i18n: ignore
            kind=int,
        ),
    )


REGISTRY: tuple[Spec, ...] = _registry()
BY_KEY: dict[str, Spec] = {s.key: s for s in REGISTRY}
DEFAULTS: dict[str, SettingValue] = {s.key: s.default for s in REGISTRY}


_DYNAMIC_ID = r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$"


def _field(spec: Spec, *, aliased: bool = True) -> tuple[Any, Any]:
    alias = spec.key if aliased else None
    if spec.options_from is not None:  # checked against the options of the moment (dynamic_ok)
        return str, Field(spec.default, alias=alias, max_length=64, pattern=_DYNAMIC_ID)
    if spec.options and spec.control in ("segmented",):
        values = tuple(o[0] for o in spec.options)
        return Literal[values], Field(spec.default, alias=alias)
    if spec.kind is bool:
        return bool, Field(spec.default, alias=alias)
    if spec.kind is int:
        return int, Field(spec.default, alias=alias, ge=spec.min, le=spec.max)
    if spec.kind is float:  # plugins' fractional numbers (an int is a float too)
        return float | int, Field(spec.default, alias=alias, ge=spec.min, le=spec.max)
    if spec.control == "color":
        return str, Field(spec.default, alias=alias, pattern=r"^#[0-9A-Fa-f]{6}$")
    return str, Field(spec.default, alias=alias, max_length=spec.max_length or TEXT_MAX)


Values: type[BaseModel] = create_model(  # type: ignore[call-overload]
    "SettingsValues",
    __config__=ConfigDict(extra="ignore", populate_by_name=False, strict=True),
    **{s.key.replace(".", "__"): _field(s) for s in REGISTRY},
)


class SettingsFile(BaseModel):
    version: int = FILE_VERSION
    values: dict[str, Any] = Field(default_factory=dict)  # each checked on its own (SettingsStore._split)


class SettingsError(ValueError):
    pass


class ConflictError(SettingsError):
    pass


def dynamic_options(spec: Spec) -> list[tuple[SettingScalar, str]]:
    """The options of an ``options_from`` setting now: the built-in ones, then the plugins'."""
    from .extensions import EXTENSIONS
    from .layout import registry as visuals

    out: list[tuple[SettingScalar, str]] = [(o[0], o[1]) for o in spec.options]
    known = {o[0] for o in out}
    if spec.options_from == "visualizations":
        for v in visuals.all_visuals():
            if v.id not in known:
                out.append((v.id, v.title))
    elif spec.options_from == "color_modes":
        for c in EXTENSIONS.of("color_mode"):
            if c.id not in known:
                out.append((c.id, c.spec.title.get(i18n.language())))
    return out


def dynamic_ok(spec: Spec, value: SettingValue) -> bool:
    return spec.options_from is None or value in [o[0] for o in dynamic_options(spec)]


def validate(values: dict[str, Any]) -> dict[str, SettingValue]:
    """Full, validated values (unknown keys dropped, missing keys defaulted)."""
    try:
        model = Values.model_validate(values)
    except ValidationError as exc:
        err = exc.errors()[0]
        key = str(err["loc"][0]) if err["loc"] else "?"
        name = _(BY_KEY[key].name) if key in BY_KEY else key
        raise SettingsError(TEXT["invalid_value"].format(name=name, msg=err["msg"])) from exc
    out: dict[str, SettingValue] = model.model_dump(by_alias=True)
    for key, value in out.items():
        spec = BY_KEY[key]
        if spec.options_from is not None and not dynamic_ok(spec, value):
            raise SettingsError(TEXT["invalid_value"].format(name=_(spec.name), msg=f"{value!r}"))
    return out


_ITEM_MODELS: dict[tuple[str, int], type[BaseModel]] = {}


def _validate_list(spec: Spec, value: Any) -> list[SettingEntry]:
    """A ``list`` setting's value: at most ``max_items`` entries, each with exactly its fields (a
    missing one gets the field's default), each value checked as that field."""
    name = spec.text("name", spec.name)
    if not isinstance(value, list):
        raise SettingsError(TEXT["not_list"].format(name=name))
    limit = spec.max_items or LIST_ITEMS
    if len(value) > limit:
        raise SettingsError(TEXT["too_many"].format(name=name, limit=limit))
    keys = {f.key for f in spec.fields}
    out: list[SettingEntry] = []
    for n, entry in enumerate(value, 1):
        if not isinstance(entry, dict):
            raise SettingsError(TEXT["invalid_entry"].format(name=name, n=n, msg=TEXT["not_entry"]))
        unknown = sorted(str(k) for k in set(entry) - keys)
        if unknown:
            msg = TEXT["unknown_field"].format(field=unknown[0])
            raise SettingsError(TEXT["invalid_entry"].format(name=name, n=n, msg=msg))
        row: SettingEntry = {}
        for f in spec.fields:
            try:
                row[f.key] = validate_item(f, entry.get(f.key, f.default))  # type: ignore[assignment]
            except SettingsError as exc:
                raise SettingsError(TEXT["invalid_entry"].format(name=name, n=n, msg=str(exc))) from exc
        out.append(row)
    return out


def validate_item(spec: Spec, value: Any) -> SettingValue:
    """One value of ``spec``; SettingsError says what is wrong with it."""
    if spec.control == "list":
        return _validate_list(spec, value)
    key = (spec.key, id(spec))
    model = _ITEM_MODELS.get(key)
    if model is None:
        typ, info = _field(spec, aliased=False)
        model = create_model("SettingItem", __config__=ConfigDict(extra="forbid", strict=True), v=(typ, info))
        _ITEM_MODELS[key] = model
    try:
        got = model.model_validate({"v": value}).v  # type: ignore[attr-defined]
    except ValidationError as exc:
        err = exc.errors()[0]
        raise SettingsError(
            TEXT["invalid_value"].format(name=spec.text("name", spec.name), msg=err["msg"])
        ) from exc
    if spec.options and spec.options_from is None and spec.plugin is not None:
        allowed = [o[0] for o in spec.options]
        # multi: the chosen values joined by commas, each one of the options (in their order)
        if spec.control == "multi" and isinstance(got, str):
            parts = [x for x in got.split(",") if x]
            if all(x in allowed for x in parts):
                return ",".join(v for v in allowed if v in parts)
        if spec.control == "multi" or got not in allowed:
            raise SettingsError(
                TEXT["invalid_value"].format(name=spec.text("name", spec.name), msg=f"{got!r}")
            )
    return got  # type: ignore[no-any-return]


def plugin_specs() -> list[Spec]:
    """The settings of the enabled plugins (``setting`` contributions), in registry order."""
    from .extensions import EXTENSIONS

    return [c.impl for c in EXTENSIONS.of("setting") if isinstance(c.impl, Spec)]


def plugin_spec(key: str) -> Spec | None:
    from .extensions import EXTENSIONS

    if not key.startswith("plugin."):
        return None
    c = EXTENSIONS.get("setting", key[len("plugin.") :])
    return c.impl if c is not None and isinstance(c.impl, Spec) else None


def _options(s: Spec) -> list[SettingOption]:
    if s.options_from is not None:
        dyn = dynamic_options(s)
        builtin = {o[0] for o in s.options}
        return [
            SettingOption(value=v, label=_(label) if v in builtin and label else label) for v, label in dyn
        ]
    if s.plugin is not None:
        return [
            SettingOption(value=o[0], label=s.text(f"option:{i}", str(o[1]))) for i, o in enumerate(s.options)
        ]
    return [
        SettingOption(
            value=o[0],
            label=_(o[1]) if o[1] else o[1],
            reason=_(o[2]) if len(o) > 2 and o[2] else "",
        )
        for o in s.options
    ]


def meta() -> list[SettingMeta]:
    """The settings' metadata in the current language (M7.5); the Chinese name stays searchable.
    The plugins' settings follow the built-in ones (category ``plugins``, grouped by plugin)."""
    source = i18n.language() == i18n.SOURCE
    out = []
    for s in (*REGISTRY, *plugin_specs()):
        out.append(
            SettingMeta(
                key=s.key,
                category=s.category,  # type: ignore[arg-type]
                group=s.text("group", s.group),
                group_note=s.text("group_note", s.group_note) if s.group_note else "",
                name=s.text("name", s.name),
                description=s.text("description", s.description) if s.description else "",
                control=s.control,  # type: ignore[arg-type]
                options=_options(s),
                min=s.min,
                max=s.max,
                step=s.step,
                unit=s.text("unit", s.unit) if s.unit else "",
                applies=s.applies,  # type: ignore[arg-type]
                synonyms=list(s.synonyms) if source else [*s.synonyms, s.name],
                enabled=not s.reason,
                reason=_(s.reason) if s.reason else "",
                depends=s.depends,
                plugin=s.plugin,
                placeholder=s.text("placeholder", "") if s.plugin else "",
                max_length=s.max_length,
                fields=[_field_meta(f) for f in s.fields],
                max_items=(s.max_items or LIST_ITEMS) if s.control == "list" else None,
            )
        )
    return out


def _field_meta(f: Spec) -> SettingField:
    """A field of a list setting's entries, in the interface language."""
    return SettingField(
        name=f.key,
        title=f.text("name", f.name),
        description=f.text("description", f.description) if f.description else "",
        control=f.control,  # type: ignore[arg-type]
        default=f.default,  # type: ignore[arg-type]
        options=_options(f),
        min=f.min,
        max=f.max,
        step=f.step,
        unit=f.text("unit", f.unit) if f.unit else "",
        placeholder=f.text("placeholder", ""),
        max_length=f.max_length,
    )


def system_unit_base() -> Literal[1000, 1024]:
    """'跟随系统' (W7 待定 7): Windows 1024, macOS 1000, Linux GNOME 1000, other desktops 1024."""
    if IS_MAC:
        return 1000
    if sys.platform.startswith("linux") and "gnome" in os.environ.get("XDG_CURRENT_DESKTOP", "").lower():
        return 1000
    return 1024


class SettingsStore:
    """The settings document: thread-safe reads, validated patches, atomic saves, change listeners."""

    def __init__(self, file: Path | None = None) -> None:
        self.file = file or paths.config_dir() / "settings.json"
        self._lock = threading.Lock()
        self.version = 0
        self.values: dict[str, SettingValue] = dict(DEFAULTS)
        # plugin.<id>.<name> of the enabled plugins (missing ones: their defaults)
        self.plugin_values: dict[str, SettingValue] = {}
        # what the file holds that is not in use now — keys of plugins that are off, unknown keys,
        # values of dynamic options a plugin offered (its visualization) — written back unchanged
        self.foreign: dict[str, Any] = {}
        self._error: tuple[str, str] | None = None  # (TEXT key, reason): translated when shown
        self._mtime: float | None = None
        self.listeners: dict[str, Callable[[SettingsStore], None]] = {}
        self._load()

    # ------------------------------------------------------------ persistence

    def _load(self) -> bool:
        try:
            stat = self.file.stat()
        except FileNotFoundError:
            self._mtime = None
            return False
        except OSError as exc:
            self._error = ("unreadable", str(exc.strerror or exc))
            return False
        self._mtime = stat.st_mtime
        try:
            doc = SettingsFile.model_validate(json.loads(self.file.read_text(encoding="utf-8")))
        except (ValueError, ValidationError) as exc:
            self._error = ("invalid_file", str(exc))
            logger.warning("settings file {} is invalid: {}", self.file, exc)
            return False
        values, plugin_values, foreign, bad = self._split(doc.values)
        changed = values != self.values or plugin_values != self.plugin_values
        self.values, self.plugin_values, self.foreign = values, plugin_values, foreign
        self._error = ("invalid_items", _("、").join(bad)) if bad else None
        if bad:
            logger.warning("settings file {}: invalid values reset to their defaults: {}", self.file, bad)
        return changed

    def _split(
        self, raw: dict[str, Any]
    ) -> tuple[dict[str, SettingValue], dict[str, SettingValue], dict[str, Any], list[str]]:
        """Each value on its own: a bad one falls back to its default (the others stay); plugin keys
        of plugins that are not here, and unknown keys, are kept aside as they are."""
        values = dict(DEFAULTS)
        plugin_values: dict[str, SettingValue] = {}
        foreign: dict[str, Any] = {}
        bad: list[str] = []
        for key, value in raw.items():
            spec = BY_KEY.get(key) or plugin_spec(key)
            if spec is None:
                foreign[key] = value
                continue
            try:
                got = validate_item(spec, value)
            except SettingsError:
                bad.append(spec.text("name", spec.name))
                continue
            if not dynamic_ok(spec, got):
                foreign[key] = value  # a plugin's visualization or colour mode: back when it is
                continue
            if spec.plugin is None:
                values[key] = got
            else:
                plugin_values[key] = got
        return values, plugin_values, foreign, bad

    def plugins_changed(self) -> None:
        """Plugins came or went: their stored values move between in use and kept aside."""
        with self._lock:
            raw = {**self.foreign, **self.plugin_values}
            raw.update({k: v for k, v in self.values.items() if v != DEFAULTS[k]})
            values, plugin_values, foreign, _bad = self._split(raw)
            changed = values != self.values or plugin_values != self.plugin_values
            self.values, self.plugin_values, self.foreign = values, plugin_values, foreign
            if changed:
                self.version += 1
        if changed:
            self._notify()

    def _save(self) -> None:
        self.file.parent.mkdir(parents=True, exist_ok=True)
        changed: dict[str, Any] = {k: v for k, v in self.values.items() if v != DEFAULTS[k]}
        changed.update(self.foreign)
        for key, value in self.plugin_values.items():
            spec = plugin_spec(key)
            if spec is None or value != spec.default:
                changed[key] = value
        doc = SettingsFile(values=dict(sorted(changed.items())))
        tmp = self.file.with_suffix(".tmp")
        tmp.write_text(json.dumps(doc.model_dump(), ensure_ascii=False, indent=2), encoding="utf-8")
        os.replace(tmp, self.file)
        self._mtime = self.file.stat().st_mtime

    def poll(self) -> bool:
        """Reload after an external edit of the file (UI-W7-25). True if the values changed."""
        try:
            mtime: float | None = self.file.stat().st_mtime
        except OSError:
            mtime = None
        if mtime == self._mtime:
            return False
        with self._lock:
            changed = self._load()
            if changed:
                self.version += 1
        if changed or self.error:
            self._notify()
        return changed

    @property
    def error(self) -> str:
        """What is wrong with the settings file, in the current language ("" when nothing is)."""
        if self._error is None:
            return ""
        key, reason = self._error
        return TEXT[key].format(reason=reason)

    # ------------------------------------------------------------ access

    def get(self, key: str) -> SettingScalar:
        """A setting that holds one value (every built-in one); a plugin's list setting raises
        TypeError (read it with :meth:`get_value`)."""
        value = self.get_value(key)
        if isinstance(value, list):
            raise TypeError(f"{key} holds a list of entries: read it with get_value")
        return value

    def get_value(self, key: str) -> SettingValue:
        """Any setting's value, a plugin's list setting too (its entries)."""
        if key in self.values:
            return self.values[key]
        if key in self.plugin_values:
            return self.plugin_values[key]
        spec = plugin_spec(key)
        if spec is None:
            raise KeyError(key)
        return spec.default

    def has(self, key: str) -> bool:
        return key in self.values or plugin_spec(key) is not None

    def plugin_settings(self, plugin: str) -> dict[str, SettingValue]:
        """A plugin's values by local name (defaults included): what its code sees as ctx.settings."""
        prefix = f"plugin.{plugin}."
        out: dict[str, SettingValue] = {}
        for spec in plugin_specs():
            if spec.key.startswith(prefix):
                value = self.plugin_values.get(spec.key, spec.default)
                # a list: the plugin's own copy (its code may change it without changing the setting)
                out[spec.key[len(prefix) :]] = [dict(e) for e in value] if isinstance(value, list) else value
        return out

    def update(self, patch: dict[str, SettingValue], version: int | None = None) -> None:
        specs = {k: BY_KEY.get(k) or plugin_spec(k) for k in patch}
        unknown = [k for k, s in specs.items() if s is None]
        if unknown:
            raise SettingsError(TEXT["unknown"].format(keys=", ".join(unknown)))
        locked = [
            _(BY_KEY[k].name)
            for k, v in patch.items()
            if k in BY_KEY and BY_KEY[k].reason and v != self.values[k]
        ]
        if locked:
            raise SettingsError(TEXT["locked"].format(names=_("、").join(locked)))
        builtin = {k: v for k, v in patch.items() if k in BY_KEY}
        plugin = {k: validate_item(specs[k], v) for k, v in patch.items() if k not in BY_KEY}  # type: ignore[arg-type]
        with self._lock:
            if version is not None and version != self.version:
                raise ConflictError(TEXT["conflict"].format(version=self.version))
            values = validate({**self.values, **builtin})
            plugin_values = {**self.plugin_values, **plugin}
            if values == self.values and plugin_values == self.plugin_values:
                return
            for k in builtin:
                self.foreign.pop(k, None)
            self.values, self.plugin_values = values, plugin_values
            self.version += 1
            self._save()
        self._notify()

    def reset(self, category: str | None) -> None:
        keys = [s.key for s in REGISTRY if category is None or s.category == category]
        with self._lock:
            values = {**self.values, **{k: DEFAULTS[k] for k in keys}}
            plugin_values = {} if category in (None, "plugins") else self.plugin_values
            if values == self.values and plugin_values == self.plugin_values:
                return
            self.values, self.plugin_values = values, plugin_values
            self.version += 1
            self._save()
        self._notify()

    def unit_base(self) -> Literal[1000, 1024]:
        v = self.values["scan.unit_base"]
        return 1000 if v == "1000" else 1024 if v == "1024" else system_unit_base()

    def state(self) -> SettingsState:
        specs = plugin_specs()
        return SettingsState(
            version=self.version,
            values={**self.values, **{s.key: self.plugin_values.get(s.key, s.default) for s in specs}},
            defaults={**DEFAULTS, **{s.key: s.default for s in specs}},
            meta=meta(),
            path=str(self.file),
            logs=str(paths.logs_dir()),
            snapshots=str(paths.snapshots_dir()),
            error=self.error,
            unit_base=self.unit_base(),
            language=i18n.resolve(str(self.values["general.language"])),
        )

    def _notify(self) -> None:
        for listener in list(self.listeners.values()):
            try:
                listener(self)
            except Exception:
                logger.exception("settings listener failed")


__all__ = [
    "BY_KEY",
    "DEFAULTS",
    "REGISTRY",
    "ConflictError",
    "SettingsError",
    "SettingsStore",
    "Spec",
    "validate",
]

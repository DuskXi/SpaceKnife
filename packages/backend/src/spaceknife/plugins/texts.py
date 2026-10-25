"""What the enable dialog says about each permission (in the interface language)."""

from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path
from typing import Any

from spaceknife_protocol.plugins import ContributionGroup, PermissionInfo

from ..i18n import Texts, _

PERMISSION_TEXT = Texts(
    {
        "tree.read": "读取扫描结果：文件名、路径、大小和时间",
        "files.read": "读取扫描范围内文件的内容",
        "snapshots.read": "读取保存的快照",
        "basket.propose": "把文件加入删除篮（真正删除仍需你确认）",
        "net": "连接网络：{hosts}",
        "exec": "运行外部程序：{programs}",
        "host.open": "打开或定位文件、打开终端、打开网页",
        "clipboard": "写入剪贴板",
    }
)


KIND_TEXT = Texts(
    {
        "command": "命令",
        "keybinding": "按键",
        "setting": "设置",
        "report": "报告",
        "node_field": "详情字段",
        "insight": "洞察（代码）",
        "panel": "网页面板",
        "visualization": "可视化",
        "color_mode": "配色",
        "export_format": "导出格式",
        "export_template": "导出模板",
        "insight_rules": "洞察规则文件",
        "file_class": "文件类别",
        "folder_rule": "文件夹规则",
        "node_set": "节点集合（过滤）",
        "filter_preset": "过滤预设",
        "cleanup": "清理",
        "scan_source": "扫描来源",
        "watcher": "监控器",
        "change_annotator": "变化说明",
        "hook": "钩子",
        "icon": "图标",
        "method": "方法",
        "event": "事件",
        "state": "共享状态",
        "stream": "数据流",
    }
)
KIND_ORDER = [
    "command",
    "keybinding",
    "setting",
    "report",
    "panel",
    "node_field",
    "insight",
    "insight_rules",
    "visualization",
    "color_mode",
    "export_format",
    "export_template",
    "file_class",
    "folder_rule",
    "node_set",
    "filter_preset",
    "cleanup",
    "scan_source",
    "watcher",
    "change_annotator",
    "hook",
    "icon",
    "method",
    "event",
    "state",
    "stream",
]
MAX_ITEMS = 50


def _item(kind: str, spec: dict[str, Any], lang: str, titles: dict[str, str]) -> str:
    title = spec.get("title")
    shown = ""
    if isinstance(title, dict) and title.get("zh"):
        shown = str(title.get("en") or title["zh"]) if lang == "en" else str(title["zh"])
    name = str(spec.get("name") or "")
    if kind == "keybinding":
        cmd = str(spec.get("command", ""))
        return f"{spec.get('key', '')} → {titles.get(cmd, cmd)}"
    if kind == "folder_rule":
        return f"{spec.get('glob', '')} → {spec.get('class', '')}"
    if kind == "hook":
        return str(spec.get("event", name))
    if kind == "insight_rules":
        return str(spec.get("file", ""))
    if kind == "watcher":
        source = str(spec.get("source", ""))
        return shown or _("监控「{source}」").format(source=titles.get("source:" + source, source))
    return shown or name


def contribution_groups(desc: Any, lang: str, folder: Path | None = None) -> list[ContributionGroup]:
    """What a plugin's description adds, grouped by kind, in a fixed order (the enable dialog and
    ``plugin enable``). Its code's contributions count as much as its manifest's; with ``folder``,
    the rule files and icons it carries by convention (insights/*.toml, icons/*.svg) too."""
    by_kind: dict[str, list[str]] = {}
    if folder is not None:
        for kind, sub, pattern in (("insight_rules", "insights", "*.toml"), ("icon", "icons", "*.svg")):
            if (folder / sub).is_dir():
                by_kind[kind] = [f"{sub}/{f.name}" for f in sorted((folder / sub).glob(pattern))]
    titles: dict[str, str] = {}  # a key's command or filter preset, a watcher's source: by title
    for c in desc.contributions:
        kind = c.get("kind")
        if kind in ("command", "filter_preset", "scan_source"):
            spec = c.get("spec") or {}
            t = spec.get("title") or {}
            text = str((t.get("en") if lang == "en" else None) or t.get("zh", ""))
            key = str(spec.get("name", ""))
            titles["source:" + key if kind == "scan_source" else key] = text
    for c in desc.contributions:
        kind = str(c.get("kind", ""))
        item = _item(kind, c.get("spec") or {}, lang, titles)
        if kind in ("insight_rules", "icon") and item in by_kind.get(kind, []):
            continue
        by_kind.setdefault(kind, []).append(item)
    by_kind = {k: v for k, v in by_kind.items() if v}
    routed = {"report", "cleanup"}
    methods = [m.name for m in desc.methods if m.kind not in routed and m.public]
    command_methods = {
        (c.get("spec") or {}).get("action", {}).get("method")
        for c in desc.contributions
        if c.get("kind") == "command"
    }
    methods = [m for m in methods if m not in command_methods]
    if methods:
        by_kind["method"] = methods
    for kind, names in (("event", desc.events), ("state", desc.states), ("stream", desc.streams)):
        if names:
            by_kind[kind] = [x.name for x in names]
    order = {k: i for i, k in enumerate(KIND_ORDER)}
    out = []
    for kind in sorted(by_kind, key=lambda k: order.get(k, 99)):
        items = by_kind[kind]
        out.append(
            ContributionGroup(
                kind=kind, title=KIND_TEXT.get(kind, kind), count=len(items), items=items[:MAX_ITEMS]
            )
        )
    return out


def summary_lines(groups: Sequence[ContributionGroup], width: int = 8) -> list[str]:
    """The groups as text lines: "报告 (1): 重复文件", long lists cut ("… 47 more")."""
    out = []
    for g in groups:
        shown = [i for i in g.items if i][:width]
        more = g.count - len(shown)
        tail = _("……还有 {n} 项").format(n=more) if more > 0 and shown else ""
        body = _("、").join(shown) + (f" {tail}" if tail else "")
        out.append(f"{g.title} ({g.count})" + (f": {body}" if body else ""))
    return out


def permission_infos(
    perms: Sequence[str], hosts: Sequence[str], programs: Sequence[str]
) -> list[PermissionInfo]:
    out = []
    for p in perms:
        warn = False
        if p == "net":
            if "*" in hosts:
                text, warn = _("可以连接任意网络地址"), True
            else:
                text = PERMISSION_TEXT["net"].format(hosts=_("、").join(hosts))
        elif p == "exec":
            text, warn = PERMISSION_TEXT["exec"].format(programs=_("、").join(programs)), True
        else:
            text = PERMISSION_TEXT.get(p, p)
        out.append(PermissionInfo(id=p, text=text, warn=warn))  # type: ignore[arg-type]
    return out


__all__ = ["KIND_TEXT", "PERMISSION_TEXT", "contribution_groups", "permission_infos", "summary_lines"]

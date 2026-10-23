"""Write the generated tables of the plugin API reference (docs/plugin-api.md) from the code.

Source of truth (hand-edited):
    packages/sdk/src/spaceknife_sdk/            the objects plugin code gets and returns (MODELS)
    packages/protocol/src/spaceknife_protocol/  the report, action and field models
    packages/backend/src/spaceknife/plugins/allow.py   the built-in methods ctx.call may call
    packages/backend/src/spaceknife/palette/commands.py  the app's commands (their ids)
    design/icons/icons.json                     the app's icon names

Generated (never edit by hand; the tests run ``--check``), between
``<!-- generated:<name>:start -->`` and ``<!-- generated:<name>:end -->`` of docs/plugin-api.md:
``plugin-models`` (every field of every object), ``plugin-core`` (ctx.call / sk.core),
``plugin-commands`` (command ids for actions and keys), ``plugin-icons`` (icon names).

Usage::

    uv run python tools/gen_plugin_api.py          # rewrite the generated blocks
    uv run python tools/gen_plugin_api.py --check  # exit 1 if any block is out of date
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import re
import sys
import types
import typing
from collections.abc import Sequence
from pathlib import Path
from typing import Any

from pydantic import BaseModel

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "plugin-api.md"
ICONS = ROOT / "design" / "icons" / "icons.json"


def _models() -> list[tuple[str, type, str]]:
    """(group, class, what it is) of every object plugin code is handed or hands back."""
    import spaceknife_sdk as sdk
    from spaceknife_protocol import plugins as pp

    return [
        ("上下文", sdk.ScanInfo, "`ctx.scans()` 的一项（新的在前）；`scan_done` `scan_closed` 钩子的参数"),
        ("上下文", sdk.SnapshotInfo, "`ctx.snapshots.list()` 的一项（新的在前）"),
        ("上下文", sdk.NodeRef, "带路径的节点引用：`tree.ref(id)`；报告、字段、动作里的节点"),
        ("上下文", sdk.Node, "`tree.node(id)`；字段、洞察函数的 `node` 参数"),
        ("上下文", sdk.ReportContext, "报告、清理的第二个参数：调用发生在哪"),
        ("上下文", sdk.ActionContext, "`call` 动作调的方法收到的参数"),
        ("输入", sdk.VisualInput, "代码可视化 `(v) -> 权重数组`"),
        ("输入", sdk.ColorInput, "代码配色 `(c) -> 下标数组`"),
        ("输入", sdk.ExportInput, "导出格式 `(x) -> Iterator[str]`"),
        ("输入", sdk.ExportInfo, "导出格式旧写法 `(tree, root, match, info)` 的第四个参数"),
        ("输入", sdk.ChangeUnit, "对比原因 `(ctx, items)` 的一项"),
        ("返回", sdk.Report, "报告"),
        ("返回", sdk.Toast, "命令或方法返回它时显示一条提示"),
        ("返回", sdk.FieldValue, "节点字段（也可以只返回文字）"),
        ("返回", sdk.Insight, "代码洞察"),
        ("返回", sdk.Proposal, "清理的一个候选"),
        ("返回", sdk.Change, "监控器产出的一项变化"),
        ("报告的部件", sdk.TableColumn, "`r.col(key, 标题, 单位)`"),
        ("报告的部件", sdk.TableMore, "`r.more(cursor)`：表格的下一页"),
        ("报告的部件", sdk.StatItem, "`r.stats` 的一项"),
        ("报告的部件", sdk.BarItem, "`r.bars` 的一项（元组、dict 或它）"),
        ("报告的部件", sdk.PartItem, "`r.stack` / `r.ring` 的一段"),
        ("报告的部件", pp.LabeledAction, "`r.action(标签, icon, **动作)`：按钮"),
        ("报告的部件", sdk.ThemedColor, "分主题的颜色 `{ light, dark }`"),
    ]


# what a field means, in the docs' language (else the field's own description from the code)
NOTES: dict[tuple[str, str], str] = {
    ("ScanInfo", "scan_id"): "扫描的 id：`ctx.scan(scan_id)` 读它的树",
    ("ScanInfo", "roots"): "根的路径（扫描来源的扫描：根的名字）",
    ("ScanInfo", "items"): "文件夹和文件的个数，不算根",
    ("ScanInfo", "source"): "`scan`（磁盘扫描）、`snapshot`（打开的快照）或扫描来源的全名",
    ("ScanInfo", "state"): "扫描中、完成、停止、失败",
    ("ScanInfo", "started_at"): "开始时间，unix 秒",
    ("ScanInfo", "finished_at"): "结束时间，unix 秒；还在扫描时是 None",
    ("ScanInfo", "snapshot"): "从哪个快照文件打开的",
    ("ScanInfo", "front"): "用户眼前的那个扫描（`ctx.tree` 默认读它）",
    ("ScanInfo", "on_disk"): "节点是这台机器上的文件；扫描来源（`on_disk = false`）的是 False",
    ("SnapshotInfo", "path"): "快照文件；`ctx.snapshots.open(path)` 打开它",
    ("SnapshotInfo", "created_at"): "保存的时间（UTC）",
    ("SnapshotInfo", "roots"): "扫描的根",
    ("SnapshotInfo", "node_count"): "节点数（含虚拟的 0 号节点和根）",
    ("SnapshotInfo", "bytes_alloc"): "占用合计（字节）",
    ("SnapshotInfo", "files"): "文件数",
    ("SnapshotInfo", "file_bytes"): "快照文件本身的大小",
    ("SnapshotInfo", "method"): "怎么读的：`hard`（逐个文件夹）`mft`（NTFS 快速扫描）",
    ("NodeRef", "scan_id"): "扫描",
    ("NodeRef", "node"): "节点下标",
    ("NodeRef", "epoch"): "取下标时树的纪元：和 `tree.epoch` 不同时，宿主按 `path` 重新找",
    ("NodeRef", "path"): "节点的路径（系统自己的写法）；空时只能按下标找",
    ("Node", "id"): "节点下标（0 是虚拟的顶层节点，根从 1 起）",
    ("Node", "name"): "名字",
    ("Node", "path"): "完整路径，系统自己的分隔符（Windows 是 `\\`）；只要相对路径用 `tree.rel_path`",
    ("Node", "kind"): "`Kind.FILE` `DIR` `LINK` `OTHER`",
    ("Node", "size"): "逻辑大小；文件夹是子树合计",
    ("Node", "alloc"): "占用；文件夹是子树合计",
    ("Node", "files"): "子树里的文件数（文件自己是 1）",
    ("Node", "dirs"): "子树里的文件夹数",
    ("Node", "mtime"): "修改时间，unix 秒（0：不知道）",
    ("Node", "ctime"): "创建时间（Linux 是状态改变时间）",
    ("Node", "atime"): "访问时间",
    ("Node", "ext"): "扩展名，小写，不带点",
    ("Node", "parent"): "父节点下标（0 号节点是 −1）",
    ("Node", "depth"): "层级：0 号节点是 0，根是 1，根下面一层是 2",
    ("Node", "child_count"): "直接子项的个数",
    ("Node", "flags"): "`Flag` 的组合：`HIDDEN` `HARDLINK_DUP` `PENDING` …",
    ("Node", "cls"): "文件类别的 id（`video`、插件的 `<id>.<名字>`）；没有类别是空",
    ("ReportContext", "scan_id"): "所在的扫描（`ctx.tree` 读它）",
    ("ReportContext", "view_id"): "所在的视图（有的话）",
    ("ReportContext", "node"): "针对的节点：详情面板里是选中项（没选时是缩放根），右键菜单里是点的那一项",
    ("ReportContext", "selection"): "选中的节点（≤ 1000 个）",
    ("ReportContext", "compare_id"): "对比视图里的对比",
    ("ReportContext", "cursor"): "表格续读时 `r.more(cursor)` 给的位置；第一页是 None",
    ("ReportContext", "args"): '打开它的动作带的参数（`{ open = "report:x", args = {…} }`）',
    ("ActionContext", "scan_id"): "所在的扫描",
    ("ActionContext", "view_id"): "所在的视图",
    ("ActionContext", "node"): "针对的节点（右键菜单：点的那一项；其他地方：选中项）",
    ("ActionContext", "selection"): "选中的节点",
    ("ActionContext", "compare_id"): "对比",
    ("ActionContext", "args"): "动作里写的 `args`",
    ("VisualInput", "tree"): "这一帧所在的树",
    ("VisualInput", "options"): "声明的 `options`（本插件的设置名 → 视图里的值）",
    ("VisualInput", "ctx"): "调用的上下文：`v.settings` `v.cancelled` `v.check()` 也有",
    ("ColorInput", "tree"): "树",
    ("ColorInput", "ids"): "这一帧画出的节点（int64 数组）",
    ("ColorInput", "palette"): "色板长度：返回的下标 < 它",
    ("ColorInput", "ctx"): "调用的上下文",
    ("ExportInput", "tree"): "树",
    ("ExportInput", "root"): "要导出的子树：视图所在的位置、选中的项，或 0（全部）",
    ("ExportInput", "match"): "过滤后的布尔掩码（按节点下标）；没过滤时是 None",
    ("ExportInput", "filter"): "过滤式；没过滤时是空",
    ("ExportInput", "options"): "导出对话框里选的值（没给的取默认；`spaceknife run` 用默认）",
    ("ExportInput", "selection"): "选中的节点（「选中项」范围）",
    ("ExportInput", "ctx"): "调用的上下文",
    ("ExportInfo", "filter"): "过滤式",
    ("ExportInfo", "options"): "选项",
    ("ChangeUnit", "path"): "变化项的路径",
    ("ChangeUnit", "kind"): "`file` 或 `dir`",
    ("ChangeUnit", "status"): "`added` `removed` `changed` …",
    ("ChangeUnit", "delta"): "大小的变化（字节，可以是负的）",
    ("ChangeUnit", "base"): "旧快照里的大小",
    ("ChangeUnit", "target"): "新快照里的大小",
    ("Report", "title"): "标题（标签页、对话框显示在外面）",
    ("Report", "subtitle"): "标题下的一行",
    ("Report", "blocks"): "块（`r.*` 构造）",
    ("Report", "actions"): "报告底部的按钮",
    ("Report", "footnote"): "末尾的小字",
    ("Report", "empty"): "没有块时显示它",
    ("Toast", "text"): "文字（≤ 512 字）",
    ("Toast", "tone"): "色调",
    ("FieldValue", "value"): "值（文字 ≤ 200 字）",
    ("FieldValue", "unit"): "怎么格式化：`size` 写成 1.2 MB，`count` 加千分位，`ago` 写成多久以前 …",
    ("FieldValue", "tone"): "颜色（1.5 节的写法，或 `palette:<配色全名>#<下标>`）",
    ("FieldValue", "node"): "点击时定位到它",
    ("FieldValue", "action"): "旁边的一个按钮",
    ("Insight", "title"): "卡片标题：这是什么",
    ("Insight", "reason"): "为什么这么说",
    ("Insight", "kind"): "类型（`cache` `build` `log` …，洞察配色按它上色）",
    ("Insight", "cleanup"): "能不能删：`safe`（随便删）`tool`（用工具清）`review`（先看看）`keep`（别删）",
    ("Insight", "app"): "相关的程序",
    ("Insight", "note"): "补充说明",
    ("Insight", "command"): "清理命令：只显示、可复制，从不运行",
    ("Proposal", "node"): "节点下标、`NodeRef` 或绝对路径",
    ("Proposal", "reason"): "复核对话框里每一项的原因",
    ("Proposal", "checked"): "默认勾选",
    ("Change", "path"): "相对来源根的路径，用 `/`；空表示整个来源",
    ("Change", "kind"): "变化的种类（`overflow`：太多了，整个重跑）",
    ("TableColumn", "key"): "行里的键",
    ("TableColumn", "label"): "列标题",
    ("TableColumn", "unit"): "单位（格式化和排序方式）",
    ("TableColumn", "align"): "对齐（数字默认靠右）",
    ("TableColumn", "grow"): "宽度份数",
    ("TableMore", "method"): "再调哪个报告（线上名 `plugin.<id>.<报告名>`；`r.more` 自动填）",
    ("TableMore", "cursor"): "交给 `c.cursor` 的位置",
    ("StatItem", "hint"): "悬停提示",
    ("StatItem", "delta"): "变化量（对比报告里显示成 +/−）",
    ("BarItem", "node"): "点击时定位",
    ("LabeledAction", "action"): "动作，写法同命令的 `action`",
    ("ThemedColor", "light"): "浅色主题里的颜色 `#RRGGBB`",
    ("ThemedColor", "dark"): "深色主题里的颜色",
}


def _limits(info: Any, typ: str) -> str:
    """A field's bounds as the docs write them (≤ 120 字, ≤ 1000 个, ≥ 1)."""
    out = []
    count = "≤ {} 字" if typ.startswith("str") else "≤ {} 个"
    for m in getattr(info, "metadata", ()):
        for attr, fmt in (
            ("max_length", count),
            ("ge", "≥ {}"),
            ("le", "≤ {}"),
            ("gt", "> {}"),
            ("lt", "< {}"),
        ):
            v = getattr(m, attr, None)
            if v is not None:
                out.append(fmt.format(v))
    return "，".join(out)


# names the reference does not list (the SDK's machinery, not data) — the test keeps this complete
NOT_DATA = frozenset({"Plugin", "Context", "TreeView", "Rows", "Columns", "Listing", "Params", "Payload"})
NOT_DATA |= {"Kind", "Flag", "T", "Text", "Cancelled", "PluginError", "layout", "r"}

_PREFIX = re.compile(
    r"\b(?:typing|types|datetime|pathlib|numpy|collections\.abc|spaceknife_[a-z_]+(?:\.[a-z_]+)*)\."
)


def type_name(t: Any) -> str:
    """A field's type as an author reads it: ``int | None``, ``list[str]``, ``str | Text``."""
    if t is None or t is type(None):
        return "None"
    origin = typing.get_origin(t)
    if origin is typing.Annotated:
        base, *meta = typing.get_args(t)
        if any("localize" in repr(m) or "as_text" in repr(m) for m in meta):
            return "str | Text"
        return type_name(base)
    if origin is typing.Literal:
        return " ".join(f"`{a}`" if isinstance(a, str) else repr(a) for a in typing.get_args(t)).join("()")
    if origin in (typing.Union, types.UnionType):
        return " | ".join(type_name(a) for a in typing.get_args(t))
    if origin is not None:
        args = typing.get_args(t)
        name = getattr(origin, "__name__", str(origin))
        return f"{name}[{', '.join(type_name(a) for a in args)}]" if args else name
    if isinstance(t, type):
        return t.__name__
    if isinstance(t, typing.TypeAliasType):
        return t.__name__
    return _PREFIX.sub("", repr(t)).replace("'", "")


def _default(value: Any) -> str:
    if value is dataclasses.MISSING or value is ...:
        return "必需"
    if isinstance(value, str):
        return f'`"{value}"`'
    if isinstance(value, BaseModel):
        return "—"
    return f"`{value!r}`"


def model_rows(cls: type) -> list[tuple[str, str, str, str]]:
    """(field, type, default, description) of a pydantic model or a dataclass."""
    rows: list[tuple[str, str, str, str]] = []
    if isinstance(cls, type) and issubclass(cls, BaseModel):
        hints = typing.get_type_hints(cls, include_extras=True)
        for name, info in cls.model_fields.items():
            if info.is_required():
                default = "必需"
            elif info.default_factory is not None:
                made = info.default_factory()  # type: ignore[call-arg]
                default = f"`{made!r}`" if made in ([], {}, ()) else "—"
            else:
                default = _default(info.default)
            note = NOTES.get((cls.__name__, name)) or info.description or ""
            typ = type_name(hints.get(name, info.annotation))
            limit = _limits(info, typ)
            rows.append(
                (
                    info.alias or name,
                    typ,
                    default,
                    f"{note}（{limit}）" if note and limit else note or limit,
                )
            )
        return rows
    if dataclasses.is_dataclass(cls):
        import spaceknife_sdk as sdk

        # names its module imports only for type checking (Context in models.py)
        hints = typing.get_type_hints(cls, localns={n: getattr(sdk, n) for n in sdk.__all__})
        for f in dataclasses.fields(cls):
            if f.default is not dataclasses.MISSING:
                default = _default(f.default)
            elif f.default_factory is not dataclasses.MISSING:
                default = f"`{f.default_factory()!r}`"
            else:
                default = "必需"
            rows.append(
                (f.name, type_name(hints.get(f.name, f.type)), default, NOTES.get((cls.__name__, f.name), ""))
            )
        return rows
    raise TypeError(f"{cls!r} is neither a pydantic model nor a dataclass")


def _cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


def models_block() -> str:
    out: list[str] = []
    group = ""
    for g, cls, what in _models():
        if g != group:
            out += [f"### {g}", ""]
            group = g
        out += [
            f"#### `{cls.__name__}`",
            "",
            what + "。",
            "",
            "| 字段 | 类型 | 默认 | 说明 |",
            "|---|---|---|---|",
        ]
        for name, typ, default, desc in model_rows(cls):
            out.append(f"| `{name}` | {_cell(typ)} | {_cell(default)} | {_cell(desc)} |")
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def core_block() -> str:
    from spaceknife.plugins.allow import ALLOWED
    from spaceknife_protocol.methods import METHODS

    out = ["| 方法 | 权限 | 参数 | 结果 |", "|---|---|---|---|"]
    for name in sorted(ALLOWED):
        spec = METHODS.get(name)
        perm = f"`{ALLOWED[name]}`" if ALLOWED[name] else "—"
        if spec is None:
            out.append(f"| `{name}` | {perm} | — | — |")
            continue
        params = " ".join(f"`{f.alias or n}`" for n, f in spec.params.model_fields.items()) or "—"
        out.append(f"| `{name}` | {perm} | {params} | `{spec.result.__name__}` |")
    return "\n".join(out) + "\n"


def commands_block() -> str:
    from spaceknife import i18n
    from spaceknife.palette.commands import registry

    en = i18n.catalog("en")
    out = ["| 命令 id | 名称 | Name |", "|---|---|---|"]
    for c in sorted((c for c in registry() if c.owner == "builtin"), key=lambda c: c.id):
        out.append(f"| `{c.id}` | {_cell(c.name)} | {_cell(en.get(c.name, c.name))} |")
    return "\n".join(out) + "\n"


def icons_block() -> str:
    names = sorted(json.loads(ICONS.read_text(encoding="utf-8"))["icons"])
    return " ".join(f"`{n}`" for n in names) + "\n"


BLOCKS = {
    "plugin-models": models_block,
    "plugin-core": core_block,
    "plugin-commands": commands_block,
    "plugin-icons": icons_block,
}


def markers(name: str) -> tuple[str, str]:
    return f"<!-- generated:{name}:start -->", f"<!-- generated:{name}:end -->"


def replace_block(text: str, name: str, body: str) -> str:
    start, end = markers(name)
    i, j = text.find(start), text.find(end)
    if i < 0 or j < i:
        raise SystemExit(f"{DOC}: no {start} … {end}")
    return text[: i + len(start)] + "\n\n" + body.rstrip() + "\n\n" + text[j:]


def wanted() -> str:
    text = DOC.read_text(encoding="utf-8")
    for name, make in BLOCKS.items():
        text = replace_block(text, name, make())
    return text


def main(argv: Sequence[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="exit 1 if docs/plugin-api.md is out of date")
    args = ap.parse_args(argv)
    text = wanted()
    if args.check:
        if DOC.read_text(encoding="utf-8") != text:
            print(f"{DOC.relative_to(ROOT)} is out of date: uv run python tools/gen_plugin_api.py")
            return 1
        return 0
    DOC.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {DOC.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

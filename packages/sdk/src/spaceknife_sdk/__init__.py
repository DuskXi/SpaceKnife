"""SpaceKnife plugin SDK: what a plugin imports. See docs/plugin-dev.md.

>>> from spaceknife_sdk import Context, Plugin, Report, ReportContext, T, r
>>> plugin = Plugin()                       # reads plugin.toml next to this file
>>> @plugin.report(T("又大又旧的文件", "Big old files"))
... async def files(ctx: Context, c: ReportContext) -> Report:
...     rows = ctx.tree.query(">100mb;m>1y", kind="file", columns=("bytes", "path"), sort="-bytes")
...     return Report(blocks=[r.table([r.col("bytes", T("占用", "Size"), "size"),
...                                    r.col("path", T("位置", "Location"), "path")], rows=rows)])
"""

from __future__ import annotations

from spaceknife_protocol.base import Params, Payload
from spaceknife_protocol.errors import ErrorCode, RequestError
from spaceknife_protocol.plugins import (
    ActionContext,
    BarItem,
    FieldValue,
    NodeRef,
    PartItem,
    Report,
    ReportContext,
    StatItem,
    TableColumn,
    TableMore,
    ThemedColor,
    Toast,
)
from spaceknife_protocol.text import T, Text

from . import layout
from . import report as r
from .context import Context, FileNotAllowed
from .errors import Cancelled, PluginError
from .models import (
    Change,
    ChangeUnit,
    ColorInput,
    ExportInfo,
    ExportInput,
    Insight,
    Listing,
    Proposal,
    ScanInfo,
    SnapshotInfo,
    VisualInput,
)
from .plugin import Plugin
from .tree import Columns, Flag, Kind, Node, Rows, TreeView

__version__ = "0.1.0"

__all__ = [
    "ActionContext",
    "BarItem",
    "Cancelled",
    "Change",
    "ChangeUnit",
    "ColorInput",
    "Columns",
    "Context",
    "ErrorCode",
    "ExportInfo",
    "ExportInput",
    "FieldValue",
    "FileNotAllowed",
    "Flag",
    "Insight",
    "Kind",
    "Listing",
    "Node",
    "NodeRef",
    "Params",
    "PartItem",
    "Payload",
    "Plugin",
    "PluginError",
    "Proposal",
    "Report",
    "ReportContext",
    "RequestError",
    "Rows",
    "ScanInfo",
    "SnapshotInfo",
    "StatItem",
    "T",
    "TableColumn",
    "TableMore",
    "Text",
    "ThemedColor",
    "Toast",
    "TreeView",
    "VisualInput",
    "layout",
    "r",
]

"""The CSV, JSON and HTML exports, declared with the SDK like any plugin's export format.

A shipped plugin runs in the app's own process, so ``tree.store`` is the scan's store and the
formats are the app's own writers (``spaceknife.export.formats``): what they write is byte for byte
what it was before they were a plugin. A plugin of your own writes its rows from ``tree.query`` /
``tree.walk`` instead (docs/plugin-dev.md 「导出格式」).
"""

from __future__ import annotations

from collections.abc import Iterator

import numpy as np

from spaceknife_sdk import ExportInfo, Plugin, T, TreeView

plugin = Plugin()


@plugin.export_format(
    "csv",
    T("CSV 表格", "CSV table"),
    description=T(
        "每个文件和文件夹一行：路径、大小、占用、修改时间等，可用 Excel 打开",
        "One row per file and folder: path, size, size on disk, modified time …; opens in Excel",
    ),
    extension="csv",
    media_type="text/csv",
    bom=True,
)
def csv(tree: TreeView, root: int, match: np.ndarray | None, info: ExportInfo) -> Iterator[str]:
    from spaceknife.export.formats import iter_csv

    return iter_csv(tree.store, root, match=match)


@plugin.export_format(
    "json",
    T("JSON 数据", "JSON data"),
    description=T("整棵树的结构化数据，给脚本和其他程序读", "The whole tree as structured data, for scripts"),
    extension="json",
    media_type="application/json",
)
def json(tree: TreeView, root: int, match: np.ndarray | None, info: ExportInfo) -> Iterator[str]:
    from spaceknife.export.formats import iter_json

    return iter_json(tree.store, root, match=match, filter_text=info.filter)


@plugin.export_format(
    "html",
    T("HTML 报告", "HTML report"),
    description=T(
        "一个可以在浏览器里打开、展开和折叠的文件夹报告",
        "A folder report to open, expand and fold in a browser",
    ),
    extension="html",
    media_type="text/html",
)
def html(tree: TreeView, root: int, match: np.ndarray | None, info: ExportInfo) -> Iterator[str]:
    from spaceknife.export.formats import iter_html

    return iter_html(tree.store, root, match=match, filter_text=info.filter)

"""CSV, JSON and HTML exports of a tree, beside the template language (docs/export-templates.md).

All three walk the tree the way templates do (:class:`~.template.ExportTree`: depth first, the
largest first, a filter mask honoured), so a filtered export shows exactly what the filtered view
shows. CSV and JSON stream (``iter_csv`` / ``iter_json`` yield pieces); the HTML report is bounded
(a few levels, the largest children, the largest files and extensions), so it stays small and
readable for any tree, and is self-contained: inline CSS, no scripts, no external assets.

The data formats use English keys (a stable schema for tools); the report's text is Chinese and
lives in :data:`TEXT`.
"""

from __future__ import annotations

import csv
import html
import io
import json
import time
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from datetime import UTC, datetime
from functools import lru_cache
from json.encoder import encode_basestring

import numpy as np

from .. import __version__, fmt
from ..store.nodes import Flag, Kind, NodeStore
from .template import Element, ExportTree, Ordering, export_roots, local_time

# User-facing text of the HTML report (M7.5 extracts it for translation).
TEXT: dict[str, str] = {
    "title": "空间报告",
    "roots": "{count} 个位置",
    "generated": "生成于 {time}",
    "filter": "过滤：{expr}",
    "disk": "占用空间",
    "logical": "逻辑大小",
    "files": "文件",
    "folders": "文件夹",
    "matched": "匹配过滤的文件",
    "structure": "文件夹结构",
    "structure_note": "每层按占用从大到小，最多 {top} 项、{depth} 层",
    "largest": "最大的文件",
    "extensions": "按扩展名",
    "col_name": "名称",
    "col_share": "占比",
    "col_disk": "占用",
    "col_files": "文件数",
    "col_modified": "修改于",
    "col_path": "路径",
    "col_ext": "扩展名",
    "no_ext": "（无扩展名）",
    "rest": "其余 {count} 项",
    "empty": "没有内容",
    "footer": "由 SpaceKnife {version} 生成 · 大小均为占用空间 · {base}",
    "base_1024": "1 KB = 1024 字节",
    "base_1000": "1 KB = 1000 字节",
    "csv": "CSV 表格",
    "json": "JSON 数据",
    "html": "HTML 报告",
}

CSV_COLUMNS = (
    "path",
    "name",
    "type",
    "depth",
    "size",
    "disk_size",
    "files",
    "folders",
    "modified",
    "created",
    "accessed",
    "attributes",
)
KIND_NAMES = {Kind.FILE: "file", Kind.DIR: "folder", Kind.LINK: "link", Kind.OTHER: "other"}
ATTRIBUTES: tuple[tuple[Flag, str], ...] = (
    (Flag.READONLY, "readonly"),
    (Flag.HIDDEN, "hidden"),
    (Flag.SYSTEM, "system"),
    (Flag.ARCHIVE, "archive"),
    (Flag.COMPRESSED, "compressed"),
    (Flag.ENCRYPTED, "encrypted"),
    (Flag.SPARSE, "sparse"),
    (Flag.TEMPORARY, "temporary"),
    (Flag.OFFLINE, "offline"),
    (Flag.NOT_INDEXED, "notindexed"),
    (Flag.REPARSE, "reparse"),
    (Flag.CLOUD, "cloud"),
    (Flag.HARDLINK_DUP, "hardlink"),
    (Flag.ACCESS_DENIED, "denied"),
    (Flag.ERROR, "error"),
    (Flag.MOUNT, "mount"),
    (Flag.PENDING, "pending"),
)
REPORT_ORDER = Ordering(blocks="mixed", sort="size", descending=True)  # as the treemap shows it
JSON_FORMAT = "spaceknife-export"
JSON_VERSION = 1


_ATTR_BITS = tuple((int(flag), name) for flag, name in ATTRIBUTES)
_KINDS = ("file", "folder", "link", "other")  # by Kind value
BATCH = 4096  # elements whose columns are read at once


def _exact_time(ts: int) -> str:
    try:
        return datetime.fromtimestamp(ts, UTC).astimezone().isoformat(timespec="seconds")
    except (OSError, OverflowError, ValueError):  # outside what the platform's local time handles
        return datetime.fromtimestamp(ts, UTC).isoformat(timespec="seconds")


def iso_time(ts: int) -> str:
    """ISO 8601 local time with its offset (``2026-10-18T12:30:00+08:00``); "" when unknown."""
    return _exact_time(ts) if ts > 0 else ""


class LocalTimes:
    """:func:`iso_time` for many timestamps at once.

    The zone offset is looked up once per UTC day (at its first and last second); a day on which
    it changes (a daylight saving switch) is converted one timestamp at a time.
    """

    def __init__(self) -> None:
        self._days: dict[int, tuple[int, str] | None] = {}

    def _zone(self, day: int) -> tuple[int, str] | None:
        if day not in self._days:
            zone: tuple[int, str] | None = None
            try:
                first = datetime.fromtimestamp(day * 86400, UTC).astimezone()
                last = datetime.fromtimestamp(day * 86400 + 86399, UTC).astimezone()
            except (OSError, OverflowError, ValueError):
                pass
            else:
                offset = first.utcoffset()
                if offset is not None and offset == last.utcoffset():
                    zone = (int(offset.total_seconds()), first.isoformat()[19:])  # "+08:00"
            self._days[day] = zone
        return self._days[day]

    def many(self, stamps: np.ndarray) -> list[str]:
        values = stamps.astype(np.int64)
        out = [""] * len(values)
        known = np.flatnonzero(values > 0)
        if not len(known):
            return out
        t = values[known]
        days, which = np.unique(t // 86400, return_inverse=True)
        zones = [self._zone(int(d)) for d in days.tolist()]
        offsets = np.array([z[0] if z is not None else 0 for z in zones], dtype=np.int64)[which]
        text = np.datetime_as_string((t + offsets).astype("datetime64[s]"), unit="s").tolist()
        for k, stamp, local, z in zip(known.tolist(), t.tolist(), text, which.tolist(), strict=True):
            zone = zones[z]
            out[k] = local + zone[1] if zone is not None else _exact_time(stamp)
        return out


@lru_cache(maxsize=4096)
def _attributes(flags: int) -> tuple[str, ...]:
    return tuple(name for bit, name in _ATTR_BITS if flags & bit)


def attributes(flags: Flag | int) -> list[str]:
    return list(_attributes(int(flags)))


@lru_cache(maxsize=4096)
def _attributes_json(flags: int) -> str:
    return json.dumps(list(_attributes(flags)))


@dataclass(slots=True)
class _Batch:
    """Columns of a run of elements, as Python lists (one numpy gather per batch, not per element)."""

    elements: list[Element]
    kind: list[str]
    flags: list[int]
    size: list[int]
    alloc: list[int]
    files: list[int]
    dirs: list[int]
    mtime: list[str]
    ctime: list[str]
    atime: list[str]


def _batches(tree: ExportTree, times: LocalTimes, size: int = BATCH) -> Iterator[_Batch]:
    def gather(elements: list[Element]) -> _Batch:
        pos = np.fromiter((e.pos for e in elements), dtype=np.int64, count=len(elements))
        c = tree.columns(pos)
        kinds = np.minimum(c["kind"], len(_KINDS) - 1).tolist()
        return _Batch(
            elements,
            [_KINDS[k] for k in kinds],
            c["flags"].tolist(),
            c["size"].tolist(),
            c["alloc"].tolist(),
            c["files"].tolist(),
            c["dirs"].tolist(),
            times.many(c["mtime"]),
            times.many(c["ctime"]),
            times.many(c["atime"]),
        )

    run: list[Element] = []
    for el in tree.walk():
        run.append(el)
        if len(run) >= size:
            yield gather(run)
            run = []
    if run:
        yield gather(run)


# ---------------------------------------------------------------------------------------- CSV


def iter_csv(store: NodeStore, root: int, *, match: np.ndarray | None = None) -> Iterator[str]:
    """RFC 4180 CSV (CRLF rows, a header row), one row per element, depth first; one piece per batch."""
    buf = io.StringIO()
    writer = csv.writer(buf, lineterminator="\r\n")
    writer.writerow(CSV_COLUMNS)
    times = LocalTimes()
    for r in export_roots(store, root):
        tree = ExportTree(store, r, match=match, ordering=REPORT_ORDER)
        for b in _batches(tree, times):
            for i, el in enumerate(b.elements):
                folder = el.is_dir
                writer.writerow(
                    (
                        el.path,
                        el.name,
                        b.kind[i],
                        el.level,
                        b.size[i],
                        b.alloc[i],
                        b.files[i] if folder else "",
                        b.dirs[i] if folder else "",
                        b.mtime[i],
                        b.ctime[i],
                        b.atime[i],
                        " ".join(_attributes(b.flags[i])),
                    )
                )
            yield buf.getvalue()
            buf.seek(0)
            buf.truncate()
    if buf.tell():
        yield buf.getvalue()


def export_csv(store: NodeStore, root: int, *, match: np.ndarray | None = None) -> str:
    return "".join(iter_csv(store, root, match=match))


# ---------------------------------------------------------------------------------------- JSON


def _quoted(text: str) -> str:
    return f'"{text}"' if text else "null"  # ISO times need no escaping


def iter_json(
    store: NodeStore,
    root: int,
    *,
    match: np.ndarray | None = None,
    filter_text: str = "",
    now: float | None = None,
) -> Iterator[str]:
    """A JSON document: metadata and ``roots``, each a nested tree (folders carry ``children``).

    One element per line, indented by its level; one piece per batch of elements.
    """
    head = {
        "format": JSON_FORMAT,
        "version": JSON_VERSION,
        "generator": f"SpaceKnife {__version__}",
        "generated_at": iso_time(int(time.time() if now is None else now)),
        "filter": filter_text or None,
    }
    yield json.dumps(head, ensure_ascii=False)[:-1] + ',\n"roots": ['
    times = LocalTimes()
    parts: list[str] = []
    for index, r in enumerate(export_roots(store, root)):
        tree = ExportTree(store, r, match=match, ordering=REPORT_ORDER)
        open_levels: list[int] = []  # folders whose children list is open
        later: list[bool] = []  # a child was written into it already (a comma first)
        for b in _batches(tree, times):
            for i, el in enumerate(b.elements):
                level = el.level
                while open_levels and open_levels[-1] >= level:
                    open_levels.pop()
                    later.pop()
                    parts.append("]}")
                if level == 0:
                    parts.append(("," if index else "") + f'\n{{"path": {encode_basestring(el.path)}, ')
                else:
                    parts.append(",\n" if later[-1] else "\n")
                    later[-1] = True
                    parts.append("  " * level + "{")
                text = (
                    f'"name": {encode_basestring(el.name)}, "type": "{b.kind[i]}", '
                    f'"size": {b.size[i]}, "disk_size": {b.alloc[i]}'
                )
                if el.is_dir:
                    text += f', "files": {b.files[i]}, "folders": {b.dirs[i]}'
                text += (
                    f', "modified": {_quoted(b.mtime[i])}, "created": {_quoted(b.ctime[i])}, '
                    f'"accessed": {_quoted(b.atime[i])}, "attributes": {_attributes_json(b.flags[i])}'
                )
                if el.is_dir:
                    parts.append(text + ', "children": [')
                    open_levels.append(level)
                    later.append(False)
                else:
                    parts.append(text + "}")
            yield "".join(parts)
            parts.clear()
        parts.append("]}" * len(open_levels))
    parts.append("\n]}\n")
    yield "".join(parts)


def export_json(
    store: NodeStore, root: int, *, match: np.ndarray | None = None, filter_text: str = ""
) -> str:
    return "".join(iter_json(store, root, match=match, filter_text=filter_text))


# ---------------------------------------------------------------------------------------- HTML

_CSS = """
:root{--bg:#08090B;--panel:#0E1013;--line:#1D2027;--line2:#2A2E37;--text:#F1F2F5;--text2:#AEB3BE;
--text3:#8F95A2;--accent:#8B9CFF;--track:#1A1D23;color-scheme:dark}
@media (prefers-color-scheme:light){:root{--bg:#F6F7F9;--panel:#FFFFFF;--line:#E7E9ED;--line2:#D6D9DF;
--text:#14161B;--text2:#454B57;--text3:#6A7080;--accent:#5464E8;--track:#EDEFF3;color-scheme:light}}
@media print{:root{--bg:#FFFFFF;--panel:#FFFFFF;--line:#DDDDDD;--line2:#CCCCCC;--text:#000000;
--text2:#333333;--text3:#555555;--accent:#4455CC;--track:#EEEEEE;color-scheme:light}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);
font:13px/1.45 Geist,"Noto Sans SC",system-ui,-apple-system,"Segoe UI","Microsoft YaHei",sans-serif}
main{max-width:1080px;margin:0 auto;padding:32px 16px 48px}
.brand{font-size:11px;letter-spacing:.08em;text-transform:uppercase;color:var(--text3)}
h1{font-size:28px;line-height:1.2;font-weight:600;margin:6px 0 8px}
h2{font-size:15px;font-weight:600;margin:0 0 4px}
.mono,.path{font-family:"Geist Mono",ui-monospace,"Cascadia Mono",Menlo,monospace}
.path{color:var(--text2);word-break:break-all}
.meta,.note{color:var(--text3)}
.note{margin:0 0 12px}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px;margin:20px 0 8px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:14px 16px}
.card .k{color:var(--text3);font-size:12px}
.card .v{font-size:22px;font-weight:600;margin-top:4px;font-variant-numeric:tabular-nums}
section{margin-top:32px}
.root+.root{margin-top:56px;padding-top:24px;border-top:1px solid var(--line2)}
.box{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:6px 0;overflow:hidden}
.row{display:grid;grid-template-columns:minmax(0,1fr) 160px 88px 56px;gap:12px;align-items:center;
padding:4px 14px 4px calc(14px + var(--d,0) * 18px)}
.row:hover{background:var(--track)}
.name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.dir>.name{font-weight:600}
summary{list-style:none;cursor:pointer}
summary::-webkit-details-marker{display:none}
summary>.name::before{content:"\\25B8";display:inline-block;width:14px;color:var(--text3)}
details[open]>summary>.name::before{content:"\\25BE"}
.leaf>.name{padding-left:14px}
.bar{height:6px;background:var(--track);border-radius:3px;overflow:hidden}
.bar i{display:block;height:100%;background:var(--accent);border-radius:3px}
.num{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}
.muted{color:var(--text3)}
table{width:100%;border-collapse:collapse}
th,td{padding:6px 14px;text-align:left;border-bottom:1px solid var(--line);vertical-align:top}
tr:last-child td{border-bottom:0}
th{color:var(--text3);font-weight:500;font-size:12px}
th.num,td.num{text-align:right}
td.path{word-break:break-all}
footer{margin-top:40px;color:var(--text3);font-size:12px}
@media (max-width:640px){.row{grid-template-columns:minmax(0,1fr) 76px 48px}.row .bar{display:none}
h1{font-size:22px}.hide-sm{display:none}}
"""


def _e(text: str) -> str:
    return html.escape(text, quote=True)


def _share(part: int, whole: int) -> float:
    return part / whole if whole > 0 else 0.0


def _row(name: str, alloc: int, whole: int, depth: int, cls: str, tag: str = "div") -> str:
    pct = f"{_share(alloc, whole) * 100:.1f}%"
    name = _e(name)
    return (
        f'<{tag} class="row {cls}" style="--d:{depth}">'
        f'<span class="name" title="{name}">{name}</span>'
        f'<span class="bar"><i style="width:{pct}"></i></span>'
        f'<span class="num">{_e(fmt.size(alloc))}</span><span class="num muted">{pct}</span></{tag}>'
    )


def _structure(tree: ExportTree, out: list[str], depth: int, top: int) -> None:
    store, ids = tree.store, tree.ids

    def folder(pos: int, level: int) -> None:
        whole = tree.sizes(pos)[1]
        kids = tree.children(pos)
        shown, rest = kids[:top], kids[top:]
        keys = tree.alloc_key(shown).tolist() if len(shown) else []
        for k, alloc in zip(shown.tolist(), keys, strict=True):
            name = store.name(int(ids[k]))
            if tree.is_dir[k] and level + 1 < depth and tree.kcount[k] > 0:
                out.append("<details>" + _row(name, alloc, whole, level, "dir", "summary"))
                folder(k, level + 1)
                out.append("</details>")
            else:
                out.append(_row(name, alloc, whole, level, "dir leaf" if tree.is_dir[k] else "leaf"))
        if len(rest):
            alloc = int(tree.alloc_key(rest).sum())
            label = TEXT["rest"].format(count=f"{len(rest):,}")
            out.append(_row(label, alloc, whole, level, "leaf muted"))

    out.append(
        "<details open>" + _row(tree.root_name, tree.sizes(0)[1], tree.sizes(0)[1], 0, "dir", "summary")
    )
    folder(0, 1)
    out.append("</details>")


def _visible_leaves(tree: ExportTree) -> np.ndarray:
    leaf = ~tree.is_dir
    if tree.vis is not None:
        leaf &= tree.vis
    leaves: np.ndarray = np.flatnonzero(leaf)
    return leaves


def _largest(tree: ExportTree, out: list[str], limit: int) -> None:
    leaves = _visible_leaves(tree)
    if not len(leaves) or limit <= 0:
        out.append(f'<p class="note">{TEXT["empty"]}</p>')
        return
    keys = tree.alloc_key(leaves)
    if len(leaves) > limit:
        part = np.argpartition(-keys, limit - 1)[:limit]
        leaves, keys = leaves[part], keys[part]
    order = np.argsort(-keys, kind="stable")
    out.append(
        f'<div class="box"><table><thead><tr><th class="num">{TEXT["col_disk"]}</th>'
        f'<th class="num hide-sm">{TEXT["col_modified"]}</th><th>{TEXT["col_path"]}</th></tr></thead><tbody>'
    )
    for i in order.tolist():
        pos = int(leaves[i])
        node = int(tree.ids[pos])
        mtime = tree.time(pos, 0)
        when = local_time(mtime, "%Y-%m-%d")
        out.append(
            f'<tr><td class="num">{_e(fmt.size(int(keys[i])))}</td><td class="num muted hide-sm">{when}</td>'
            f'<td class="path mono">{_e(tree.store.path(node))}</td></tr>'
        )
    out.append("</tbody></table></div>")


def _extensions(tree: ExportTree, out: list[str], limit: int) -> None:
    leaves = _visible_leaves(tree)
    if not len(leaves) or limit <= 0:
        out.append(f'<p class="note">{TEXT["empty"]}</p>')
        return
    table = tree.store.ext_table
    ext = tree.store.cols["ext"][tree.ids[leaves]].astype(np.int64)
    sums = np.zeros(len(table), dtype=np.int64)
    np.add.at(sums, ext, tree.alloc_key(leaves))
    counts = np.bincount(ext, minlength=len(table))
    used = np.flatnonzero(counts)
    order = used[np.lexsort((-counts[used], -sums[used]))][:limit]
    whole = int(sums.sum())
    out.append(
        f'<div class="box"><table><thead><tr><th>{TEXT["col_ext"]}</th>'
        f'<th class="num">{TEXT["col_files"]}</th><th class="num">{TEXT["col_disk"]}</th>'
        f'<th class="num">{TEXT["col_share"]}</th></tr></thead><tbody>'
    )
    for i in order.tolist():
        label = f".{table[i]}" if table[i] else TEXT["no_ext"]
        share = _share(int(sums[i]), whole)
        out.append(
            f'<tr><td class="mono">{_e(label)}</td><td class="num">{int(counts[i]):,}</td>'
            f'<td class="num">{_e(fmt.size(int(sums[i])))}</td>'
            f'<td class="num muted">{share * 100:.1f}%</td></tr>'
        )
    out.append("</tbody></table></div>")


def export_html(
    store: NodeStore,
    root: int,
    *,
    match: np.ndarray | None = None,
    filter_text: str = "",
    now: float | None = None,
    depth: int = 4,
    top: int = 30,
    largest: int = 50,
    extensions: int = 20,
) -> str:
    """A self-contained HTML report: totals, the folder structure (``depth`` levels, ``top`` largest
    children each), the ``largest`` files and the ``extensions`` largest extensions."""
    roots = export_roots(store, root)
    stamp = local_time(int(time.time() if now is None else now), "%Y-%m-%d %H:%M")
    subject = store.path(roots[0]) if len(roots) == 1 else TEXT["roots"].format(count=len(roots))
    out = [
        '<!doctype html>\n<html lang="zh-CN"><head><meta charset="utf-8">'
        '<meta name="viewport" content="width=device-width, initial-scale=1">'
        f'<meta name="generator" content="SpaceKnife {_e(__version__)}">'
        f"<title>{_e(TEXT['title'])} · {_e(subject)}</title><style>{_CSS}</style></head><body><main>"
    ]
    meta = [TEXT["generated"].format(time=stamp)]
    if filter_text:
        meta.append(TEXT["filter"].format(expr=filter_text))
    for r in roots:
        tree = ExportTree(store, r, match=match, ordering=REPORT_ORDER, now=now)
        size, alloc = tree.sizes(0)
        files, folders = tree.counts(0)
        cards = [
            (TEXT["disk"], fmt.size(alloc)),
            (TEXT["logical"], fmt.size(size)),
            (TEXT["matched"] if tree.masked else TEXT["files"], f"{files:,}"),
            (TEXT["folders"], f"{folders:,}"),
        ]
        out.append(
            '<div class="root"><header><div class="brand">SpaceKnife</div>'
            f'<h1>{_e(TEXT["title"])}</h1><p class="path">{_e(tree.root_path)}</p>'
            f'<p class="meta">{_e(" · ".join(meta))}</p></header><div class="cards">'
        )
        for key, value in cards:
            out.append(
                f'<div class="card"><div class="k">{_e(key)}</div><div class="v">{_e(value)}</div></div>'
            )
        out.append(f"</div><section><h2>{TEXT['structure']}</h2>")
        out.append(
            f'<p class="note">{_e(TEXT["structure_note"].format(top=top, depth=depth))}</p><div class="box">'
        )
        _structure(tree, out, depth, top)
        out.append(f"</div></section><section><h2>{TEXT['largest']}</h2>")
        _largest(tree, out, largest)
        out.append(f"</section><section><h2>{TEXT['extensions']}</h2>")
        _extensions(tree, out, extensions)
        out.append("</section></div>")
    base = TEXT["base_1000"] if fmt.UNIT_BASE == 1000 else TEXT["base_1024"]
    out.append(
        f"<footer>{_e(TEXT['footer'].format(version=__version__, base=base))}</footer></main></body></html>\n"
    )
    return "".join(out)


def iter_html(
    store: NodeStore, root: int, *, match: np.ndarray | None = None, filter_text: str = ""
) -> Iterator[str]:
    yield export_html(store, root, match=match, filter_text=filter_text)


# ---------------------------------------------------------------------------------------- registry


@dataclass(frozen=True, slots=True)
class ExportFormat:
    key: str  # "csv" | "json" | "html"
    title: str  # user-facing
    extension: str
    media_type: str
    bom: bool  # write a UTF-8 BOM: Excel reads a CSV without one in the ANSI code page
    chunks: Callable[..., Iterator[str]]  # (store, root, *, match=None, filter_text="")


def _csv_chunks(
    store: NodeStore, root: int, *, match: np.ndarray | None = None, filter_text: str = ""
) -> Iterator[str]:
    return iter_csv(store, root, match=match)


FORMATS: dict[str, ExportFormat] = {
    "csv": ExportFormat("csv", TEXT["csv"], ".csv", "text/csv", True, _csv_chunks),
    "json": ExportFormat("json", TEXT["json"], ".json", "application/json", False, iter_json),
    "html": ExportFormat("html", TEXT["html"], ".html", "text/html", False, iter_html),
}

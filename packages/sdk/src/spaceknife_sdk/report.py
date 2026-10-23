"""Report documents: block builders (``r.stats``, ``r.table`` …) and a plain-text rendering.

The frontends draw the blocks with their own components (numbers, dates and sizes formatted like
everything else in the app); a plugin only fills them::

    return Report(blocks=[
        r.stats([(T("文件", "Files"), len(rows), "count"), (T("合计", "Total"), total, "size")]),
        r.table([r.col("alloc", T("占用", "Size"), "size"), r.col("path", T("位置", "Location"), "path")],
                rows=rows),
    ])
"""

from __future__ import annotations

import datetime as _dt
from collections.abc import Iterable, Mapping, Sequence
from typing import Any

from spaceknife_protocol.plugins import (
    BarItem,
    BarsBlock,
    Block,
    CalloutBlock,
    KvBlock,
    KvItem,
    LabeledAction,
    MdBlock,
    MeterBlock,
    NodeItem,
    NodeRef,
    NodesBlock,
    PartItem,
    ProgressBlock,
    Report,
    RingBlock,
    SectionBlock,
    SparkBlock,
    StackBlock,
    StatItem,
    StatsBlock,
    TableBlock,
    TableColumn,
    TableMore,
    TableRow,
    Unit,
)
from spaceknife_protocol.text import Text, localize

from ._current import current_context
from .actions import normalize_action, resolve_action
from .tree import Rows

TextIn = Text | str


def _ref(node: int | NodeRef | Mapping[str, Any] | None) -> NodeRef | None:
    if node is None or isinstance(node, NodeRef):
        return node
    if isinstance(node, Mapping):
        return NodeRef.model_validate(dict(node))
    ctx = current_context()
    if ctx is None:
        raise ValueError("an integer node needs a plugin call (or pass a NodeRef)")
    return ctx.tree.ref(int(node), path=False)


def _refs(nodes: Sequence[int | NodeRef | Mapping[str, Any] | None]) -> list[NodeRef | None]:
    """Several nodes at once: integer ids get their paths in one request."""
    ints = [int(n) for n in nodes if isinstance(n, int) and not isinstance(n, bool)]
    paths: dict[int, str] = {}
    if ints:
        ctx = current_context()
        if ctx is None:
            raise ValueError("integer nodes need a plugin call (or pass NodeRefs)")
        tree = ctx.tree
        uniq = list(dict.fromkeys(ints))
        paths = dict(zip(uniq, tree.paths(uniq), strict=True))
        out: list[NodeRef | None] = []
        for n in nodes:
            if isinstance(n, int) and not isinstance(n, bool):
                out.append(NodeRef(scan_id=tree.scan_id, node=n, epoch=tree.epoch, path=paths[n]))
            else:
                out.append(_ref(n))
        return out
    return [_ref(n) for n in nodes]


def action(label: TextIn, icon: str = "", **short: Any) -> LabeledAction:
    """A button: ``r.action(T("全部加入删除篮", "Add all"), cleanup="all")``."""
    norm = normalize_action(short)
    ctx = current_context()
    plugin = ctx.plugin.id if ctx is not None else "local.plugin"
    return LabeledAction(label=label, icon=icon, action=resolve_action(norm, plugin))


def md(text: TextIn) -> MdBlock:
    return MdBlock(text=text)


def _stat(item: Any) -> StatItem:
    if isinstance(item, StatItem):
        return item
    if isinstance(item, Mapping):
        return StatItem.model_validate(dict(item))
    label, value, *rest = item
    unit: Any = rest[0] if rest else "count"
    hint = rest[1] if len(rest) > 1 else ""
    return StatItem(label=label, value=value, unit=unit, hint=hint)


def stats(items: Iterable[Any]) -> StatsBlock:
    """Key numbers in a row: ``(label, value, unit[, hint])`` tuples."""
    return StatsBlock(items=[_stat(i) for i in items])


def kv(items: Iterable[Any]) -> KvBlock:
    """A details-panel style table: ``(label, value[, unit[, node]])`` tuples."""
    out = []
    for item in items:
        if isinstance(item, KvItem):
            out.append(item)
            continue
        label, value, *rest = item
        unit: Any = rest[0] if rest else "text"
        node = _ref(rest[1]) if len(rest) > 1 else None
        out.append(KvItem(label=label, value=value, unit=unit, node=node))
    return KvBlock(items=out)


def col(
    key: str, label: TextIn, unit: Unit = "text", *, align: str | None = None, grow: float | None = None
) -> TableColumn:
    return TableColumn(key=key, label=label, unit=unit, align=align, grow=grow)  # type: ignore[arg-type]


def table(
    columns: Sequence[TableColumn],
    rows: Rows | Iterable[Mapping[str, Any]],
    *,
    sort: str | None = None,
    page_size: int = 50,
    select: bool = False,
    more: TableMore | None = None,
) -> TableBlock:
    """Rows are dicts with a value per column key, and optionally ``node`` (an id or a NodeRef) and
    ``actions``; :class:`Rows` from a query work as they are. At most 2000 rows (use ``more``)."""
    keys = [c.key for c in columns]
    if isinstance(rows, Rows):
        records = rows.records()
        if "path" in rows.texts:
            tree = rows.tree
            refs: list[NodeRef | None] = [
                NodeRef(scan_id=tree.scan_id, node=int(i), epoch=tree.epoch, path=p)
                for i, p in zip(rows.ids.tolist(), rows.paths, strict=True)
            ]
        else:
            refs = _refs([r["node"] for r in records])
    else:
        records = [dict(r) for r in rows]
        refs = _refs([r.get("node") for r in records])
    out: list[TableRow] = []
    for rec, ref in zip(records, refs, strict=False):  # the host cuts to MAX_TABLE_ROWS and says so
        cells = {k: _cell(rec.get(k)) for k in keys}
        acts = rec.get("actions") or []
        out.append(TableRow(cells=cells, node=ref, actions=list(acts)))
    return TableBlock(
        columns=list(columns), rows=out, sort=sort, page_size=page_size, select=select, more=more
    )


def _cell(v: Any) -> Any:
    if isinstance(v, Text):
        return localize(v)
    if hasattr(v, "item") and not isinstance(v, str | bytes):  # numpy scalars
        return v.item()
    return v


def bars(items: Iterable[Any], *, max: float | None = None) -> BarsBlock:
    """Horizontal bars: ``(label, value[, unit[, node[, tone]]])`` tuples, dicts with those keys, or
    :class:`BarItem`."""
    out = []
    for item in items:
        if isinstance(item, BarItem):
            out.append(item)
            continue
        if isinstance(item, Mapping):
            d = dict(item)
            if "node" in d:
                d["node"] = _ref(d["node"])
            d.setdefault("unit", "size")
            out.append(BarItem.model_validate(d))
            continue
        label, value, *rest = item
        out.append(
            BarItem(
                label=label,
                value=float(value),
                unit=rest[0] if rest else "size",
                node=_ref(rest[1]) if len(rest) > 1 else None,
                tone=rest[2] if len(rest) > 2 else None,
            )
        )
    return BarsBlock(items=out, max=max)


def _parts(parts: Iterable[Any]) -> list[PartItem]:
    out = []
    for p in parts:
        if isinstance(p, PartItem):
            out.append(p)
            continue
        label, value, *rest = p
        out.append(PartItem(label=label, value=float(value), tone=rest[0] if rest else None))
    return out


def stack(parts: Iterable[Any], unit: Unit = "size") -> StackBlock:
    return StackBlock(parts=_parts(parts), unit=unit)


def ring(parts: Iterable[Any], unit: Unit = "size", center: TextIn = "") -> RingBlock:
    return RingBlock(parts=_parts(parts), unit=unit, center=center)


def meter(
    value: float, max: float, label: TextIn = "", unit: Unit = "size", tone: str | None = None
) -> MeterBlock:
    return MeterBlock(value=value, max=max, label=label, unit=unit, tone=tone)


def spark(values: Iterable[float], labels: Iterable[TextIn] = (), unit: Unit = "size") -> SparkBlock:
    return SparkBlock(values=[float(v) for v in values], labels=list(labels), unit=unit)


def nodes(items: Iterable[Any], actions: Iterable[LabeledAction] = ()) -> NodesBlock:
    """Nodes with icons: ids, NodeRefs, or ``(node, label, size, reason)`` tuples."""
    raw = list(items)
    heads = [i[0] if isinstance(i, tuple) else i for i in raw]
    refs = _refs(heads)
    out = []
    for item, ref in zip(raw, refs, strict=True):
        assert ref is not None
        if isinstance(item, tuple):
            label = item[1] if len(item) > 1 else ""
            size = item[2] if len(item) > 2 else None
            reason = item[3] if len(item) > 3 else ""
            out.append(NodeItem(node=ref, label=label, size=size, reason=reason))
        else:
            out.append(NodeItem(node=ref))
    return NodesBlock(items=out, actions=list(actions))


def callout(text: TextIn, tone: str = "info") -> CalloutBlock:
    return CalloutBlock(text=text, tone=tone)  # type: ignore[arg-type]


def progress(done: float, total: float, text: TextIn = "") -> ProgressBlock:
    return ProgressBlock(done=done, total=total, text=text)


def section(title: TextIn, blocks: Iterable[Block], *, collapsed: bool = False) -> SectionBlock:
    return SectionBlock(title=title, blocks=list(blocks), collapsed=collapsed)


# --------------------------------------------------------------------------- plain text


def fmt(value: Any, unit: str) -> str:
    """A value the way the plain-text rendering shows it (the frontends format on their own)."""
    if value is None:
        return "—"
    if unit == "size" and isinstance(value, int | float):
        n = float(value)
        for suffix in ("B", "KB", "MB", "GB", "TB", "PB"):
            if abs(n) < 1024 or suffix == "PB":
                return f"{n:.0f} {suffix}" if suffix == "B" else f"{n:.1f} {suffix}"
            n /= 1024
    if unit == "pct" and isinstance(value, int | float):
        return f"{value * 100:.1f}%"
    if unit == "count" and isinstance(value, int | float):
        return f"{value:,.0f}"
    if unit == "date" and isinstance(value, int | float):
        return _dt.datetime.fromtimestamp(value, _dt.UTC).strftime("%Y-%m-%d")
    if unit == "ago" and isinstance(value, int | float):
        if not value:
            return "—"
        days = (_dt.datetime.now(_dt.UTC).timestamp() - value) / 86400
        return f"{days:.0f} d" if days < 365 else f"{days / 365:.1f} y"
    if unit == "duration" and isinstance(value, int | float):
        return f"{value:.1f} s"
    if unit == "bytes_per_s" and isinstance(value, int | float):
        return fmt(value, "size") + "/s"
    return str(value)


def _lines(block: Any, depth: int) -> list[str]:
    pad = "  " * depth
    t = block.type
    if t == "md":
        return [pad + line for line in block.text.splitlines()]
    if t == "stats":
        return [pad + " · ".join(f"{i.label}: {fmt(i.value, i.unit)}" for i in block.items)]
    if t == "kv":
        return [f"{pad}{i.label}: {fmt(i.value, i.unit)}" for i in block.items]
    if t == "table":
        head = pad + " | ".join(c.label for c in block.columns)
        out = [head, pad + "-" * max(3, len(head) - len(pad))]
        for row in block.rows:
            out.append(pad + " | ".join(fmt(row.cells.get(c.key), c.unit) for c in block.columns))
        if block.more is not None:
            out.append(pad + "…")
        return out
    if t == "bars":
        return [f"{pad}{i.label}: {fmt(i.value, i.unit)}" for i in block.items]
    if t in ("stack", "ring"):
        return [pad + " + ".join(f"{p.label} {fmt(p.value, block.unit)}" for p in block.parts)]
    if t == "meter":
        return [f"{pad}{block.label}: {fmt(block.value, block.unit)} / {fmt(block.max, block.unit)}"]
    if t == "spark":
        return [pad + " ".join(fmt(v, block.unit) for v in block.values)]
    if t == "nodes":
        return [
            f"{pad}- {i.label or i.node.path or i.node.node}" + (f" ({i.reason})" if i.reason else "")
            for i in block.items
        ]
    if t == "callout":
        return [f"{pad}[{block.tone}] {block.text}"]
    if t == "progress":
        return [f"{pad}{block.text} {block.done:g}/{block.total:g}".strip()]
    if t == "section":
        out = [f"{pad}## {block.title}"]
        for b in block.blocks:
            out += _lines(b, depth + 1)
        return out
    return [f"{pad}<{t}>"]


def render_text(report: Report | Mapping[str, Any]) -> str:
    """A report as plain text: what ``plugin render`` prints and tests assert on."""
    rep = report if isinstance(report, Report) else Report.model_validate(dict(report))
    out: list[str] = []
    if rep.title:
        out.append(f"# {rep.title}")
    if rep.subtitle:
        out.append(str(rep.subtitle))
    if not rep.blocks and rep.empty:
        out.append(str(rep.empty))
    for b in rep.blocks:
        out += _lines(b, 0)
    for a in rep.actions:
        out.append(f"[{a.label}]")
    if rep.footnote:
        out.append(str(rep.footnote))
    return "\n".join(out)


__all__ = [
    "action",
    "bars",
    "callout",
    "col",
    "fmt",
    "kv",
    "md",
    "meter",
    "nodes",
    "progress",
    "render_text",
    "ring",
    "section",
    "spark",
    "stack",
    "stats",
    "table",
]

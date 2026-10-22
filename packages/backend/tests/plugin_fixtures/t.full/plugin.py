"""Test plugin: one of each code contribution (tests/test_plugins_*.py)."""

from __future__ import annotations

import zipfile
from collections.abc import AsyncIterator, Iterator
from pathlib import PurePath

import numpy as np

from spaceknife_sdk import (
    ActionContext,
    Change,
    ChangeUnit,
    Columns,
    Context,
    FieldValue,
    Insight,
    Listing,
    Node,
    Params,
    Payload,
    Plugin,
    Proposal,
    Report,
    ReportContext,
    T,
    Toast,
    TreeView,
    VisualInput,
    r,
)

plugin = Plugin()


class SumParams(Params):
    a: int
    b: int = 0


class SumResult(Payload):
    total: int


@plugin.event
class Progress(Payload):
    done: int
    total: int


@plugin.state
class Status(Payload):
    runs: int = 0


@plugin.method
async def add(ctx: Context, p: SumParams) -> SumResult:
    ctx.emit(Progress(done=1, total=1))
    ctx.state(Status).update(runs=ctx.state(Status).get().runs + 1)
    return SumResult(total=p.a + p.b)


@plugin.method
def lang(ctx: Context) -> Payload:
    class Out(Payload):
        text: str

    return Out(text=ctx.t("你好", "Hello"))


@plugin.method
async def columns(ctx: Context) -> Columns:
    return Columns(ids=np.arange(5, dtype=np.int64), double=np.arange(5, dtype=np.int64) * 2)


@plugin.method
async def hash_first(ctx: Context) -> Payload:
    class Out(Payload):
        digest: str
        head: str

    rows = ctx.tree.query("*.dat", kind="file", columns=("path",), sort="path", limit=1)
    path = rows.paths[0]
    return Out(digest=ctx.files.hash(path), head=ctx.files.read(path, 4).decode())


@plugin.method
async def tree_info(ctx: Context) -> Payload:
    class Out(Payload):
        n: int
        roots: list[int]
        files: int
        alloc_total: int
        walked: int
        largest: str

    t = ctx.tree
    alloc = t.col("alloc")
    kind = t.col("kind")
    walked = sum(len(ids) for ids in t.walk())
    big = t.query(kind="file", columns=("bytes", "name"), sort="-bytes", limit=1)
    return Out(
        n=t.n,
        roots=t.roots,
        files=int((kind == 0).sum()),
        alloc_total=int(alloc[kind == 0].sum()),
        walked=walked,
        largest=big.names[0],
    )


@plugin.method
async def propose(ctx: Context) -> Payload:
    class Out(Payload):
        added: int

    rows = ctx.tree.query("*.log", kind="file", columns=("path",))
    return Out(added=ctx.basket.propose(rows.ids.tolist(), T("日志", "logs")))


@plugin.method
async def core_call(ctx: Context) -> Payload:
    class Out(Payload):
        items: int

    return Out(items=len(ctx.call("basket.list")["items"]))


@plugin.command("hello", T("问好", "Greet"), keys=["Ctrl+Alt+G"], menus=["node"])
async def hello(ctx: Context, a: ActionContext) -> Toast:
    return Toast(text=T("你好", "Hello"), tone="ok")


@plugin.method
async def why_not(ctx: Context, a: ActionContext) -> str | None:
    return None


@plugin.report(T("大文件", "Big files"), name="big", slot="tab", refresh=["scan"])
async def big(ctx: Context, c: ReportContext) -> Report:
    rows = ctx.tree.query(
        f">{ctx.settings['min_size']}b", kind="file", columns=("bytes", "path"), sort="-bytes"
    )
    ctx.progress(1, 1)
    return Report(
        title=T("大文件", "Big files"),
        blocks=[
            r.stats(
                [
                    (T("文件", "Files"), len(rows), "count"),
                    (T("合计", "Total"), int(rows["bytes"].sum()), "size"),
                ]
            ),
            r.table(
                [r.col("bytes", T("占用", "Size"), "size"), r.col("path", T("位置", "Location"), "path")],
                rows=rows,
            ),
        ],
        actions=[r.action(T("全部加入删除篮", "Add all to the basket"), cleanup="logs")],
    )


@plugin.node_field(T("名字长度", "Name length"), slots=["inspector", "hover"])
async def name_len(ctx: Context, node: Node) -> FieldValue | str | None:
    return FieldValue(value=len(node.name), unit="count")


@plugin.node_field(T("扩展名", "Extension"))
def ext(ctx: Context, node: Node) -> str | None:
    return node.ext or None


@plugin.insight
async def explain(ctx: Context, node: Node) -> Insight | None:
    if node.name.endswith(".log"):
        return Insight(
            title=T("日志", "Log"), reason=T("程序写的日志", "A program's log"), kind="log", cleanup="safe"
        )
    return None


@plugin.visualization("by_name", T("按名字长度", "By name length"), base="flame")
def by_name(v: VisualInput) -> np.ndarray:
    return v.tree.col("child_count").astype(np.float64) * 0 + 1  # every node weighs 1


@plugin.color_mode("parity", T("奇偶", "Odd and even"), palette=["muted", "ok", "warn"])
def parity(tree: TreeView, ids: np.ndarray) -> np.ndarray:
    return (1 + (ids % 2)).astype(np.uint8)


@plugin.export_format("lines", T("名字逐行", "Names, one a line"), extension=".txt")
def lines(tree: TreeView, root: int, match: np.ndarray | None) -> Iterator[str]:
    rows = tree.query(kind="file", columns=("name",), sort="name")
    for name in rows.names:
        yield name + "\n"


@plugin.node_set("logs", T("日志", "Logs"), aliases=["logfiles"])
async def logs(ctx: Context, tree: TreeView) -> np.ndarray:
    return tree.query("*.log", kind="file", columns=()).ids


@plugin.cleanup(T("删除日志", "Remove logs"), name="logs")
async def clean_logs(ctx: Context, c: ReportContext) -> list[Proposal]:
    rows = ctx.tree.query("*.log", kind="file", columns=("path",))
    return [Proposal(node=int(i), reason=T("日志文件", "A log file")) for i in rows.ids]


@plugin.scan_source("zip", T("压缩包", "Archive"), pick="file", extensions=[".zip"])
async def zip_source(ctx: Context, uri: str, out: Listing) -> None:
    with zipfile.ZipFile(ctx.files.open_path(uri)) as z:
        for info in z.infolist():
            if info.is_dir():
                out.dir(info.filename)
            else:
                out.file(info.filename, size=info.file_size, alloc=info.compress_size)
    out.root_name(PurePath(uri).name)


@plugin.watcher("zip")
async def watch_zip(ctx: Context, uri: str) -> AsyncIterator[list[Change]]:
    yield [Change(path="a.txt", kind="modified")]


@plugin.change_annotator
async def why(ctx: Context, items: list[ChangeUnit]) -> list[str | None]:
    return ["grew" if i.delta > 0 else None for i in items]


@plugin.on("scan_done")
async def done(ctx: Context, scan: object) -> None:
    ctx.state(Status).update(runs=-1)


@plugin.stream
async def ticks(ctx: Context) -> AsyncIterator[Progress]:
    for i in range(3):
        yield Progress(done=i, total=3)

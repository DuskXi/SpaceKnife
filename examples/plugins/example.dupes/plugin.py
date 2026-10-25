"""Duplicate files: groups of files with the same contents (docs/plugin-dev.md 「参考插件」).

The search runs when asked (the report, the cleanup, the ``find`` method), on a worker thread
(``ctx.compute``): files of the same size are hashed — the first 64 KiB, then the whole file for
those that still match — a few at a time (``ctx.files.hash_many``: its reads keep to the disk-load
limit the user chose), with progress and a shared state the command's status line shows. Extra hard
links and cloud placeholders are left out (they free nothing; reading a placeholder would download
it); what the extra copies take is their allocated size. The field and the node set only read the
last result (a field has 1 s, a set 5 s: never enough to hash a disk) and are asked again when a
search finishes (``ctx.invalidate``). A whole disk takes longer than any call may: the command
「在后台查找」 starts the ``search`` background task, which the status bar shows with its progress
and a cancel button; the report then shows what it found. 「只保留这一份」 in a file's context menu
and in the details panel is greyed (with the reason) on folders and on files without copies; it
runs the same cleanup with an argument (``keep``: that file's group only, that file kept).
"""

from __future__ import annotations

from collections import defaultdict

import numpy as np

from spaceknife_sdk import (
    ActionContext,
    Context,
    FieldValue,
    Flag,
    Kind,
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
    r,
)

plugin = Plugin()

HEAD = 64 << 10  # bytes hashed first
REASON_PATH = 100  # characters of the kept copy's path in a proposal's reason (it holds 120)
SKIP = int(Flag.HARDLINK_DUP | Flag.CLOUD | Flag.OFFLINE)  # frees nothing / not on this disk


@plugin.event
class Progress(Payload):
    done: int
    total: int


@plugin.state
class Status(Payload):
    groups: int = 0
    wasted: int = 0
    busy: bool = False


class FindParams(Params):
    min_size: int | None = None  # default: the setting


class Group(Payload):
    size: int
    nodes: list[int]
    paths: list[str]


class FindResult(Payload):
    groups: list[Group]
    wasted: int  # bytes the extra copies take on disk (allocated)


_cache: dict[tuple[str, int, int, int], FindResult] = {}


def _key(tree: TreeView, min_size: int) -> tuple[str, int, int, int]:
    return (tree.scan_id, tree.epoch, tree.version, min_size)


def _cached(ctx: Context) -> FindResult | None:
    """The last search of this tree as it is now, if there was one (fields and sets read only this)."""
    return _cache.get(_key(ctx.tree, int(ctx.settings["min_size"])))


def _find(ctx: Context, min_size: int) -> FindResult:
    """The groups (blocking: hashing reads the files; runs on a worker thread)."""
    tree = ctx.tree
    key = _key(tree, min_size)
    hit = _cache.get(key)
    if hit is not None:
        return hit
    c = tree.cols("kind", "size", "alloc", "flags")  # whole columns: no row limit to run into
    files = np.flatnonzero((c["kind"] == Kind.FILE) & (c["size"] >= min_size) & ((c["flags"] & SKIP) == 0))
    by_size: dict[int, list[int]] = defaultdict(list)
    for node in files.tolist():
        by_size[int(c["size"][node])].append(node)
    todo = [(size, nodes) for size, nodes in by_size.items() if len(nodes) > 1]
    total = sum(len(nodes) for _size, nodes in todo)
    done = 0
    groups: list[Group] = []
    for size, nodes in sorted(todo, reverse=True):
        ctx.check()  # cancelled (a newer search, the user went on): stop here
        heads: dict[str | None, list[int]] = defaultdict(list)
        for node, digest in zip(nodes, ctx.files.hash_many(nodes, limit=HEAD), strict=True):
            heads[digest].append(node)
        done += len(nodes)
        ctx.emit(Progress(done=done, total=total))
        ctx.progress(
            done,
            total,
            T("已比较 {done} / {total} 个文件", "Compared {done} of {total} files").format(
                done=done, total=total
            ),
        )  # the report's progress bar; in the background task, the status bar's
        for digest, same in heads.items():
            if digest is None or len(same) < 2:
                continue  # unreadable, or unique
            full: dict[str | None, list[int]] = defaultdict(list)
            digests = ctx.files.hash_many(same) if size > HEAD else ["head"] * len(same)
            for node, whole in zip(same, digests, strict=True):
                full[whole].append(node)
            for whole, members in full.items():
                if whole is not None and len(members) > 1:
                    paths = tree.paths(members)
                    order = sorted(range(len(members)), key=lambda i: paths[i])
                    groups.append(
                        Group(size=size, nodes=[members[i] for i in order], paths=[paths[i] for i in order])
                    )
    ctx.emit(Progress(done=total, total=total))
    wasted = sum(int(c["alloc"][n]) for g in groups for n in g.nodes[1:])
    result = FindResult(groups=groups, wasted=wasted)
    _cache.clear()  # one scan version at a time
    _cache[key] = result
    return result


def _search(ctx: Context, min_size: int) -> FindResult:
    """The search with the status line kept up to date (blocking)."""
    state = ctx.state(Status)
    state.update(busy=True)
    try:
        found = _find(ctx, min_size)
    finally:
        state.update(busy=False)
    state.update(groups=len(found.groups), wasted=found.wasted)
    ctx.invalidate("node_field", "node_set")  # the field and :set:dupes show the new result
    return found


async def groups(ctx: Context, min_size: int | None = None) -> FindResult:
    return await ctx.compute(_search, ctx, int(min_size or ctx.settings["min_size"]))


@plugin.background("search", T("查找重复文件", "Finding duplicate files"))
def search(ctx: Context) -> None:
    """The whole scan, however long it takes (the status bar shows it; cancelling stops it within a
    MiB read); the report, the cleanup, the field and the set use what it found. When it is done a
    notice says so, with a button opening the report (a system notification while minimized)."""
    found = _search(ctx, int(ctx.settings["min_size"]))
    ctx.notify(
        T("重复文件找完了", "Duplicate files: done"),
        T("{groups} 组，多占 {wasted:size}", "{groups} groups, {wasted:size} extra").format(
            groups=len(found.groups), wasted=found.wasted
        ),
        action="open",
    )


@plugin.command(
    "search", T("在后台查找重复文件", "Find duplicates in the background"), icon="copy", when=["done"]
)
def start_search(ctx: Context, a: ActionContext) -> Toast:
    ctx.tasks.start("search", key=a.scan_id or "")  # one per scan; again while it runs: the same one
    return Toast(
        text=T("正在后台查找，进度在状态栏", "Searching in the background; the status bar shows how far")
    )


@plugin.method(latest=True, timeout=600)
async def find(ctx: Context, p: FindParams) -> FindResult:
    """Groups of files with the same contents, the largest first."""
    return await groups(ctx, p.min_size)


@plugin.report(T("重复文件", "Duplicate files"), name="dupes", icon="copy", refresh=["scan"], timeout=600)
async def dupes(ctx: Context, c: ReportContext) -> Report:
    found = await groups(ctx)
    rows = [
        {"node": g.nodes[0], "size": g.size, "copies": len(g.nodes), "path": g.paths[0]} for g in found.groups
    ]
    return Report(
        title=T("重复文件", "Duplicate files"),
        empty=T("没有内容相同的文件", "No files with the same contents"),
        blocks=[
            r.stats(
                [
                    (T("组", "Groups"), len(found.groups), "count"),
                    (T("多余的副本占用", "Extra copies take"), found.wasted, "size"),
                ]
            ),
            r.table(
                [
                    r.col("size", T("每份大小", "Size each"), "size"),
                    r.col("copies", T("份数", "Copies"), "count"),
                    r.col("path", T("第一份", "First copy"), "path"),
                ],
                rows=rows,
                sort="-size",
            ),
        ]
        if found.groups
        else [],
        actions=[r.action(T("把多余的副本加入删除篮", "Add the extra copies to the basket"), cleanup="extra")]
        if found.groups
        else [],
    )


@plugin.cleanup(T("多余的副本", "Extra copies"), name="extra", icon="copy", timeout=600)
async def extra(ctx: Context, c: ReportContext) -> list[Proposal]:
    """All copies but the first of each group (by path); with ``args = { keep = "node" }`` (the
    command below) only the group of ``c.node``, keeping that one. The user reviews them first."""
    found = await groups(ctx)
    only = c.node if c.args.get("keep") == "node" else None
    picked = [
        (g, g.nodes.index(only) if only is not None else 0)
        for g in found.groups
        if only is None or only in g.nodes
    ]
    # the kept copy by its path under the scanned folder (one request), shortened at its start
    kept_at = ctx.tree.rel_paths([g.nodes[k] for g, k in picked]) if picked else []
    out = []
    for (g, kept), at in zip(picked, kept_at, strict=True):
        reason = T("与 {keep} 相同", "Same as {keep}").format(keep=_short(at))
        out += [Proposal(node=node, reason=reason) for i, node in enumerate(g.nodes) if i != kept]
    return out


def _short(path: str) -> str:
    """``path`` in at most REASON_PATH characters: the end kept (the file's name and folders)."""
    return path if len(path) <= REASON_PATH else "…" + path[-(REASON_PATH - 1) :]


@plugin.method
def has_copies(ctx: Context, a: ActionContext) -> str:
    """Why 「只保留这一份」 can't run on ``a.node`` (the menu's file): no copies in the last search."""
    found = _cached(ctx)
    if found is None:
        return ctx.t("先查找一次重复文件", "Find duplicates first")
    if not any(a.node in g.nodes for g in found.groups):
        return ctx.t("没有内容和它相同的文件", "No other file has its contents")
    return ""


# on a file in the context menu and the details panel: greyed on folders (when) and on files
# without copies (check); the clicked file is c.node of the cleanup, ``keep`` tells it what to do
plugin.command(
    "keep_this",
    T("只保留这一份", "Keep only this one"),
    icon="copy",
    when=["file"],
    menus=["node", "inspector"],
    check="has_copies",
    action={"cleanup": "extra", "args": {"keep": "node"}},
)


@plugin.node_field(T("同内容的副本", "Same-content copies"), kinds=["file"])
def copies(ctx: Context, node: Node) -> FieldValue | None:
    """From the last search only (1 s is not enough to hash): nothing until one ran."""
    found = _cached(ctx)
    if found is None or node.size < int(ctx.settings["min_size"]):
        return None
    for g in found.groups:
        if node.id in g.nodes:
            return FieldValue(value=len(g.nodes) - 1, unit="count")
    return None


@plugin.node_set("dupes", T("重复文件", "Duplicates"), aliases=["duplicates"])
def dupes_set(ctx: Context, tree: TreeView) -> np.ndarray:
    """``:set:dupes`` in the filter box: every file that has a copy (after a search ran)."""
    found = _cached(ctx)
    return np.asarray([n for g in found.groups for n in g.nodes] if found else [], dtype=np.int64)

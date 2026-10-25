"""Duplicate files: groups of files with the same contents (docs/plugin-dev.md 「参考插件」).

The search runs when asked (the report, the cleanup, the ``find`` method), on a worker thread
(``ctx.compute``): files of the same size are hashed — the first 64 KiB, then the whole file for
those that still match — a few at a time (``ctx.files.hash_many``), with progress events and a
shared state the command's status line shows. Extra hard links and cloud placeholders are left out
(they free nothing; reading a placeholder would download it); what the extra copies take is their
allocated size. The field and the node set only read the last result (a field has 1 s, a set 5 s:
never enough to hash a disk) and are asked again when a search finishes (``ctx.invalidate``).
"""

from __future__ import annotations

from collections import defaultdict

import numpy as np

from spaceknife_sdk import (
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
    TreeView,
    r,
)

plugin = Plugin()

HEAD = 64 << 10  # bytes hashed first
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


async def groups(ctx: Context, min_size: int | None = None) -> FindResult:
    state = ctx.state(Status)
    state.update(busy=True)
    try:
        found = await ctx.compute(_find, ctx, int(min_size or ctx.settings["min_size"]))
    finally:
        state.update(busy=False)
    state.update(groups=len(found.groups), wasted=found.wasted)
    ctx.invalidate("node_field", "node_set")  # the field and :set:dupes show the new result
    return found


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
    """All copies but the first of each group (by path); the user reviews them first."""
    found = await groups(ctx)
    out = []
    for g in found.groups:
        keep = g.paths[0]
        for node in g.nodes[1:]:
            out.append(Proposal(node=node, reason=T("与 {keep} 相同", "Same as {keep}").format(keep=keep)))
    return out


@plugin.node_field(T("同内容的副本", "Copies with the same contents"), kinds=["file"])
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

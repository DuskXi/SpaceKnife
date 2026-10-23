"""Duplicate files: groups of files with the same contents (docs/plugin-dev.md 「参考插件」).

Files of the same size are hashed — the first 64 KiB, then the whole file for those that still
match — on a worker thread (``ctx.compute``), with progress events and a shared state the command's
status line shows. One result per scan version and minimum size is kept, so the report, the field,
the node set and the cleanup share it.
"""

from __future__ import annotations

from collections import defaultdict

import numpy as np

from spaceknife_sdk import (
    Context,
    FieldValue,
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
    wasted: int  # bytes the extra copies take


_cache: dict[tuple[str, int, int, int], FindResult] = {}


def _find(ctx: Context, min_size: int) -> FindResult:
    """The groups (blocking: hashing reads the files; runs on a worker thread)."""
    tree = ctx.tree
    key = (tree.scan_id, tree.epoch, tree.version, min_size)
    hit = _cache.get(key)
    if hit is not None:
        return hit
    rows = tree.query(kind="file", min_size=min_size, columns=("size", "path"), limit=500_000)
    by_size: dict[int, list[tuple[int, str]]] = defaultdict(list)
    for node, size, path in zip(rows.ids.tolist(), rows["size"].tolist(), rows.paths, strict=True):
        if size >= min_size:
            by_size[int(size)].append((int(node), path))
    todo = [(size, items) for size, items in by_size.items() if len(items) > 1]
    total = sum(len(items) for _size, items in todo)
    done = 0
    groups: list[Group] = []
    for size, items in sorted(todo, reverse=True):
        heads: dict[str, list[tuple[int, str]]] = defaultdict(list)
        for node, path in items:
            heads[ctx.files.hash(path, limit=HEAD)].append((node, path))
            done += 1
            if done % 50 == 0:
                ctx.emit(Progress(done=done, total=total))
        for same in heads.values():
            if len(same) < 2:
                continue
            full: dict[str, list[tuple[int, str]]] = defaultdict(list)
            for node, path in same:
                full[ctx.files.hash(path) if size > HEAD else "head"].append((node, path))
            for members in full.values():
                if len(members) > 1:
                    members.sort(key=lambda m: m[1])
                    groups.append(
                        Group(size=size, nodes=[m[0] for m in members], paths=[m[1] for m in members])
                    )
    ctx.emit(Progress(done=total, total=total))
    result = FindResult(groups=groups, wasted=sum(g.size * (len(g.nodes) - 1) for g in groups))
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


@plugin.node_field(T("同内容的副本", "Copies with the same contents"))
async def copies(ctx: Context, node: Node) -> FieldValue | None:
    if not node.is_file or node.size < int(ctx.settings["min_size"]):
        return None
    found = await groups(ctx)
    for g in found.groups:
        if node.id in g.nodes:
            return FieldValue(value=len(g.nodes) - 1, unit="count")
    return None


@plugin.node_set("dupes", T("重复文件", "Duplicates"), aliases=["duplicates"])
async def dupes_set(ctx: Context, tree: TreeView) -> np.ndarray:
    """``:set:dupes`` in the filter box: every file that has a copy."""
    found = await groups(ctx)
    return np.asarray([n for g in found.groups for n in g.nodes], dtype=np.int64)

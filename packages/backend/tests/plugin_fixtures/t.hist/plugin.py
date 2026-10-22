"""Test plugin: saved snapshots through ``ctx.snapshots`` (tests/test_plugins_live.py)."""

from __future__ import annotations

from spaceknife_sdk import Context, Params, Payload, Plugin, Report, ReportContext, T, r
from spaceknife_sdk.errors import HostError

plugin = Plugin()


class OpenParams(Params):
    count: int = 1
    close: bool = False


class Opened(Payload):
    listed: int
    nodes: list[int]
    refused: int = 0  # the error code of the open that was refused (0: none)


@plugin.method
async def open_some(ctx: Context, p: OpenParams) -> Opened:
    """Open the newest ``count`` snapshots one after the other (closing each when asked)."""
    snaps = ctx.snapshots.list()
    nodes: list[int] = []
    refused = 0
    for info in snaps[: p.count]:
        try:
            tree = ctx.snapshots.open(str(info["path"]))
        except HostError as exc:
            refused = exc.code
            break
        nodes.append(tree.n)
        if p.close:
            tree.close()
    return Opened(listed=len(snaps), nodes=nodes, refused=refused)


@plugin.report(T("快照历史", "Snapshot history"), slot="history")
async def history(ctx: Context, c: ReportContext) -> Report:
    snaps = ctx.snapshots.list()
    return Report(
        blocks=[
            r.stats([(T("快照", "Snapshots"), len(snaps), "count")]),
            r.spark([float(s["bytes_alloc"]) for s in snaps], unit="size"),
        ]
    )

"""Acceptance probe: records what each kind of call gets (the call as the host describes it, the
scan its ``ctx.tree`` reads, the language), what ``tree_changed`` hears, and has calls that are
slow, crash the process or read files. The tests read it all back with ``dump``."""

from __future__ import annotations

import asyncio
import dataclasses
import os
import time
from collections.abc import AsyncIterator
from typing import Any

import numpy as np

from spaceknife_sdk import (
    ActionContext,
    ColorInput,
    Context,
    Node,
    Params,
    Payload,
    Plugin,
    PluginError,
    Report,
    ReportContext,
    ScanInfo,
    T,
    Text,
    TreeChange,
    r,
)

plugin = Plugin()
plugin.setting("bands", T("分档", "Bands"), "1d, 1w")

KEEP = 400  # the newest records of each list
SEEN: dict[str, Any] = {
    "calls": [],  # one per call of any kind: record()
    "changes": [],  # tree_changed calls that read the tree
    "hook_calls": 0,  # tree_changed calls, the ones called again included
    "slow_started": 0,
    "slow_done": 0,
    "keeper": 0,  # starts of the restart=True task in this process
    "settings": [],
    "langs": [],
}
HOLD = {"armed": False, "holding": False}


def _keep(name: str, item: dict[str, Any]) -> None:
    items = SEEN[name]
    items.append(item)
    del items[:-KEEP]


def record(ctx: Context, kind: str, a: Any = None, **more: Any) -> dict[str, Any]:
    """What this call got: the host's description of it, the scan ``ctx.tree`` reads, the language
    and, for calls with an ActionContext or ReportContext, that."""
    info = (
        dataclasses.asdict(ctx.call_info) if dataclasses.is_dataclass(ctx.call_info) else vars(ctx.call_info)
    )
    seen: dict[str, Any] = {"kind": kind, "info": dict(info), "lang": ctx.lang, "pid": os.getpid()}
    try:
        seen["tree"] = ctx.tree.scan_id
    except Exception as exc:
        seen["tree"] = f"error {type(exc).__name__}: {exc}"
    if a is not None:
        seen["given"] = a.model_dump(mode="json")
    seen.update(more)
    _keep("calls", seen)
    return seen


class About(Params):
    """A plain method's or a stream's params naming the scan (as a panel's do)."""

    scan_id: str | None = None
    view_id: str | None = None
    node: int | None = None


class Wait(Params):
    seconds: float = 1.0


class Said(Payload):
    said: dict[str, Any]


class Dump(Payload):
    seen: dict[str, Any]
    hold: dict[str, bool]
    pid: int


class Hashed(Payload):
    files: int
    digests: int
    took: float


# ----------------------------------------------------------------------------- reading it back


@plugin.method
def dump(ctx: Context) -> Dump:
    return Dump(seen=SEEN, hold=HOLD, pid=os.getpid())


@plugin.method
def forget(ctx: Context) -> Dump:
    """Start the records afresh (a test's second part)."""
    for key, value in SEEN.items():
        SEEN[key] = [] if isinstance(value, list) else 0
    return Dump(seen=SEEN, hold=HOLD, pid=os.getpid())


# ----------------------------------------------------------------------------- each kind of call


@plugin.method
def about(ctx: Context, p: About) -> Said:
    return Said(said=record(ctx, "method", p))


@plugin.method
def where(ctx: Context, a: ActionContext) -> str:
    """A command's check: always unavailable, the reason says which scan it read."""
    seen = record(ctx, "check", a)
    return f"tree={seen['tree']} node={a.node}"


plugin.command(
    "where_cmd", T("探针：在哪", "Probe: where"), menus=["node"], check="where", action={"open": "settings"}
)


@plugin.method
def refuses(ctx: Context, a: ActionContext) -> str | None:
    raise PluginError(T("坏了", "broken"))


@plugin.method
def breaks(ctx: Context, a: ActionContext) -> str | None:
    raise ValueError("kaputt")


@plugin.method
def dawdles(ctx: Context, a: ActionContext) -> str | None:
    time.sleep(0.4)  # longer than the host waits for a check (50 ms)
    return "too late"


plugin.command(
    "refuse_cmd",
    T("探针：拒绝", "Probe: refuses"),
    menus=["node"],
    check="refuses",
    action={"open": "settings"},
)
plugin.command(
    "break_cmd", T("探针：出错", "Probe: breaks"), menus=["node"], check="breaks", action={"open": "settings"}
)
plugin.command(
    "dawdle_cmd",
    T("探针：慢", "Probe: dawdles"),
    menus=["node"],
    check="dawdles",
    action={"open": "settings"},
)


@plugin.command("act", T("探针：动作", "Probe: act"), menus=["node"])
def act(ctx: Context, a: ActionContext) -> None:
    """A command's action: says where it is, and starts a task about the same scan."""
    record(ctx, "action", a)
    ctx.tasks.start("which", key=a.scan_id or "")


@plugin.background("which", T("探针：哪个扫描", "Probe: which scan"))
def which_task(ctx: Context) -> None:
    record(ctx, "task")


@plugin.node_field(T("探针：哪棵树", "Probe: which tree"))
def which_field(ctx: Context, node: Node) -> str:
    seen = record(ctx, "field", node=node.id, path=node.path)
    return f"tree={seen['tree']} node={node.id}"


@plugin.report(T("探针：报告", "Probe: report"), slot="tab")
def which_report(ctx: Context, c: ReportContext) -> Report:
    seen = record(ctx, "report", c)
    return Report(title=T("探针", "Probe"), blocks=[r.md(f"tree={seen['tree']}")])


@plugin.stream
async def which_stream(ctx: Context, p: About) -> AsyncIterator[Said]:
    yield Said(said=record(ctx, "stream", p))
    await asyncio.Event().wait()  # until unsubscribed


@plugin.color_mode("bands", T("探针分档", "Probe bands"), palette=["muted", "ok", "warn", "danger"])
def bands(c: ColorInput) -> np.ndarray:
    if c.ctx is not None:
        record(c.ctx, "color", rows=len(c.ids))
    return np.ones(len(c.ids), dtype=np.uint8)


@plugin.color_legend("bands")
def bands_legend(ctx: Context) -> list[Text | str]:
    return [T("其他", "Other"), *[b.strip() for b in str(ctx.settings["bands"]).split(",")]]


# ----------------------------------------------------------------------------- hooks


@plugin.on("scan_done")
def scan_done(ctx: Context, scan: ScanInfo) -> None:
    record(ctx, "scan_done", scan=scan.scan_id)


@plugin.on("settings")
def settings_changed(ctx: Context, changed: list[str]) -> None:
    _keep("settings", {"changed": changed, "bands": ctx.settings.get("bands")})


@plugin.on("lang")
def lang_changed(ctx: Context) -> None:
    SEEN["langs"].append(ctx.lang)


@plugin.method
def hold_next(ctx: Context) -> Dump:
    """The next tree_changed call waits until the tree is reorganised (a newer epoch) before it
    reads the tree: it meets STALE_TREE and the host calls it again (round 5's ``rapidslow``)."""
    HOLD["armed"] = True
    return Dump(seen=SEEN, hold=HOLD, pid=os.getpid())


@plugin.on("tree_changed")
def tree_changed(ctx: Context, change: TreeChange) -> None:
    SEEN["hook_calls"] += 1
    record(ctx, "tree_changed", scan=change.scan_id, epoch=change.epoch)
    if HOLD["armed"]:
        HOLD["armed"], HOLD["holding"] = False, True
        try:
            deadline = time.monotonic() + 20.0
            while ctx.scan(change.scan_id).epoch == change.epoch and time.monotonic() < deadline:
                time.sleep(0.02)
        finally:
            HOLD["holding"] = False
    t = ctx.tree  # pinned to change.epoch: STALE_TREE propagates, the host calls again
    ids = [int(f) for f in change.folders]
    read = [int(n.alloc) for n in t.nodes(ids)]
    _keep(
        "changes",
        {
            "scan": change.scan_id,
            "tree": t.scan_id,
            "epoch": change.epoch,
            "tree_epoch": t.epoch,
            "paths": change.paths,
            "before": change.before,
            "after": change.after,
            "read": read,
            "more": change.more,
            "changes": change.changes,
        },
    )


# ----------------------------------------------------------------------------- slow, crashing, reading


@plugin.method
def slow(ctx: Context, p: Wait) -> Said:
    """A plain def that never looks at cancellation (round 4's gap 8)."""
    SEEN["slow_started"] += 1
    time.sleep(p.seconds)
    SEEN["slow_done"] += 1
    return Said(said={"slept": p.seconds})


@plugin.method
async def die(ctx: Context) -> Said:
    os._exit(3)


@plugin.method
def start_keeper(ctx: Context) -> Said:
    info = ctx.tasks.start("keeper")
    return Said(said={"task": info.id})


@plugin.background("keeper", T("探针：一直运行", "Probe: keeps running"), restart=True)
def keeper(ctx: Context) -> None:
    SEEN["keeper"] += 1
    while True:
        ctx.check()
        time.sleep(0.05)


@plugin.method
def hash_all(ctx: Context, p: About) -> Hashed:
    """Every file of the scan through ctx.files (the disk-load limit's permits)."""
    t0 = time.monotonic()
    files = [int(i) for i in ctx.tree.query(kind="file", columns=()).ids]
    digests = ctx.files.hash_many(files)
    return Hashed(files=len(files), digests=sum(d is not None for d in digests), took=time.monotonic() - t0)

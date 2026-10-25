"""Round 5 (fixes only): which tree a plugin's code reads, and reading it while it changes.

- A: a command's ``check`` (palette.check, palette.check_many: the palette, a context menu, the
  details panel) reads the scan of its context as ``ctx.tree``, as call actions, fields, insights,
  hooks, background tasks and streams do; with two scans open it used to read the newest one. A
  check that raises makes the command unavailable with the error as the reason.
- B: a ``tree_changed`` hook that reads the tree while live monitoring reorganises it (a new file
  moves rows: the epoch moves on) is called again with the new ids until it reads one tree, and its
  changes carry each folder's path and size on disk before and after. A call that came with node
  ids reads the tree of their epoch (``ctx.tree`` raises ``STALE_TREE`` once it moved on).
"""

from __future__ import annotations

import asyncio
import os
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife.plugins import host as host_mod
from spaceknife.plugins import live as live_mod
from spaceknife.server.rpc import RpcError
from spaceknife_protocol import palette as ppal
from spaceknife_protocol import view as pv
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import PALETTE_CHECK, PALETTE_CHECK_MANY
from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(240)

TOML = """id = "t.r5trees"
version = "0.1.0"
name = { zh = "第五轮的树", en = "Round five trees" }
entry = "plugin.py"
permissions = ["tree.read"]

[limits]
threads = 2
"""

CODE = """from __future__ import annotations

import time
from collections.abc import AsyncIterator
from typing import Any

from spaceknife_sdk import (
    ActionContext, Context, ErrorCode, Insight, Node, Payload, Plugin, PluginError, ScanInfo, T,
    TreeChange,
)
from spaceknife_sdk.errors import HostError

plugin = Plugin()
SEEN: dict[str, Any] = {"hooks": [], "task": None, "stale": 0, "changes": []}


class Seen(Payload):
    seen: dict[str, Any]


class Where(Payload):
    where: str


@plugin.method
def seen(ctx: Context) -> Seen:
    return Seen(seen=SEEN)


@plugin.method
def check_where(ctx: Context, a: ActionContext) -> str:
    t = ctx.tree
    return f"tree {t.scan_id}|{t.path(a.node) if a.node is not None else ''}"


@plugin.method
def check_raises(ctx: Context, a: ActionContext) -> str | None:
    raise PluginError(T("检查坏了", "The check broke"))


@plugin.method
def check_slow(ctx: Context, a: ActionContext) -> str | None:
    time.sleep(1.5)
    return "too late"


plugin.command("where_cmd", T("在哪", "Where"), menus=["node"], check="check_where", action={"open": "settings"})
plugin.command("raises_cmd", T("出错的检查", "A failing check"), check="check_raises", action={"open": "settings"})
plugin.command("slow_cmd", T("慢的检查", "A slow check"), check="check_slow", action={"open": "settings"})


@plugin.node_field(T("所在", "Where"))
def field_where(ctx: Context, node: Node) -> str:
    return f"{ctx.tree.scan_id}|{node.path}"


@plugin.insight
def insight_where(ctx: Context, node: Node) -> Insight:
    return Insight(title="where", reason=f"{ctx.tree.scan_id}|{node.path}")


@plugin.on("scan_done")
def done(ctx: Context, scan: ScanInfo) -> None:
    SEEN["hooks"].append(["scan_done", scan.scan_id, ctx.tree.scan_id])


@plugin.on("tree_changed")
def changed(ctx: Context, change: TreeChange) -> None:
    try:
        t = ctx.tree
        nodes = t.nodes(change.folders)
        read = [[t.path(f), n.alloc] for f, n in zip(change.folders, nodes)]
    except HostError as e:
        if e.code == ErrorCode.STALE_TREE:
            SEEN["stale"] += 1
        raise
    SEEN["hooks"].append(["tree_changed", change.scan_id, t.scan_id])
    SEEN["changes"].append(
        {
            "epoch": change.epoch,
            "read": read,
            "paths": change.paths,
            "before": change.before,
            "after": change.after,
            "changes": change.changes,
        }
    )


@plugin.background("where_task", T("在哪", "Where"))
def where_task(ctx: Context) -> None:
    SEEN["task"] = ctx.tree.scan_id


@plugin.method
def start_task(ctx: Context) -> Seen:
    ctx.tasks.start("where_task", replace=True)
    return Seen(seen={})


@plugin.stream
async def where_stream(ctx: Context) -> AsyncIterator[Where]:
    yield Where(where=ctx.tree.scan_id)


@plugin.method
def root_size(ctx: Context) -> Where:
    t = ctx.tree
    return Where(where=str(t.node(t.roots[0]).alloc))
"""

TREE_A = make_tree({"a": {"x.bin": 5000, "y.bin": 300}, "same.txt": 10})
TREE_B = make_tree({"b": {"z.bin": 7000}, "c": {"w.bin": 10}, "other.txt": 20})
PID = "t.r5trees"


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    folder = tmp_path / PID
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


async def _seen(host: MockHost) -> dict[str, Any]:
    out: dict[str, Any] = (await host.call("seen"))["seen"]
    return out


async def _until(cond: Callable[[], object], limit: float = 20.0) -> None:
    end = time.monotonic() + limit
    while not cond():
        if time.monotonic() > end:
            raise AssertionError("timed out")
        await asyncio.sleep(0.05)


def _view(host: MockHost, scan_id: str) -> str:
    vhost = host.server.views.open(
        host.session, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    return str(vhost.view.id)


# --------------------------------------------------------------------------- A: the context's scan


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_with_two_scans_every_call_reads_the_scan_of_its_context(
    plugin: Path, process: bool, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_mod, "CHECK_TIMEOUT", 1.0)  # a loaded test machine: a check answers
    async with MockHost.load(plugin, process=process) as host:
        first = host.scan(TREE_A)
        second = host.scan(TREE_B)  # the newest scan: the one in front
        await host.settled()
        hp = host.server.plugins
        assert hp.api.active_scan() == second
        host.scan_id = first
        node = host.find("a/x.bin")
        path = host.path("a/x.bin")
        here = _view(host, first)
        _view(host, second)  # opened later: still the scan "in front"
        assert hp.api.active_scan() == second

        # the palette, a context menu, the details panel: the check reads the first scan's tree
        context = ppal.PaletteContext(tab="scan", scan_id=first, view_id=here, node=node)
        check = host.server.router.get(PALETTE_CHECK.name)
        many = host.server.router.get(PALETTE_CHECK_MANY.name)
        assert check is not None and many is not None
        one = await check.handler(
            host.session, ppal.PaletteCheckParams(id=f"{PID}.where_cmd", context=context)
        )
        assert (one.enabled, one.status) == (False, f"tree {first}|{path}")  # was: the second scan's
        got = await many.handler(
            host.session,
            ppal.PaletteCheckManyParams(
                ids=[f"{PID}.where_cmd", f"{PID}.raises_cmd", f"{PID}.slow_cmd"], context=context
            ),
        )
        by_id = {c.id: c for c in got.commands}
        assert by_id[f"{PID}.where_cmd"].status == f"tree {first}|{path}"
        # a check that raises: unavailable, with why (was: available)
        failed = by_id[f"{PID}.raises_cmd"]
        assert not failed.enabled and failed.status.startswith("检查出错：") and "检查坏了" in failed.status
        # one that does not answer in time: available
        assert by_id[f"{PID}.slow_cmd"].enabled
        # MockHost asks the same way
        assert await host.command_available("where_cmd", node=node) == f"tree {first}|{path}"

        # a field and an insight of the first scan
        fields = await host.fields(node=node)
        assert [f.value for f in fields if f.plugin == PID] == [f"{first}|{path}"]
        cards = await host.insights(node=node)
        assert [c.reason for c in cards if c.plugin == PID] == [f"{first}|{path}"]

        # hooks about the first scan
        hp.fire("scan_done", scan=hp.api.scan_info(hp.server.scans.scans[first]))
        hp.live.tree_changed(first, {node: 0})
        await host.settled()
        hooks = (await _seen(host))["hooks"]
        assert ["scan_done", first, first] in hooks and ["tree_changed", first, first] in hooks

        # a background task started from a call about the first scan
        await host.call("start_task")
        await _until(lambda: any(t.name == "where_task" and t.state == "done" for t in host.tasks()))
        assert (await _seen(host))["task"] == first

        # a stream whose params name the first scan (a panel's)
        sub = await hp.open_topic(host.session, f"plugin.{PID}.where_stream", {"scan_id": first}, None)
        assert sub is not None
        await _until(lambda: any(n == f"plugin.{PID}.where_stream" for n, _p in host.session.sent))
        sent = [p for n, p in host.session.sent if n == f"plugin.{PID}.where_stream"]
        assert sent[0]["value"] == {"where": first}
        sub.close()


async def test_a_check_without_a_view_reads_the_scan_of_the_tab(plugin: Path) -> None:
    from spaceknife.palette.commands import Context as PaletteContext

    async with MockHost.load(plugin) as host:
        first = host.scan(TREE_A)
        host.scan(TREE_B)
        await host.settled()
        hp = host.server.plugins
        assert hp.action_context(PaletteContext(tab="scan", has_scan=True, scan_id=first)) == {
            "scan_id": first
        }


async def test_a_call_reads_the_tree_of_its_node_ids(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        sid = host.scan(TREE_A)
        await host.settled()
        hp = host.server.plugins
        epoch = int(host.store.epoch)
        size = (await hp.call(PID, "method", "root_size", {}, ctx={"scan_id": sid, "epoch": epoch}))[0]
        assert size == {"where": str(host.node("").alloc)}
        # ids of an earlier epoch (the tree was reorganised since the host took them): not read
        # against the new tree, STALE_TREE
        with pytest.raises(RpcError) as err:
            await hp.call(PID, "method", "root_size", {}, ctx={"scan_id": sid, "epoch": epoch - 1})
        assert err.value.code == ErrorCode.STALE_TREE


def test_a_tree_of_a_call_refuses_columns_of_another_epoch() -> None:
    from spaceknife_sdk._link import Image
    from spaceknife_sdk.errors import HostError
    from spaceknife_sdk.tree import TreeView

    class Link:
        def image(self, scan_id: str, columns: list[str]) -> Image:
            cols = {c: np.zeros(3, dtype=np.int64) for c in columns}
            return Image(scan_id=scan_id, version=2, epoch=5, n=3, columns=cols)

        def request(self, method: str, params: dict[str, Any]) -> tuple[Any, list[bytes]]:
            return {"node": 2, "epoch": 5}, []

    info = {"scan_id": "s", "epoch": 4, "n": 3, "roots": [1]}
    pinned = TreeView(Link(), info, pinned=True)  # type: ignore[arg-type]
    with pytest.raises(HostError) as err:
        pinned.col("size")
    assert err.value.code == ErrorCode.STALE_TREE
    with pytest.raises(HostError):
        pinned.find("/x")  # an id of the newer tree
    free = TreeView(Link(), info)  # type: ignore[arg-type]
    assert len(free.col("size")) == 3 and free.epoch == 5  # not of a call: follows the newest


# --------------------------------------------------------------------------- B: tree_changed and epochs


def _churn(host: MockHost, sid: str, plan: list[str]) -> list[str]:
    """The tree is reorganised (its epoch moves on; ids stay, the rows did not really move) right
    before the host answers the plugin's requests named in ``plan``, one after the other: the
    first ``host.tree`` of it, then the next ``host.node`` … The list empties as they come."""
    hp = host.server.plugins
    api = hp.api
    real = api.handle

    async def handle(rec: Any, method: str, params: dict[str, Any], frames: list[bytes]) -> Any:
        if plan and method == plan[0]:
            plan.pop(0)
            hp.server.scans.scans[sid].store.epoch += 1
            hp.server.views.remap(sid, {}, lambda ids: np.asarray(ids, dtype=np.int64))
        return await real(rec, method, params, frames)

    api.handle = handle
    return plan


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_tree_changed_reads_one_tree_while_the_epoch_moves_on(plugin: Path, process: bool) -> None:
    from loguru import logger

    lines: list[str] = []
    sink = logger.add(lambda m: lines.append(str(m)), level="WARNING")
    try:
        async with MockHost.load(plugin, process=process) as host:
            sid = host.scan(TREE_A)
            await host.settled()
            hp = host.server.plugins
            x, a = host.find("a/x.bin"), host.find("a")
            epoch = int(host.store.epoch)
            # three reorganisations while the hook reads: between the call and its first look at
            # ctx.tree, then twice while it reads the folders (each time it met the new tree, it
            # was called again with the folders' new ids)
            plan = _churn(host, sid, ["host.tree", "host.node", "host.paths"])
            tot, root = host.store.cols["tot_alloc"], host.find("")
            # x.bin grew by 300 bytes on disk: a and the root took 300 bytes less before
            hp.live.tree_changed(sid, {x: 300}, {a: int(tot[a]) - 300, root: int(tot[root]) - 300})
            await host.settled()
            assert plan == [] and int(host.store.epoch) == epoch + 3
            seen = await _seen(host)
            assert seen["stale"] == 3
            [change] = seen["changes"]  # delivered once, whole
            assert change["paths"] == [host.path("a")] and change["epoch"] == epoch + 3
            assert change["read"] == [[host.path("a"), change["after"][0]]]  # of the tree it is about
            assert change["after"] == [int(host.store.cols["tot_alloc"][a])]
            assert change["before"] == [change["after"][0] - 300]
            assert not [line for line in lines if "tree_changed" in line]
    finally:
        logger.remove(sink)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_changes_a_hook_kept_missing_come_with_the_next_ones(
    plugin: Path, process: bool, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(live_mod, "TREE_EVENT_S", 0.2)
    async with MockHost.load(plugin, process=process) as host:
        sid = host.scan(TREE_A)
        await host.settled()
        hp = host.server.plugins
        # every try of the first delivery meets a reorganised tree: its changes wait for the next
        plan = _churn(host, sid, ["host.tree"] * (host_mod.HOOK_STALE_RETRIES + 1))
        tot, a, top = host.store.cols["tot_alloc"], host.find("a"), host.find("")
        size_a, size_top = int(tot[a]), int(tot[top])  # (the tree stays: as after both changes)
        hp.live.tree_changed(sid, {host.find("a/x.bin"): 100}, {a: size_a - 100, top: size_top - 107})
        await _until(lambda: not plan)
        hp.live.tree_changed(sid, {host.find("same.txt"): 7}, {top: size_top - 7})  # meanwhile
        await host.settled()
        seen = await _seen(host)
        assert seen["stale"] == host_mod.HOOK_STALE_RETRIES + 1
        [change] = seen["changes"]
        root, folder = host.path(""), host.path("a")
        assert change["paths"] == [folder, root] and change["changes"] == 2
        assert change["before"] == [change["after"][0] - 100, change["after"][1] - 107]


async def test_a_hook_that_always_meets_a_new_tree_is_given_up_on(
    plugin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from loguru import logger

    monkeypatch.setattr(live_mod, "TREE_EVENT_S", 0.05)
    monkeypatch.setattr(host_mod, "STALE_RETRY_S", 0.0)
    lines: list[str] = []
    sink = logger.add(lambda m: lines.append(str(m)), level="WARNING")
    try:
        async with MockHost.load(plugin) as host:
            sid = host.scan(TREE_A)
            await host.settled()
            hp = host.server.plugins
            _churn(host, sid, ["host.tree"] * 10_000)
            hp.live.tree_changed(sid, {host.find("a/x.bin"): 1})
            await host.settled()
            tries = (live_mod.TREE_REQUEUES + 1) * (host_mod.HOOK_STALE_RETRIES + 1)
            assert (await _seen(host))["stale"] == tries
            assert any("reorganised every time" in line for line in lines)
            assert not hp.live._tree and not hp.live._requeued
    finally:
        logger.remove(sink)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_files_written_while_the_hook_reads_move_real_rows(
    plugin: Path, tmp_path: Path, process: bool
) -> None:
    """The monitor's own relist (what a new file does: the folder's rows move, the epoch goes on)
    happens while the hook reads; every write still reaches the hook, read from one tree."""
    from spaceknife.monitor.base import Change

    folder = tmp_path / "watched"
    for name in ("one", "two"):
        (folder / name).mkdir(parents=True)
        (folder / name / "seed.bin").write_bytes(b"s" * 100)
    async with MockHost.load(plugin, process=process) as host:
        sid = await host.scan_dir(folder)
        await host.settled()
        hp = host.server.plugins
        mon = hp.server.scans.scans[sid].monitor
        assert mon is not None
        api = hp.api
        real = api.handle
        written: list[Path] = []

        async def handle(rec: Any, method: str, params: dict[str, Any], frames: list[bytes]) -> Any:
            if method in ("host.node", "host.paths") and len(written) < 4:
                target = folder / ("one", "two")[len(written) % 2] / f"new{len(written)}.bin"
                target.write_bytes(b"n" * 5000)
                written.append(target)
                epoch = int(hp.server.scans.scans[sid].store.epoch)
                await mon._process([Change("created", str(target), is_dir=False)])
                assert int(hp.server.scans.scans[sid].store.epoch) > epoch  # rows moved
            return await real(rec, method, params, frames)

        api.handle = handle
        start = int(host.store.epoch)
        hp.live.tree_changed(sid, {host.find("one/seed.bin"): 0})
        await _until(lambda: len(written) == 4 and not hp.live._tree and not hp.live._tree_tasks, 30.0)
        await host.settled()
        assert int(host.store.epoch) >= start + 4
        seen = await _seen(host)
        assert seen["stale"] >= 1
        reached: set[str] = set()
        for change in seen["changes"]:
            sizes = {p: s for p, s in change["read"]}
            assert sorted(sizes) == sorted(change["paths"])  # read from the tree the change is of
            for p, after in zip(change["paths"], change["after"], strict=True):
                assert sizes[p] == after
            reached |= set(change["paths"])
        assert {str(folder / "one"), str(folder / "two")} <= {os.path.normpath(p) for p in reached}
        # the folders' sizes: what the new files added is in after - before
        last = {
            p: (b, a)
            for c in seen["changes"]
            for p, b, a in zip(c["paths"], c["before"], c["after"], strict=True)
        }
        assert all(b is None or b <= a for b, a in last.values())

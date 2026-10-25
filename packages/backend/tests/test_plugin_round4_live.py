"""Round 4, what a plugin hears of while it runs (docs/plugins.md 「第三轮之后的改进」): the
``tree_changed`` hook (coalesced per scan, folder ids that follow remaps, ``ctx.tree`` that scan)
and ``ScanInfo.monitoring``; the ``start`` hook on first enable and on a dev reload; the
``snapshots`` hook and the ``snapshots.changed`` event; a command's method returning a
comparison to open; ``ctx.notify``; a code colour mode's legend and the ``settings`` hook seeing the
new settings."""

from __future__ import annotations

import asyncio
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(240)

TOML = """id = "t.r4live"
version = "0.1.0"
name = { zh = "第四轮变化", en = "Round four changes" }
entry = "plugin.py"
permissions = ["tree.read", "snapshots.read", "notify"]

[[settings]]
name = "bands"
title = { zh = "分档", en = "Bands" }
default = "7d, 30d"
"""

CODE = """from __future__ import annotations

from typing import Any

import numpy as np

from spaceknife_sdk import (
    ColorInput, CompareOpen, Context, Payload, Plugin, SnapshotsChange, T, Text, TreeChange
)

plugin = Plugin()
SEEN: dict[str, Any] = {"starts": [], "settings": [], "trees": [], "snapshots": []}


class Seen(Payload):
    seen: dict[str, Any]


@plugin.method
def seen(ctx: Context) -> Seen:
    return Seen(seen=SEEN)


@plugin.on("start")
def started(ctx: Context) -> None:
    SEEN["starts"].append(ctx.settings["bands"])


@plugin.on("settings")
def settings_changed(ctx: Context, changed: list[str]) -> None:
    SEEN["settings"].append([changed, ctx.settings["bands"]])


@plugin.on("tree_changed")
def tree_changed(ctx: Context, change: TreeChange) -> None:
    t = ctx.tree
    SEEN["trees"].append(
        {
            "scan_id": change.scan_id,
            "tree": t.scan_id,
            "epoch": change.epoch == t.epoch,
            "version": change.version,
            "folders": sorted(t.name(f) for f in change.folders),
            "changes": change.changes,
            "more": change.more,
        }
    )


@plugin.on("snapshots")
def snapshots(ctx: Context, change: SnapshotsChange) -> None:
    SEEN["snapshots"].append({"saved": change.saved, "removed": change.removed})


@plugin.color_mode("bands", T("分档", "Bands"), palette=["muted", "ok", "warn", "danger"])
def bands(c: ColorInput) -> np.ndarray:
    return np.ones(len(c.ids), dtype=np.uint8)


@plugin.color_legend("bands")
def bands_legend(ctx: Context) -> list[Text | str]:
    return [T("其他", "Other"), *[b.strip() for b in str(ctx.settings["bands"]).split(",")]]


@plugin.method
def notice(ctx: Context) -> Seen:
    first = ctx.notify(
        T("磁盘涨了", "The disk grew"),
        T("下载文件夹多了 3 GB", "Downloads grew by 3 GB"),
        action="show",
        button=T("看看", "Show"),
        args={"folder": "Downloads"},
        tone="warn",
    )
    second = ctx.notify("again")
    return Seen(seen={"first": first, "second": second})


plugin.command("show", T("看看增长", "Show the growth"), action={"call": "noop"})


@plugin.method
def noop(ctx: Context) -> None:
    return None


@plugin.command("compare_last", T("和上次比", "Compare with last time"))
def compare_last(ctx: Context) -> CompareOpen:
    snaps = ctx.snapshots.list()
    return CompareOpen(base=snaps[0].path, target="now")
"""

TREE = make_tree({"a.log": b"a" * 5000, "b.txt": b"b" * 3000, "sub": {"c.log": b"c" * 2000, "d.bin": b"d"}})


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    folder = tmp_path / "t.r4live"
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


async def _seen(host: MockHost) -> dict[str, Any]:
    out: dict[str, Any] = (await host.call("seen"))["seen"]
    return out


async def _until(cond: Callable[[], object], limit: float = 15.0) -> None:
    end = time.monotonic() + limit
    while not cond():
        if time.monotonic() > end:
            raise AssertionError("timed out")
        await asyncio.sleep(0.05)


# --------------------------------------------------------------------------- gap 14: the start hook


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_the_start_hook_runs_on_first_enable_and_on_a_dev_reload(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        await host.settled()
        # the process started to describe the plugin, before its hooks were registered: start ran
        # anyway, once they were (and with its settings)
        assert (await _seen(host))["starts"] == ["7d, 30d"]
        if not process:
            return
        hp = host.server.plugins
        await hp.restart("t.r4live")  # what `plugin dev` does when a file changed
        await host.settled()
        assert (await _seen(host))["starts"] == ["7d, 30d"]  # a new process: start ran in it once


async def test_a_hook_the_registry_lacks_is_logged(plugin: Path) -> None:
    from loguru import logger

    from spaceknife.extensions import EXTENSIONS

    lines: list[str] = []
    sink = logger.add(lambda m: lines.append(str(m)), level="WARNING")
    try:
        async with MockHost.load(plugin) as host:
            await host.settled()
            hp = host.server.plugins
            rec = hp.records["t.r4live"]
            EXTENSIONS.unregister_owner("t.r4live")
            await hp._hook(rec, "start", hp.places.nowhere())
            assert any("hook start of t.r4live not run" in line for line in lines)
    finally:
        logger.remove(sink)


# --------------------------------------------------------------------------- gap 9: legends, settings


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_the_legend_and_the_settings_hook_see_the_new_settings(
    plugin: Path, process: bool, monkeypatch: pytest.MonkeyPatch
) -> None:
    from spaceknife.extensions import EXTENSIONS

    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        await host.settled()
        hp = host.server.plugins
        # the app tells every plugin process of new settings one after the other: one slow pipe
        # (another plugin busy reading a big frame) holds up the others' (the round-3 ageviz case)
        push = hp.push_update

        async def slow_push(fields: dict[str, Any]) -> None:
            await asyncio.sleep(0.5)
            await push(fields)

        monkeypatch.setattr(hp, "push_update", slow_push)
        mode = EXTENSIONS.get("color_mode", "t.r4live.bands")
        assert mode is not None

        def labels() -> list[str]:
            return [t for t, _c in hp.legend_of(mode, "zh-CN")]

        labels()  # asked in the background
        await host.settled()
        assert labels() == ["其他", "7d", "30d"]
        version = EXTENSIONS.version
        host.server.settings.update({"plugin.t.r4live.bands": "1d, 2d, 3d"})
        await host.settled()
        # was: the legend was asked before the plugin had the new value, and kept as the new one
        assert labels() == ["其他", "1d", "2d", "3d"]
        assert EXTENSIONS.version > version  # the windows read it again
        assert (await _seen(host))["settings"] == [[["bands"], "1d, 2d, 3d"]]


# --------------------------------------------------------------------------- gap 4: tree_changed


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_tree_changed_comes_once_a_second_with_the_folders(plugin: Path, process: bool) -> None:
    from spaceknife.plugins import live

    async with MockHost.load(plugin, process=process) as host:
        sid = host.scan(TREE)
        await host.settled()
        await host.changed("sub/c.log", "a.log")
        trees = (await _seen(host))["trees"]
        root = host.store.name(1)
        assert trees == [
            {
                "scan_id": sid,
                "tree": sid,  # ctx.tree is the scan that changed
                "epoch": True,
                "version": int(host.store.version),
                "folders": sorted([root, "sub"]),
                "changes": 2,
                "more": 0,
            }
        ]
        # a burst comes together: one call for three changes within the second
        hp = host.server.plugins
        for rel in ("b.txt", "sub/d.bin", "sub"):
            hp.live.tree_changed(sid, {host.find(rel): 1})
        await host.settled()
        trees = (await _seen(host))["trees"]
        assert len(trees) == 2 and trees[1]["changes"] == 3 and trees[1]["folders"] == sorted([root, "sub"])
        # bounded: more folders than one call lists are counted
        many = {i: 1 for i in range(len(host.store))}
        due = live._TreeDue(epoch=0)
        due.add(list(range(live.TREE_FOLDERS + 10)), len(many))
        assert len(due.folders) == live.TREE_FOLDERS and due.more == 10


async def test_pending_folders_follow_a_remap(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        sid = host.scan(TREE)
        await host.settled()
        hp = host.server.plugins
        sub = host.find("sub")
        hp.live.tree_changed(sid, {host.find("sub/c.log"): 1})
        assert list(hp.live._tree[("t.r4live", sid)].folders) == [sub]
        host.store.epoch += 1  # rows moved (a relist, a compaction): ids are new
        hp.server.views.remap(sid, {}, lambda ids: np.where(np.asarray(ids) == sub, 1, -1))
        due = hp.live._tree[("t.r4live", sid)]
        assert list(due.folders) == [1] and due.epoch == host.store.epoch
        await host.settled()
        assert (await _seen(host))["trees"][0]["folders"] == [host.store.name(1)]


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_file_written_in_a_monitored_scan_reaches_the_hook(
    plugin: Path, tmp_path: Path, process: bool
) -> None:
    folder = tmp_path / "watched"
    (folder / "deep").mkdir(parents=True)
    (folder / "deep" / "x.bin").write_bytes(b"x" * 100)
    async with MockHost.load(plugin, process=process) as host:
        sid = await host.scan_dir(folder)
        await host.settled()
        hp = host.server.plugins

        def monitoring() -> str:
            return str(hp.api.scan_info(hp.server.scans.scans[sid])["monitoring"])

        mon = hp.server.scans.scans[sid].monitor
        await _until(lambda: monitoring() not in ("off", "starting") and mon.fed)
        assert monitoring() in ("running", "degraded")
        await asyncio.sleep(0.3)  # inotify places its watches on its own thread
        (folder / "deep" / "y.bin").write_bytes(b"y" * 5000)
        await _until(lambda: bool(hp.live._tree) or bool(hp.live._tree_tasks), 20.0)
        await host.settled()
        trees = (await _seen(host))["trees"]
        assert trees and trees[-1]["scan_id"] == sid and "deep" in trees[-1]["folders"]


async def test_scans_say_whether_they_are_monitored(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        sid = host.scan(TREE)
        await host.settled()
        info = host.server.plugins.api.scan_info(host.server.scans.scans[sid])
        assert info["monitoring"] == "off"  # a snapshot-like scan: nothing watches it
        from spaceknife_sdk import ScanInfo

        assert ScanInfo.model_validate(info).monitoring == "off"


# --------------------------------------------------------------------------- gap 15: snapshots


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_the_snapshots_hook_and_event(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        await host.settled()
        path = host.snapshot()
        await host.settled()
        assert (await _seen(host))["snapshots"] == [{"saved": [path], "removed": []}]
        events = [p for n, p in host.session.sent if n == "snapshots.changed"]
        assert events == [{"saved": [path], "removed": []}]
        # pruning, as an automatic save does: both in one call
        hp = host.server.plugins
        hp.server.scans._snapshots_changed(["/s/new.sksnap"], ["/s/old.sksnap"])
        await host.settled()
        assert (await _seen(host))["snapshots"][-1] == {
            "saved": ["/s/new.sksnap"],
            "removed": ["/s/old.sksnap"],
        }
        # a command's method returns a comparison to open (the window does it)
        assert await host.call("compare_last") == {
            "type": "compare",
            "base": path,
            "target": "now",
            "scan_id": None,
        }


async def test_the_snapshots_hook_needs_snapshots_read(plugin: Path) -> None:
    async with MockHost.load(plugin, grant=["tree.read", "notify"]) as host:
        host.scan(TREE)
        await host.settled()
        with pytest.raises(PermissionError, match=r"snapshots\.read"):
            await host.hook("snapshots")
        host.snapshot()
        await host.settled()
        assert (await _seen(host))["snapshots"] == []


def test_check_asks_for_the_permissions_of_notify_snapshots_and_compare(plugin: Path) -> None:
    from spaceknife.plugins.check import check_plugin

    toml = plugin / "plugin.toml"
    toml.write_text(
        toml.read_text(encoding="utf-8").replace(', "snapshots.read", "notify"', ""), encoding="utf-8"
    )
    result = check_plugin(plugin, contracts=False)
    missing = sorted(i.text() for i in result.issues if i.code == "missing_permission")
    assert any("'notify'" in m for m in missing) and any("'snapshots.read'" in m for m in missing)


# --------------------------------------------------------------------------- gap 16: notices


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_notify_sends_a_notice_with_a_button(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        assert (await host.call("notice"))["seen"] == {"first": True, "second": False}  # one per 5 s
        await host.settled()
        assert len(host.notices) == 1
        n = host.notices[0]
        assert n["plugin"] == "t.r4live" and n["title"] == "磁盘涨了" and n["body"] == "下载文件夹多了 3 GB"
        assert (
            n["command"] == "t.r4live.show" and n["button"] == "看看" and n["args"] == {"folder": "Downloads"}
        )
        assert n["tone"] == "warn" and n["id"]


async def test_notices_are_checked_by_the_host(plugin: Path) -> None:
    from spaceknife.plugins import live

    async with MockHost.load(plugin) as host:
        await host.settled()
        hp = host.server.plugins
        rec = hp.records["t.r4live"]
        hp.live.notice(
            rec, {"title": "x" * 500, "command": "other.plugin.cmd", "args": {"a": "y" * 5000}}, None
        )
        n = host.notices[-1]
        assert len(n["title"]) == 120 and n["command"] is None and n["args"] == {}
        hp.live.notice(rec, {"title": "too soon"}, None)  # within NOTICE_GAP_S of the last
        assert len(host.notices) == 1
        hp.live._notice_at.clear()
        rec.granted = frozenset({"tree.read"})
        hp.live.notice(rec, {"title": "no permission"}, None)
        assert len(host.notices) == 1
        assert live.NOTICE_GAP_S >= 1


async def test_notify_needs_its_permission(plugin: Path) -> None:
    from spaceknife_sdk import RequestError

    async with MockHost.load(plugin, grant=["tree.read", "snapshots.read"]) as host:
        with pytest.raises(RequestError, match="notify"):
            await host.call("notice")

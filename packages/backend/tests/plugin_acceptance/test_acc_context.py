"""Acceptance: every kind of plugin call reads the scan it is about, with two scans open (round 5's
probe ``twoscans``; rounds 2-5 each found one kind that read the newest scan instead: checks,
hooks, tasks, streams). The older scan is the one the calls are about; the newer one is in front."""

from __future__ import annotations

import asyncio
import shutil
from pathlib import Path
from typing import Any

import pytest

from .harness import FIXTURES, Backend

pytestmark = pytest.mark.timeout(240)


def _disk(data: Path) -> Path:
    disk = data / "disk"
    for rel, size in (
        ("docs/a.bin", 5000),
        ("docs/b.txt", 300),
        ("exports/fixture.json", 1200),
        ("photos/2021/x.jpg", 40_000),
        ("photos/y.jpg", 20_000),
    ):
        f = disk / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(b"d" * size)
    return disk


async def _install(backend: Backend, tmp_path: Path, runtime: str) -> str:
    if runtime == "process":
        return await backend.install(FIXTURES / "acc.probe")
    # developer mode: a plugin that asks for it runs inside the backend
    await backend.settings({"advanced.plugin_dev_mode": True})
    folder = tmp_path / "acc.probe"
    shutil.copytree(FIXTURES / "acc.probe", folder, ignore=shutil.ignore_patterns("__pycache__"))
    toml = folder / "plugin.toml"
    toml.write_text(
        toml.read_text(encoding="utf-8").replace(
            'entry = "plugin.py"', 'entry = "plugin.py"\nruntime = "inprocess"'
        ),
        encoding="utf-8",
    )
    pid = await backend.install(folder)
    assert (await backend.plugin(pid))["runtime"] == "inprocess"
    return pid


async def _one(backend: Backend, pid: str, kind: str, cond: Any = None) -> dict[str, Any]:
    """The newest record of a kind of call (that meets ``cond``), once there is one."""

    async def probe() -> list[dict[str, Any]]:
        return [c for c in await backend.calls(pid, kind) if cond is None or cond(c)]

    found = await backend.until(f"a {kind} call", probe)
    return found[-1]


@pytest.mark.parametrize("runtime", ["process", "inprocess"])
async def test_each_kind_of_call_reads_the_scan_it_is_about(
    backend: Backend, data: Path, tmp_path: Path, runtime: str
) -> None:
    pid = await _install(backend, tmp_path, runtime)
    disk = _disk(data)
    older = await backend.scan(disk)
    newer = await backend.scan(disk / "docs")  # in front from now on
    v_old = await backend.view(older)
    v_new = await backend.view(newer)
    fixture = await backend.find(older, disk / "exports" / "fixture.json")
    a_bin = await backend.find(newer, disk / "docs" / "a.bin")
    assert fixture is not None and a_bin is not None
    where = f"{pid}.where_cmd"

    # a command's check (context menu: palette.check_many; keyboard: palette.check). A check that
    # does not answer within 50 ms leaves the command available: ask until it has answered.
    for sid, vid, node in ((older, v_old, fixture), (newer, v_new, a_bin)):
        got = await backend.until(
            f"the check on {sid}",
            lambda sid=sid, vid=vid, node=node: backend.check_many([where], sid, vid, node),
            lambda r: r[where][1] != "",
        )
        assert got[where] == (False, f"tree={sid} node={node}")
    single = await backend.until(
        "palette.check on the older scan without a view",
        lambda: backend.call(
            "palette.check",
            {
                "id": where,
                "context": {"tab": "scan", "scan_id": older, "view_id": None, "basket": 0, "node": fixture},
            },
        ),
        lambda r: bool(r.get("status")),
    )
    assert single["status"].startswith(f"tree={older} ")  # the node: test_every_kind_gets_the_whole_context

    # a node field of the details panel
    for sid, node in ((older, fixture), (newer, a_bin)):
        fields = await backend.until(
            f"the field on {sid}",
            lambda sid=sid, node=node: backend.fields(pid, sid, node),
            lambda f: bool(f) and f[0].get("value") not in (None, ""),
        )
        assert fields[0]["value"] == f"tree={sid} node={node}"

    # a plain method naming the scan, a command's action (with its ActionContext), a report
    said = (await backend.pcall(pid, "about", scan_id=older))["said"]
    assert said["tree"] == older and said["info"]["scan_id"] == older
    await backend.pcall(
        pid,
        "act",
        scan_id=older,
        view_id=v_old,
        node=fixture,
        selection=[fixture],
        args={},
        zoom_node=None,
        filter="",
    )
    action = await _one(backend, pid, "action")
    assert action["tree"] == older and action["given"]["scan_id"] == older
    await backend.pcall(pid, "which_report", scan_id=older, node=fixture)
    assert (await _one(backend, pid, "report"))["tree"] == older

    # a background task the action started: it reads the scan of the call that started it
    task = await _one(backend, pid, "task")
    assert task["tree"] == older
    assert any(t["name"] == "which" and t["key"] == older for t in await backend.tasks(pid))

    # a stream about the older scan (a panel's subscription)
    heard: list[dict[str, Any]] = []
    topic = f"plugin.{pid}.which_stream"
    backend.conn.on_raw(topic, lambda p: heard.append(p))
    sub = await backend.call("$/subscribe", {"topic": topic, "params": {"scan_id": older}})

    async def streamed() -> list[dict[str, Any]]:
        await asyncio.sleep(0)
        return heard

    await backend.until("the stream's first value", streamed)
    assert heard[0]["value"]["said"]["tree"] == older
    await backend.call("$/unsubscribe", {"sub": sub["sub"]})

    # hooks: scan_done of each scan, tree_changed of the older one
    done = {c["scan"]: c["tree"] for c in await backend.calls(pid, "scan_done")}
    assert done == {older: older, newer: newer}
    await backend.watching(older)
    (disk / "photos" / "new.bin").write_bytes(b"n" * 30_000)
    changed = await backend.until(
        "tree_changed of the older scan",
        lambda: backend.dump(pid),
        lambda d: any(c["scan"] == older for c in d["changes"]),
    )
    change = next(c for c in changed["changes"] if c["scan"] == older)
    assert change["tree"] == older and change["tree_epoch"] == change["epoch"]

    # a colour mode painting a view of the older scan
    await backend.view(older, color_mode=f"{pid}.bands")
    assert (await _one(backend, pid, "color"))["tree"] == older


async def test_checks_that_fail_or_dawdle(backend: Backend, probe: str, data: Path) -> None:
    """A check that raises makes its command unavailable with the error as the reason (a
    PluginError: its text; another exception: its type and message, round 5's new problem 5);
    one that takes longer than the host waits leaves it available. The process stays."""
    disk = _disk(data)
    sid = await backend.scan(disk)
    vid = await backend.view(sid)
    node = await backend.find(sid, disk / "docs" / "a.bin")
    ids = [f"{probe}.{c}" for c in ("refuse_cmd", "break_cmd", "dawdle_cmd")]
    pid0 = (await backend.pcall(probe, "dump"))["pid"]
    got = await backend.until(
        "the failing checks' reasons",
        lambda: backend.check_many(ids, sid, vid, node),
        lambda r: r[ids[0]][1] != "" and r[ids[1]][1] != "",
    )
    assert got[ids[0]] == (False, "检查出错：坏了"), got
    assert got[ids[1]] == (False, "检查出错：ValueError: kaputt"), got
    assert got[ids[2]] == (True, ""), got
    process = (await backend.plugin(probe))["process"]
    assert process["pid"] == pid0 and process["restarts"] == 0, process


def _wants(seen: dict[str, Any], want: dict[str, Any]) -> list[str]:
    """What a call's context lacks: each wanted value missing from the call (as ctx.call_info and
    ctx.lang give it, or the call's ActionContext / ReportContext)."""
    info = {**seen["info"], "lang": seen["lang"], "tree": seen["tree"]}
    given = seen.get("given") or {}
    out = []
    for k, v in want.items():
        got = info.get(k, given.get(k))
        if got != v:
            out.append(f"{seen['kind']}: {k} = {got!r}, want {v!r}")
    return out


@pytest.mark.xfail(
    strict=True,
    reason="收尾第 3 步（一处把调用的上下文交给每种调用）之前，各种调用拿到的上下文还不齐；那一步合并后去掉这个标记",
)
async def test_every_kind_gets_the_whole_context(backend: Backend, probe: str, data: Path) -> None:
    """One place hands a call's context to every kind of call: the scan, the view, the node, the
    selection, the zoom root, the filter and the language, the tree's epoch with node ids. Each
    call here is about the older scan's view, zoomed into photos, filtered, y.jpg selected."""
    pid = probe
    disk = _disk(data)
    older = await backend.scan(disk)
    await backend.scan(disk / "docs")
    view = await backend.view(older)
    photos = await backend.find(older, disk / "photos")
    y = await backend.find(older, disk / "photos" / "y.jpg")
    assert photos is not None and y is not None
    await backend.call("view.zoom", {"view_id": view, "key": photos})
    await backend.call("view.filter", {"view_id": view, "text": "*.jpg"})
    await backend.call("view.select", {"view_id": view, "key": y})
    where = f"{pid}.where_cmd"
    on_view = {"scan_id": older, "view_id": view, "zoom_node": photos, "filter": "*.jpg", "lang": "zh-CN"}

    await backend.until(
        "the check",
        lambda: backend.check_many([where], older, view, y),
        lambda r: r[where][1] != "",
    )
    await backend.until(
        "the check without a view",
        lambda: backend.call(
            "palette.check",
            {
                "id": where,
                "context": {"tab": "scan", "scan_id": older, "view_id": None, "basket": 0, "node": y},
            },
        ),
        lambda r: bool(r.get("status")),
    )
    await backend.pcall(pid, "about", scan_id=older, view_id=view, node=y)
    await backend.pcall(pid, "which_report", scan_id=older, view_id=view, node=y)
    await backend.pcall(
        pid,
        "act",
        scan_id=older,
        view_id=view,
        node=y,
        selection=[y],
        zoom_node=photos,
        filter="*.jpg",
        args={},
    )
    topic = f"plugin.{pid}.which_stream"
    sub = await backend.call(
        "$/subscribe", {"topic": topic, "params": {"scan_id": older, "view_id": view, "node": y}}
    )
    await backend.until(
        "a field",
        lambda: backend.fields(pid, older, y),
        lambda f: bool(f) and f[0].get("value") not in (None, ""),
    )
    await backend.until(
        "every kind",
        lambda: backend.dump(pid),
        lambda d: {"task", "stream"} <= {c["kind"] for c in d["calls"]},
    )
    await backend.call("$/unsubscribe", {"sub": sub["sub"]})
    calls = (await backend.dump(pid))["calls"]

    def last(kind: str, cond: Any = None) -> dict[str, Any]:
        return [c for c in calls if c["kind"] == kind and (cond is None or cond(c))][-1]

    missing: list[str] = []
    check = last("check", lambda c: c["info"].get("view_id") == view)
    missing += _wants(check, {**on_view, "node": y, "selection": [y]})
    missing += _wants(
        last("check", lambda c: c["info"].get("view_id") is None), {"scan_id": older, "node": y}
    )
    for kind in ("method", "report", "stream"):
        missing += _wants(last(kind), {**on_view, "node": y, "selection": [y]})
    missing += _wants(last("action"), {**on_view, "node": y, "selection": [y]})
    missing += _wants(last("field"), {"scan_id": older, "node": y, "lang": "zh-CN"})
    missing += _wants(last("task"), {"scan_id": older, "lang": "zh-CN"})
    missing += _wants(last("scan_done", lambda c: c["scan"] == older), {"scan_id": older, "lang": "zh-CN"})
    for c in calls:
        if c["kind"] in ("check", "field") and c["info"].get("epoch") is None:
            missing.append(f"{c['kind']}: no epoch with its node ids")
    assert not missing, "\n".join(missing)

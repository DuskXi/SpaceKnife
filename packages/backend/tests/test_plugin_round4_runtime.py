"""Round 4 of the plugin trials: what the plugin process does with its time and its disk.

- A cancelled plain-def call no longer gets the plugin process restarted: its place goes to the next
  call at once, the function finishes on a spare thread; only every thread held by cancelled calls
  for STUCK_AFTER_S restarts it (``limits.threads`` sets how many threads).
- Background tasks (``@plugin.background``): tracked by the host (progress, cancelling, a time
  limit, the language, a restart after a crash, ``start = "startup"``), seen by every window.
- ``ctx.files`` reads go through the disk's I/O governor (the disk-load limit), hashing reports
  progress and stops when cancelled.
"""

from __future__ import annotations

import asyncio
import hashlib
import os
import time
from pathlib import Path

import pytest

from spaceknife.plugins import host as host_mod
from spaceknife.plugins import hostapi as hostapi_mod
from spaceknife.server.rpc import RpcError
from spaceknife_protocol.errors import ErrorCode
from spaceknife_sdk.manifest import parse_manifest
from spaceknife_sdk.testing import MockHost, make_tree

from .plugin_helpers import fixture

pytestmark = pytest.mark.timeout(120)

TREE = make_tree(
    {
        "big.bin": os.urandom(3 << 20),
        "a.txt": b"hello world",
        "sub": {"b.txt": b"second file", "c.bin": os.urandom(4096)},
    }
)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


def tasks_host(process: bool) -> MockHost:
    host = MockHost.load(fixture("t.tasks"), process=process)
    host.scan(TREE)
    return host


async def until(cond: object, seconds: float = 10.0) -> None:
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if cond():  # type: ignore[operator]
            return
        await asyncio.sleep(0.05)
    raise AssertionError("timed out")


async def ran(host: MockHost, task: str) -> None:
    """Until the plugin's code of ``task`` has started (it counts its starts)."""
    for _ in range(200):
        if (await host.call("seen"))[task] >= 1:
            return
        await asyncio.sleep(0.05)
    raise AssertionError(f"{task} never started")


# --------------------------------------------------------------------------- gap 8: cancelled calls


def test_threads_are_declared_in_the_manifest() -> None:
    m = parse_manifest('id = "a.b"\nversion = "0.1.0"\nname = "x"\n[limits]\nthreads = 4\n')
    assert m.limits.threads == 4
    assert parse_manifest('id = "a.b"\nversion = "0.1.0"\nname = "x"\n').limits.threads == 2
    with pytest.raises(Exception, match="threads"):
        parse_manifest('id = "a.b"\nversion = "0.1.0"\nname = "x"\n[limits]\nthreads = 64\n')


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_cancelled_plain_calls_do_not_restart_the_process(process: bool) -> None:
    # one call thread (limits.threads = 1): three plain-def calls that outlive their 50 ms limit
    async with tasks_host(process) as host:
        first = await host.call("seen")
        rec = host.server.plugins.records["t.tasks"]
        t0 = time.monotonic()
        for _ in range(3):
            with pytest.raises(RpcError) as err:
                await host.call("slow", seconds=1.5)
            assert err.value.code == ErrorCode.PLUGIN_TIMEOUT
        # the next call does not wait for them: each cancelled one gave its thread's place away
        assert await host.call("quick") == {"tag": "quick"}
        assert time.monotonic() - t0 < 1.4
        await asyncio.sleep(2.0)  # they finish on their spare threads
        assert rec.restarts == 0 and (await host.call("seen"))["pid"] == first["pid"]


async def test_every_thread_held_for_long_restarts_it(monkeypatch: pytest.MonkeyPatch) -> None:
    # no spare threads: a cancelled call keeps its place, and after STUCK_AFTER_S the process goes
    monkeypatch.setattr(host_mod, "SPARE_THREADS", 0)
    monkeypatch.setattr(host_mod, "STUCK_AFTER_S", 1.0)
    monkeypatch.setattr(host_mod, "BACKOFF_S", (0.05, 0.05, 0.05))
    async with tasks_host(True) as host:
        pid = (await host.call("seen"))["pid"]
        rec = host.server.plugins.records["t.tasks"]
        with pytest.raises(RpcError):
            await host.call("slow", seconds=30)
        await asyncio.sleep(0.5)
        assert rec.restarts == 0  # not at once: only when it has been held this long
        await until(lambda: rec.restarts == 1, 10)
        assert (await host.call("seen"))["pid"] != pid


# --------------------------------------------------------------------------- gap 5: background tasks


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_background_task_reports_progress_and_ends(process: bool) -> None:
    async with tasks_host(process) as host:
        info = await host.task("count", {"n": 8, "pause": 0.1})
        assert info.state == "done" and info.done == 8 and info.total == 8
        assert info.text == "第 8 个" and info.title == "数数"
        assert (await host.call("seen"))["counted"] == 8
        sent = [p for name, p in host.session.sent if name == "plugin.task" and p["name"] == "count"]
        assert sent[0]["state"] == "running" and sent[-1]["state"] == "done"
        between = [p["done"] for p in sent if 0 < p["done"] < 8]
        assert between and len(sent) <= 8  # progress while it ran, every window sees it (≤ 4 a second)
        from spaceknife_protocol.base import Empty
        from spaceknife_protocol.plugins import PluginTaskCancelParams

        route = host.server.router.get("plugin.tasks")
        listed = await route.handler(host.session, Empty())
        assert any(t.id == info.id and t.state == "done" for t in listed.tasks)
        again = host.server.router.get("plugin.task_cancel")
        assert (await again.handler(host.session, PluginTaskCancelParams(id=info.id))).state == "done"


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_tasks_start_from_code_cancel_and_time_out(process: bool) -> None:
    async with tasks_host(process) as host:
        tid = (await host.call("start_count", n=3, pause=0.05))["tag"]
        await until(lambda: any(t.id == tid and t.state == "done" for t in host.tasks()))
        await host.task("forever", wait=False)
        await ran(host, "forever")
        assert await host.cancel_task("forever") == 1
        assert next(t for t in host.tasks() if t.name == "forever").state == "cancelled"
        timed = await host.task("short")
        assert timed.state == "failed" and "0.3" in timed.error
        with pytest.raises(RpcError) as err:
            await host.task("nope")
        assert err.value.code == ErrorCode.METHOD_NOT_FOUND


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_startup_task_runs_with_the_process(process: bool) -> None:
    async with tasks_host(process) as host:
        await host.call("quick")
        await until(lambda: any(t.name == "startup" and t.state == "running" for t in host.tasks()))
        assert (await host.call("seen"))["startup"] == 1
        # the process does not count as idle while it runs; restarting the plugin starts it again
        await host.server.plugins.restart("t.tasks")
        await until(lambda: sum(1 for t in host.tasks() if t.name == "startup" and t.state == "running") == 1)
        ended = [t for t in host.tasks() if t.name == "startup" and t.state != "running"]
        assert ended and ended[0].state == "cancelled"


async def test_a_task_is_started_again_after_a_crash(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(host_mod, "BACKOFF_S", (0.05, 0.05, 0.05))
    async with tasks_host(True) as host:
        before = (await host.call("seen"))["pid"]
        await host.task("forever", wait=False)
        await ran(host, "forever")
        with pytest.raises(RpcError):
            await host.call("die")
        await until(
            lambda: any(t.name == "forever" and t.attempt == 2 and t.state == "running" for t in host.tasks())
        )
        await ran(host, "forever")
        seen = await host.call("seen")
        assert seen["pid"] != before and seen["forever"] == 1  # the new process runs it
        await host.cancel_task("forever")


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_task_follows_the_language(process: bool) -> None:
    async with tasks_host(process) as host:
        await host.call("quick")
        await host.task("lang", wait=False)
        await until(lambda: any(t.name == "lang" and t.done == 1 for t in host.tasks()))
        assert next(t for t in host.tasks() if t.name == "lang").text == "中文"
        await host.set_lang("en")
        await until(lambda: any(t.name == "lang" and t.state == "done" for t in host.tasks()))
        assert (await host.call("seen"))["langs"] == ["zh-CN", "en"]  # its own code followed too
        done = next(t for t in host.tasks() if t.name == "lang")
        assert done.text == "English" and done.title == "Follows the language"
        # what a task said is kept in both languages: the windows show the one of the moment
        sent = [p for name, p in host.session.sent if name == "plugin.task" and p["name"] == "startup"]
        assert sent and sent[-1]["title"] == "Runs from the start"  # pushed again on the switch
        await host.set_lang("zh-CN")
        assert next(t for t in host.tasks() if t.name == "lang").text == "中文"


# --------------------------------------------------------------------------- gap 2: paced reads


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_reads_go_through_the_disk_governor(process: bool, monkeypatch: pytest.MonkeyPatch) -> None:
    asked: list[str] = []
    real_io = hostapi_mod.HostApi._io

    async def spy(
        self: hostapi_mod.HostApi, rec: object, p: dict[str, object], frames: list[bytes]
    ) -> object:
        asked.append(str(p.get("base")))
        return await real_io(self, rec, p, frames)  # type: ignore[arg-type]

    monkeypatch.setattr(hostapi_mod.HostApi, "_io", spy)
    async with tasks_host(process) as host:
        out = await host.call("hashing")
        data = (Path(host.files_root or "") / "big.bin").read_bytes()
        assert out["digest"] == hashlib.sha256(data).hexdigest()
        assert out["progress"][-1] == [len(data), len(data)]  # bytes read of the file's size
        assert all(d is not None for d in out["many"])
        assert asked  # every burst asked the host first
        assert host.server.plugins.api.io_permits() == 0  # and gave its permits back


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_reads_wait_for_the_governor(process: bool, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(hostapi_mod, "IO_WAIT_S", 0.2)
    async with tasks_host(process) as host:
        await host.call("quick")
        api = host.server.plugins.api
        root = str(host.files_root)
        base = os.path.normcase(os.path.realpath(root)).rstrip("\\/")
        gov = await api._governor_of(base)
        assert gov is not None
        gov.set_level("max")
        gov.set_manual_concurrency(1)
        held = await gov.permit()  # the disk's only place is taken (a scan's batch, say)
        try:
            call = asyncio.ensure_future(host.call("hashing"))
            await asyncio.sleep(1.0)
            assert not call.done()  # the plugin's read waits for the disk
        finally:
            held.release()
        out = await asyncio.wait_for(call, 20)
        assert out["many"] and api.io_permits() == 0

"""Plugin runtimes: a plugin in its own process and in the host (handshake, calls, errors, time
limits, latest-wins, cancellation, crashes and restarts)."""

from __future__ import annotations

import asyncio
import sys
import time
from pathlib import Path

import pytest

from spaceknife.plugins import host as host_mod
from spaceknife.server.rpc import RpcError
from spaceknife_protocol.errors import ErrorCode

from .plugin_helpers import mock

pytestmark = pytest.mark.timeout(120)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_methods_events_and_state(process: bool) -> None:
    async with mock("t.full", process=process) as host:
        assert await host.call("add", a=2, b=3) == {"total": 5}
        assert host.events["progress"] == [{"done": 1, "total": 1}]
        assert host.state("status") == {"runs": 1}
        assert await host.call("lang") == {"text": "你好"}
        value, frames = await host.call_frames("columns")
        assert value["columns"] and frames[0]["double"].tolist() == [0, 2, 4, 6, 8]
        rec = host.server.plugins.records["t.full"]
        assert rec.runtime.kind == ("process" if process else "inprocess")


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_invalid_params_and_plugin_errors(process: bool) -> None:
    async with mock("t.full", process=process) as host:
        with pytest.raises(RpcError) as err:
            await host.call("add", b=1)  # a is required
        assert err.value.code == ErrorCode.INVALID_PARAMS
    async with mock("t.crash", process=process) as host:
        with pytest.raises(RpcError) as err:
            await host.call("boom")
        assert err.value.code == ErrorCode.PLUGIN_ERROR
        # its type and its own message, on one line (bounded); the traceback stays in the log
        assert err.value.message == "plugin error: ValueError: a bug"
        with pytest.raises(RpcError) as err:
            await host.call("refuse")
        assert err.value.code == ErrorCode.PLUGIN_ERROR and err.value.message == "不行"
        with pytest.raises(RpcError) as err:
            await host.call("nope")
        assert err.value.code == ErrorCode.METHOD_NOT_FOUND
        assert await host.call("ping") == {"ok": True}


async def test_a_crash_restarts_then_stops_for_good(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(host_mod, "BACKOFF_S", (0.05, 0.05, 0.05))
    async with mock("t.crash", process=True) as host:
        assert await host.call("ping") == {"ok": True}
        rec = host.server.plugins.records["t.crash"]
        pid = rec.runtime.pid
        with pytest.raises(RpcError) as err:
            await host.call("die")
        assert err.value.code == ErrorCode.PLUGIN_UNAVAILABLE
        await asyncio.sleep(0.2)
        assert await host.call("ping") == {"ok": True}  # started again
        assert rec.runtime.pid != pid and rec.restarts >= 1
        for _ in range(2):
            with pytest.raises(RpcError):
                await host.call("die")
            await asyncio.sleep(0.1)
        assert rec.state == "crashed"
        with pytest.raises(RpcError) as err:
            await host.call("ping")
        assert err.value.code in (ErrorCode.PLUGIN_DISABLED, ErrorCode.PLUGIN_UNAVAILABLE)
        info = await host.server.plugins.restart("t.crash")
        assert info.state == "enabled"
        assert await host.call("ping") == {"ok": True}


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_time_limits_and_cancellation(process: bool) -> None:
    async with mock("t.slow", process=process) as host:
        t0 = time.monotonic()
        with pytest.raises(RpcError) as err:
            await host.call("short", seconds=5)
        assert err.value.code == ErrorCode.PLUGIN_TIMEOUT
        assert time.monotonic() - t0 < 3
        assert await host.call("quick") == {"tag": "quick", "cancelled": False}  # still serving


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_latest_wins(process: bool) -> None:
    async with mock("t.slow", process=process) as host:
        hp = await host._ready()
        session = host.session

        async def latest(tag: str, seconds: float) -> object:
            return await hp.call(
                "t.slow",
                "method",
                "latest",
                {"seconds": seconds, "tag": tag},
                where=hp.places.nowhere(),
                session=session,
                latest=True,
            )

        first = asyncio.create_task(latest("first", 3))
        await asyncio.sleep(0.3)
        second = await latest("second", 0.05)
        assert second[0] == {"tag": "second", "cancelled": False}
        with pytest.raises((asyncio.CancelledError, RpcError)):
            await first


async def test_a_stuck_call_is_given_up(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.plugins.runtime import process as process_mod

    monkeypatch.setattr(process_mod, "CANCEL_GRACE", 1.0)
    monkeypatch.setattr(process_mod, "WATCH_EVERY", 0.2)
    monkeypatch.setattr(host_mod, "BACKOFF_S", (0.05, 0.05, 0.05))
    # no spare threads (with them it just goes on: test_plugin_round4_runtime); held 1 s = stuck
    monkeypatch.setattr(host_mod, "SPARE_THREADS", 0)
    monkeypatch.setattr(host_mod, "STUCK_AFTER_S", 1.0)
    async with mock("t.slow", process=True) as host:
        t0 = time.monotonic()
        with pytest.raises(RpcError) as err:
            await host.call("stuck", seconds=30)
        assert err.value.code == ErrorCode.PLUGIN_TIMEOUT and time.monotonic() - t0 < 2
        # the plugin's other calls run beside it (a worker thread holds the stuck one), and the
        # process stays: a function that does not look at cancellation only costs its thread
        assert await host.call("quick") == {"tag": "quick", "cancelled": False}
        rec = host.server.plugins.records["t.slow"]
        pid = rec.runtime.pid
        await asyncio.sleep(2.0)  # past the grace for cancelled calls
        assert rec.restarts == 0 and rec.runtime.pid == pid
        # ... until every worker thread is stuck: then the process goes, and comes back
        with pytest.raises(RpcError):
            await host.call("stuck", seconds=30)
        for _ in range(50):
            await asyncio.sleep(0.1)
            if rec.restarts:
                break
        assert rec.restarts == 1
        assert await host.call("quick") == {"tag": "quick", "cancelled": False}
        assert rec.runtime.pid != pid


async def test_a_blocked_event_loop_is_logged_then_given_up(monkeypatch: pytest.MonkeyPatch) -> None:
    from loguru import logger

    from spaceknife.plugins.runtime import process as process_mod

    monkeypatch.setattr(process_mod, "CANCEL_GRACE", 0.5)
    monkeypatch.setattr(process_mod, "CANCEL_LIMIT", 2.0)
    monkeypatch.setattr(process_mod, "WATCH_EVERY", 0.2)
    monkeypatch.setattr(host_mod, "BACKOFF_S", (0.05, 0.05, 0.05))
    lines: list[str] = []
    sink = logger.add(lambda m: lines.append(str(m)), level="WARNING")
    try:
        async with mock("t.slow", process=True) as host:
            rec = host.server.plugins.records["t.slow"]
            with pytest.raises(RpcError):
                await host.call("blocking", seconds=30)
            await asyncio.sleep(1.0)  # past the grace: said which call, the process still there
            assert rec.restarts == 0
            assert any("blocking" in ln and "still runs" in ln for ln in lines)
            for _ in range(60):  # past the limit: given up, and started again when asked
                await asyncio.sleep(0.1)
                if rec.restarts or rec.runtime is None or rec.runtime.state != "running":
                    break
            assert any("stopping the plugin" in ln and "blocking" in ln for ln in lines)
            assert await host.call("quick") == {"tag": "quick", "cancelled": False}
    finally:
        logger.remove(sink)


def test_every_message_the_sdk_sends_is_on_the_wire() -> None:
    import ast
    import typing

    import spaceknife_sdk
    from spaceknife_sdk import wire

    union = typing.get_args(typing.get_args(wire.FromPlugin)[0])
    on_wire = {m.model_fields["type"].default for m in union}
    sent: set[str] = set()
    for f in Path(spaceknife_sdk.__file__).parent.glob("*.py"):
        for call in ast.walk(ast.parse(f.read_text(encoding="utf-8"))):
            if (
                isinstance(call, ast.Call)
                and isinstance(call.func, ast.Attribute)
                and call.func.attr == "notify"
                and call.args
                and isinstance(call.args[0], ast.Constant)
            ):
                sent.add(str(call.args[0].value))
    assert sent and sent <= on_wire, sent - on_wire


async def test_concurrency_limit(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(host_mod, "MAX_QUEUED", 2)
    async with mock("t.slow") as host:
        hp = await host._ready()
        tasks = [
            asyncio.create_task(
                hp.call("t.slow", "method", "latest", {"seconds": 0.5}, where=hp.places.nowhere(), timeout=5)
            )
            for _ in range(8)
        ]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        limited = [r for r in results if isinstance(r, RpcError) and r.code == ErrorCode.PLUGIN_LIMIT]
        assert limited and len(limited) < 8


async def test_process_stderr_and_info() -> None:
    async with mock("t.full", process=True) as host:
        await host.call("add", a=1)
        info = host.server.plugins.info(host.server.plugins.records["t.full"])
        assert info.process is not None and info.process.state == "running" and info.process.pid
        assert info.runtime == "process" and info.granted == ["tree.read", "files.read", "basket.propose"]
        if sys.platform != "darwin":  # macOS: from the heartbeat, which may not have come yet
            assert info.process.memory > 1 << 20

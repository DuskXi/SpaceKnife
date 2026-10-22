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
        assert err.value.code == ErrorCode.PLUGIN_ERROR and "ValueError" in err.value.message
        assert "a bug" not in err.value.message  # no traceback or exception text for the frontends
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
                "t.slow", "method", "latest", {"seconds": seconds, "tag": tag}, session=session, latest=True
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
    async with mock("t.slow", process=True) as host:
        t0 = time.monotonic()
        with pytest.raises(RpcError) as err:
            await host.call("stuck", seconds=30)
        assert err.value.code == ErrorCode.PLUGIN_TIMEOUT and time.monotonic() - t0 < 2
        # the plugin's other calls run beside it (a worker thread holds the stuck one) ...
        assert await host.call("quick") == {"tag": "quick", "cancelled": False}
        rec = host.server.plugins.records["t.slow"]
        pid = rec.runtime.pid
        # ... until the grace for the cancelled call is over: then the process goes, and comes back
        for _ in range(50):
            await asyncio.sleep(0.1)
            if rec.restarts:
                break
        assert rec.restarts == 1
        assert await host.call("quick") == {"tag": "quick", "cancelled": False}
        assert rec.runtime.pid != pid


async def test_concurrency_limit(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(host_mod, "MAX_QUEUED", 2)
    async with mock("t.slow") as host:
        hp = await host._ready()
        tasks = [
            asyncio.create_task(hp.call("t.slow", "method", "latest", {"seconds": 0.5}, timeout=5))
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

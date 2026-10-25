"""Acceptance: the plugin process under trouble — calls cancelled while they run, a process that
dies, disk reads under the disk-load limit (round 4's gap 8 and F, the restart after a crash)."""

from __future__ import annotations

import asyncio
import contextlib
from collections.abc import Awaitable
from pathlib import Path

import pytest

from spaceknife_client.connection import RemoteError

from .harness import Backend

pytestmark = pytest.mark.timeout(240)

SLOW_S = 4.0  # how long each cancelled call goes on by itself


async def test_cancelled_calls_do_not_hold_the_plugin_up(backend: Backend, probe: str) -> None:
    """Three plain-def calls that never look at cancellation (the probe allows two threads), each
    cancelled by the frontend while it runs: the next call answers at once, from the same process
    (no restart), and the three finish on their own later."""
    before = await backend.dump(probe)
    pid0 = (await backend.pcall(probe, "dump"))["pid"]
    for n in range(1, 4):
        call = asyncio.create_task(backend.pcall(probe, "slow", seconds=SLOW_S))
        await backend.until(
            f"slow call {n} to run",
            lambda: backend.dump(probe),
            lambda d, n=n: d["slow_started"] >= before["slow_started"] + n,
        )
        call.cancel()  # the client sends $/cancel
        with contextlib.suppress(asyncio.CancelledError):
            await call
    quick = await backend.pcall(probe, "dump", timeout=SLOW_S - 1)
    assert quick["pid"] == pid0
    assert quick["seen"]["slow_done"] == before["slow_done"]  # it did not wait for them
    process = (await backend.plugin(probe))["process"]
    assert process["pid"] == pid0 and process["restarts"] == 0, process
    await backend.until(
        "the cancelled calls to end by themselves",
        lambda: backend.pcall(probe, "dump"),
        lambda d: d["seen"]["slow_done"] == before["slow_done"] + 3,
        limit=SLOW_S + 30,
    )
    process = (await backend.plugin(probe))["process"]
    assert process["pid"] == pid0 and process["restarts"] == 0, process


async def test_a_crashed_process_comes_back_with_its_task(backend: Backend, probe: str) -> None:
    """The plugin's process dies (os._exit) while a ``restart=True`` background task runs: the
    call gets an error, the host starts the process again and the task in it (attempt 2); calls
    work again, the plugin stays enabled."""
    await backend.pcall(probe, "start_keeper")

    def keeper(attempt: int) -> Awaitable[list[dict[str, object]]]:
        async def probe_tasks() -> list[dict[str, object]]:
            return [t for t in await backend.tasks(probe) if t["name"] == "keeper"]

        return backend.until(
            f"the keeper task, attempt {attempt}, to run",
            probe_tasks,
            lambda ts: any(t["state"] == "running" and t["attempt"] == attempt for t in ts),
        )

    await keeper(1)
    first = await backend.pcall(probe, "dump")
    assert first["seen"]["keeper"] == 1
    err = await backend.error(f"plugin.{probe}.die")
    assert err.code is not None
    await keeper(2)
    again = await backend.until(
        "a call to answer from the new process",
        lambda: backend.pcall(probe, "dump"),
        lambda d: d["pid"] != first["pid"] and d["seen"]["keeper"] == 1,
    )
    assert again["seen"]["calls"] == []  # a fresh process
    info = await backend.plugin(probe)
    assert info["state"] == "enabled", info
    assert info["process"]["restarts"] >= 1 and info["process"]["pid"] == again["pid"], info["process"]


async def test_disk_reads_give_their_permits_back_at_once(backend: Backend, probe: str, data: Path) -> None:
    """``ctx.files`` reads through the disk's load limit, one permit at a time here (the scan ran
    with one listing at once, which the disk's governor keeps). Each permit goes back as soon as
    its read is done: six files hash in well under the 2 s after which the host takes a permit
    back by itself (round 4's gap F: each read test waited exactly 2 s more)."""
    from spaceknife.plugins.hostapi import PERMIT_MAX_S

    for i in range(6):
        (data / f"f{i}.bin").write_bytes(bytes([i]) * (4096 + i))
    st = await backend.call("scan.start", {"roots": [str(data)], "auto_snapshot": False, "workers": 1})
    sid = st["scan_id"]
    await backend.until(
        "the scan", lambda: backend.call("scan.status", {"scan_id": sid}), lambda s: s["state"] == "done"
    )
    try:
        out = await backend.pcall(probe, "hash_all", scan_id=sid)
    except RemoteError as exc:  # pragma: no cover - the message says why
        raise AssertionError(f"hash_all failed: {exc}\n{backend.tail()}") from None
    assert out["files"] == 6 and out["digests"] == 6, out
    assert out["took"] < PERMIT_MAX_S, out
    assert "a disk-read permit was not given back in time" not in backend.log()

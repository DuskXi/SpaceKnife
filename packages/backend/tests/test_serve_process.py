"""End-to-end: run ``python -m spaceknife serve`` as a child process, like Electron will."""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path
from typing import Any

from spaceknife_client.connection import Connection
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import APP_SHUTDOWN, SYSTEM_PING
from spaceknife_protocol.system import PingParams

from .conftest import CLIENT, TOKEN

START_TIMEOUT = 30


async def _spawn(*args: str) -> asyncio.subprocess.Process:
    return await asyncio.create_subprocess_exec(
        sys.executable,
        "-X",
        "utf8",
        "-m",
        "spaceknife",
        "serve",
        "--log-format",
        "plain",
        *args,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )


async def _ready(proc: asyncio.subprocess.Process) -> dict[str, Any]:
    assert proc.stdout is not None
    line = await asyncio.wait_for(proc.stdout.readline(), timeout=START_TIMEOUT)
    assert line, "backend exited before printing the ready line"
    ready: dict[str, Any] = json.loads(line)
    assert ready["event"] == "ready"
    assert ready["url"].startswith("ws://127.0.0.1:")
    return ready


async def test_token_on_stdin_and_exit_on_stdin_eof() -> None:
    proc = await _spawn("--token-stdin")
    try:
        assert proc.stdin is not None
        proc.stdin.write((TOKEN + "\n").encode())
        await proc.stdin.drain()
        ready = await _ready(proc)
        assert "token" not in ready
        async with await Connection.open(ready["url"], TOKEN, CLIENT) as conn:
            pong = await conn.call(SYSTEM_PING, PingParams(payload="e2e"))
            assert pong.payload == "e2e"
        proc.stdin.close()
        code = await asyncio.wait_for(proc.wait(), timeout=15)
        assert code == 0
    finally:
        if proc.returncode is None:
            proc.kill()
            await proc.wait()


async def test_generated_token_and_app_shutdown() -> None:
    proc = await _spawn()
    try:
        ready = await _ready(proc)
        assert len(ready["token"]) >= 32
        async with await Connection.open(ready["url"], ready["token"], CLIENT) as conn:
            assert (await conn.call(APP_SHUTDOWN, Empty())).accepted
        code = await asyncio.wait_for(proc.wait(), timeout=15)
        assert code == 0
    finally:
        if proc.returncode is None:
            proc.kill()
            await proc.wait()


async def test_ready_file_lets_another_process_attach(tmp_path: Path) -> None:
    ready_path = tmp_path / "conn" / "ready.json"
    proc = await _spawn("--ready-file", str(ready_path))
    try:
        ready = await _ready(proc)
        info = json.loads(ready_path.read_text(encoding="utf-8"))
        assert info["url"] == ready["url"]
        assert info["token"] == ready["token"]
        async with await Connection.open(info["url"], info["token"], CLIENT) as conn:
            assert (await conn.call(APP_SHUTDOWN, Empty())).accepted
        assert await asyncio.wait_for(proc.wait(), timeout=15) == 0
        assert not ready_path.exists()
    finally:
        if proc.returncode is None:
            proc.kill()
            await proc.wait()


async def test_process_pool_scan_with_token_on_stdin(tmp_path: Path) -> None:
    """Regression: on Windows a blocking read of stdin stalled the pool's CreateProcess calls."""
    from spaceknife_protocol import scan as p
    from spaceknife_protocol.methods import SCAN_START, SCAN_STATUS

    from .tree_fixture import build_tree

    facts = build_tree(tmp_path / "root")
    proc = await _spawn("--token-stdin")
    try:
        assert proc.stdin is not None
        proc.stdin.write((TOKEN + "\n").encode())
        await proc.stdin.drain()
        ready = await _ready(proc)
        async with await Connection.open(ready["url"], TOKEN, CLIENT) as conn:
            started = await conn.call(
                SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=2, auto_snapshot=False)
            )
            status = started
            for _ in range(120):
                status = await conn.call(SCAN_STATUS, p.ScanIdParams(scan_id=started.scan_id))
                if status.state not in ("queued", "scanning"):
                    break
                await asyncio.sleep(0.25)
            assert status.state == "done"
            assert status.files == facts.files
        proc.stdin.close()
        assert await asyncio.wait_for(proc.wait(), timeout=15) == 0
    finally:
        if proc.returncode is None:
            proc.kill()
            await proc.wait()

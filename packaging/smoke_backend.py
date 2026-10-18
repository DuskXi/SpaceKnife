"""Smoke test of the frozen backend (M8): run by tools/build_package.py after PyInstaller.

    python packaging/smoke_backend.py dist/backend/spaceknife-backend/spaceknife-backend[.exe]

Small and short on purpose (a temporary folder of a few hundred bytes, an isolated home):
``version`` (the native extension must load); a scan with the native walker and one with
spawn-context worker processes (``multiprocessing.freeze_support`` in the entry point);
``serve --token-stdin``: the ready line,
``session.hello`` with the token and ``system.info`` over the WebSocket (the repo's Python client),
a graceful exit on stdin EOF. Exit status 1 on failure.
"""

from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

TIMEOUT = 60


def run(exe: Path, *args: str, env: dict[str, str]) -> str:
    out = subprocess.run([str(exe), *args], capture_output=True, env=env, timeout=TIMEOUT, check=False)
    text = out.stdout.decode("utf-8", "replace") + out.stderr.decode("utf-8", "replace")
    if out.returncode != 0:
        raise SystemExit(f"FAIL {' '.join(args)} → exit {out.returncode}\n{text}")
    return text


async def serve(exe: Path, env: dict[str, str]) -> None:
    from spaceknife_client.connection import Connection
    from spaceknife_protocol.base import Empty
    from spaceknife_protocol.methods import SYSTEM_INFO
    from spaceknife_protocol.session import ClientInfo

    proc = await asyncio.create_subprocess_exec(
        str(exe),
        "serve",
        "--token-stdin",
        "--log-format",
        "plain",
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
        env=env,
    )
    assert proc.stdin is not None
    assert proc.stdout is not None
    token = "smoke-" + os.urandom(12).hex()
    proc.stdin.write(token.encode() + b"\n")
    await proc.stdin.drain()
    ready = json.loads(await asyncio.wait_for(proc.stdout.readline(), TIMEOUT))
    client = ClientInfo(name="smoke", version="0", kind="test")
    async with await asyncio.wait_for(Connection.open(ready["url"], token, client), TIMEOUT) as conn:
        info = await asyncio.wait_for(conn.call(SYSTEM_INFO, Empty()), TIMEOUT)
    print(f"serve: {ready['url']}, server {info.server.version}, pid {info.pid}, {info.platform.system}")
    proc.stdin.close()
    code = await asyncio.wait_for(proc.wait(), TIMEOUT)
    if code != 0:
        raise SystemExit(f"FAIL serve exited with {code} after stdin EOF")


def main() -> int:
    exe = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="sk-smoke-") as tmp:
        root = Path(tmp)
        tree = root / "tree"
        for i in range(3):
            (tree / f"d{i}").mkdir(parents=True)
            for j in range(4):
                (tree / f"d{i}" / f"f{j}.txt").write_bytes(b"x" * (64 * (j + 1)))
        env = {**os.environ, "SPACEKNIFE_HOME": str(root / "home"), "NO_COLOR": "1"}
        version = run(exe, "version", env=env).strip()
        print(version)
        if "native none" in version:
            raise SystemExit("FAIL the frozen backend has no native extension (spaceknife_native._native)")
        out = run(exe, "scan", str(tree), "--no-snapshot", "--depth", "1", env=env)
        assert "d0" in out, out
        print("scan (native walker): ok")
        python_only = {**env, "SPACEKNIFE_NO_NATIVE": "1"}
        out = run(exe, "scan", str(tree), "--no-snapshot", "--workers", "2", env=python_only)
        assert "d0" in out, out
        print("scan (2 worker processes): ok")
        asyncio.run(serve(exe, env))
    print("frozen backend: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())

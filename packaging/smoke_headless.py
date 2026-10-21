"""Smoke test of headless mode in a packaged app: no window, the interface served over HTTP.

    python packaging/smoke_headless.py dist/electron/win-unpacked/SpaceKnife.com
    python packaging/smoke_headless.py dist/electron/linux-unpacked/spaceknife      # no display needed
    python packaging/smoke_headless.py <electron> <app folder>                       # a checkout's build

Starts ``<app> --headless --listen 127.0.0.1:0`` with an isolated home and reads the address it
prints. Then: the page is served; /sk/info answers only with the token; the WebSocket on the same
address refuses another site's Origin and, with the page's own, reaches the backend, whose
session.hello takes the token. Finally it stops the app the way a terminal does (SIGINT; Ctrl+Break
to SpaceKnife.com on Windows) and checks that the address is closed. On Linux this runs without a
display: Chromium's --headless switch is what lets the app start there. Exit status 1 on failure.
"""

from __future__ import annotations

import json
import os
import queue
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

from websockets.exceptions import InvalidStatus
from websockets.sync.client import connect

TIMEOUT = 90.0
URL = re.compile(r"(https?)://localhost:(\d+)/#token=([A-Za-z0-9_%-]+)")


def fail(message: str, output: list[str]) -> int:
    print(f"FAIL: {message}", file=sys.stderr)
    print("--- app output ---", file=sys.stderr)
    print("\n".join(output[-60:]), file=sys.stderr)
    return 1


def get(url: str, headers: dict[str, str] | None = None) -> tuple[int, bytes]:
    req = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=15) as res:
            return res.status, res.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def port_open(port: int) -> bool:
    with socket.socket() as s:
        s.settimeout(1)
        return s.connect_ex(("127.0.0.1", port)) == 0


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    app, extra = sys.argv[1], sys.argv[2:]
    home = tempfile.mkdtemp(prefix="sk-headless-smoke-")
    env = {**os.environ, "SPACEKNIFE_HOME": home, "SPACEKNIFE_LANGUAGE": "en", "LANG": "en_US.UTF-8"}
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    flags = subprocess.CREATE_NEW_PROCESS_GROUP if sys.platform == "win32" else 0
    proc = subprocess.Popen(
        [app, *extra, "--headless", "--listen", "127.0.0.1:0"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=env,
        creationflags=flags,
    )
    output: list[str] = []
    lines: queue.Queue[str] = queue.Queue()

    def pump() -> None:
        assert proc.stdout is not None
        for raw in proc.stdout:
            line = raw.decode("utf-8", "replace").rstrip()
            output.append(line)
            lines.put(line)

    threading.Thread(target=pump, daemon=True).start()
    try:
        deadline = time.monotonic() + TIMEOUT
        found = None
        while found is None and time.monotonic() < deadline:
            try:
                found = URL.search(lines.get(timeout=1))
            except queue.Empty:
                if proc.poll() is not None:
                    return fail(f"the app exited with {proc.returncode} before printing its address", output)
        if found is None:
            return fail("no address printed", output)
        scheme, port, token = found.group(1), int(found.group(2)), urllib.request.unquote(found.group(3))
        base = f"{scheme}://127.0.0.1:{port}"
        print(f"listening on {base}")

        status, body = get(f"{base}/")
        if status != 200 or b'id="root"' not in body:
            return fail(f"the page: HTTP {status}", output)
        if get(f"{base}/sk/info")[0] != 401:
            return fail("/sk/info answered without the token", output)
        status, body = get(f"{base}/sk/info", {"Authorization": f"Bearer {token}"})
        info = json.loads(body) if status == 200 else {}
        if info.get("platform") != {"win32": "win32", "darwin": "darwin"}.get(sys.platform, "linux"):
            return fail(f"/sk/info: HTTP {status} {body[:200]!r}", output)
        print(f"/sk/info: {info.get('platform')} {info.get('version')}")

        ws_url = f"ws://127.0.0.1:{port}/v1"
        try:
            with connect(
                ws_url, subprotocols=["spaceknife.v1"], origin="http://evil.example", open_timeout=10
            ):
                return fail("a WebSocket from another site's page was accepted", output)
        except InvalidStatus as e:
            if e.response.status_code != 403:
                return fail(f"another site's WebSocket: HTTP {e.response.status_code}, not 403", output)
        with connect(
            ws_url, subprotocols=["spaceknife.v1"], origin=f"http://127.0.0.1:{port}", open_timeout=10
        ) as ws:
            hello = {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "session.hello",
                "params": {
                    "token": token,
                    "protocol": "1.1",
                    "client": {"name": "smoke", "version": "0", "kind": "test"},
                },
            }
            ws.send(json.dumps(hello))
            reply = json.loads(ws.recv(30))
            if "result" not in reply:
                return fail(f"session.hello: {reply}", output)
            print(
                f"session.hello: {reply['result']['server']['name']} {reply['result']['server']['version']}"
            )

        # stop it as a terminal would: SIGINT, or Ctrl+Break to SpaceKnife.com, which ends the app
        forced = False
        try:
            proc.send_signal(signal.CTRL_BREAK_EVENT if sys.platform == "win32" else signal.SIGINT)
        except OSError as e:  # no console to send it through
            print(f"can't send Ctrl+Break here ({e}): ending the process tree")
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(proc.pid)], capture_output=True, check=False)
            forced = True
        try:
            code = proc.wait(30)
        except subprocess.TimeoutExpired:
            return fail("the app didn't stop after Ctrl+C", output)
        if not forced and code not in (0, 130):
            return fail(f"the app stopped with exit code {code}", output)
        for _ in range(20):
            if not port_open(port):
                break
            time.sleep(0.5)
        else:
            return fail("the address is still open after the app stopped", output)
        print(f"stopped (exit code {code}); headless smoke test passed")
        return 0
    finally:
        if proc.poll() is None:
            if sys.platform == "win32":
                subprocess.run(
                    ["taskkill", "/T", "/F", "/PID", str(proc.pid)], capture_output=True, check=False
                )
            else:
                proc.kill()
        # the backend may still hold its log for a moment
        for _ in range(10):
            try:
                shutil.rmtree(home)
                break
            except OSError:
                time.sleep(0.5)


if __name__ == "__main__":
    sys.exit(main())

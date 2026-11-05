"""Smoke test of a packaged app (M8): start it, see the backend and the start page, quit it.

    python packaging/smoke_app.py dist/electron/win-unpacked/SpaceKnife.exe
    python packaging/smoke_app.py dist/electron/linux-unpacked/spaceknife
    python packaging/smoke_app.py SpaceKnife-<v>-x86_64.AppImage     # run with --appimage-extract-and-run
    python packaging/smoke_app.py --portable <unpacked portable archive>/SpaceKnife.exe
    python packaging/smoke_app.py --portable <unpacked macOS archive>/SpaceKnife.app/Contents/MacOS/SpaceKnife

The app starts minimized (SPACEKNIFE_START_MINIMIZED) with an isolated home (temporary folder:
its own settings, snapshots and Chromium profile; with --portable the data/ folder next to the
executable, which the app has to find by itself) and the Chrome DevTools port on; through the
port the test waits for the bundled backend to be ready and the start page to list the drives,
then closes the window the way a user does and checks that the app and its backend both exit
(the backend stops on stdin EOF). Exit status 1 on failure.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.request
from collections.abc import Callable
from pathlib import Path
from typing import Any

from websockets.sync.client import ClientConnection, connect

TIMEOUT = 90.0


def pid_alive(pid: int) -> bool:
    if sys.platform == "win32":
        query = ["tasklist", "/FI", f"PID eq {pid}", "/NH", "/FO", "CSV"]
        out = subprocess.run(query, capture_output=True, text=True, check=False).stdout
        return f'"{pid}"' in out
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


class Page:
    """The window's page over the Chrome DevTools protocol (Runtime.evaluate only)."""

    def __init__(self, ws: ClientConnection) -> None:
        self.ws = ws
        self.ids = 0

    def eval(self, expression: str) -> Any:
        self.ids += 1
        params = {"expression": expression, "awaitPromise": True, "returnByValue": True}
        self.ws.send(json.dumps({"id": self.ids, "method": "Runtime.evaluate", "params": params}))
        while True:
            reply = json.loads(self.ws.recv(TIMEOUT))
            if reply.get("id") == self.ids:
                result = reply.get("result", {})
                if "exceptionDetails" in result:
                    raise RuntimeError(result["exceptionDetails"].get("text", "evaluation failed"))
                return result.get("result", {}).get("value")


def until(what: str, check: Callable[[], Any]) -> Any:
    deadline = time.monotonic() + TIMEOUT
    while True:
        value = check()
        if value:
            return value
        if time.monotonic() > deadline:
            raise SystemExit(f"FAIL timed out waiting for {what}")
        time.sleep(0.5)


def smoke(app: Path, home: Path, portable: bool = False) -> None:
    env = {**os.environ, "SPACEKNIFE_START_MINIMIZED": "1"}
    if portable:
        env.pop("SPACEKNIFE_HOME", None)  # the app finds <its folder>/data by itself
    else:
        env["SPACEKNIFE_HOME"] = str(home)
    argv = [str(app), "--remote-debugging-port=0"]
    if app.suffix == ".AppImage":
        argv.insert(1, "--appimage-extract-and-run")  # no FUSE needed
    proc = subprocess.Popen(argv, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        port_file = home / "electron" / "DevToolsActivePort"

        def port() -> int | None:
            if proc.poll() is not None:
                raise SystemExit(f"FAIL the app exited early with {proc.returncode}")
            text = port_file.read_text() if port_file.exists() else ""
            return int(text.split()[0]) if text.strip() else None

        devtools = until("the DevTools port", port)

        def target() -> str | None:
            with urllib.request.urlopen(f"http://127.0.0.1:{devtools}/json/list", timeout=5) as r:
                pages = [t for t in json.load(r) if t.get("type") == "page"]
            return pages[0]["webSocketDebuggerUrl"] if pages else None

        url = until("the window", target)
        with connect(url, max_size=None) as ws:
            page = Page(ws)
            state = until(
                "the backend",
                lambda: page.eval("window.sk.backend().then(s => s.state === 'starting' ? null : s)"),
            )
            if state.get("state") != "ready":
                raise SystemExit(f"FAIL backend {state}")
            backend_pid = state["info"]["pid"]
            print(f"backend ready: {state['info']['url']} (pid {backend_pid})")
            # the start page lists the drives once the backend answered volume.list
            volumes = until(
                "the drives", lambda: page.eval("document.querySelectorAll('.volumes .volume').length")
            )
            title = page.eval("document.title")
            lang = page.eval("document.documentElement.lang")
            text = page.eval("document.body.innerText")
            print(f"window: {title!r}, lang {lang}, {volumes} drive(s), {len(text)} characters of text")
            page.eval("window.sk.window.close(), true")
        code = proc.wait(timeout=TIMEOUT)
        print(f"app exited with {code}")
        deadline = time.monotonic() + 15
        while pid_alive(backend_pid):
            if time.monotonic() > deadline:
                raise SystemExit(f"FAIL the backend (pid {backend_pid}) outlived the app")
            time.sleep(0.3)
        print("backend exited with the app")
    finally:
        if proc.poll() is None:
            proc.kill()


def main() -> int:
    args = sys.argv[1:]
    portable = "--portable" in args
    app = Path(next(a for a in args if a != "--portable")).resolve()
    if portable:
        # the DevTools port file appears in data/electron only if the app found its data folder,
        # next to the executable (on macOS next to the app bundle)
        bundle = app.parents[2] if app.parent.name == "MacOS" and app.parents[2].suffix == ".app" else None
        home = (bundle.parent if bundle else app.parent) / "data"
        if not home.is_dir():
            raise SystemExit(f"FAIL no portable data folder at {home}")
        smoke(app, home, portable=True)
        stored = sorted(p.name for p in home.iterdir())
        print(f"portable data folder holds: {', '.join(stored)}")
        if "electron" not in stored:
            raise SystemExit("FAIL the app did not keep its data in the portable folder")
    else:
        with tempfile.TemporaryDirectory(prefix="sk-app-smoke-") as tmp:
            smoke(app, Path(tmp))
    print("packaged app: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""Run the Electron renderer in a plain browser tab (development and automated UI checks).

    uv run python tools/demo/m3_web.py [--port 0] [--home DIR]

Builds nothing: serve ``apps/electron/out/renderer`` (run ``pnpm build`` in apps/electron first),
start the backend allowing that origin, print the URL with the backend address and token in the
hash. The renderer's shim (src/renderer/src/shim.ts) stands in for the Electron preload API.
"""

from __future__ import annotations

import argparse
import functools
import http.server
import json
import os
import secrets
import subprocess
import sys
import threading
from pathlib import Path
from urllib.parse import quote

ROOT = Path(__file__).resolve().parents[2]
RENDERER = ROOT / "apps" / "electron" / "out" / "renderer"


class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: object) -> None:
        pass

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--home", default=str(ROOT.parent / "SpaceKnife-demos" / "M3-web" / ".home"))
    args = ap.parse_args()
    if not (RENDERER / "index.html").exists():
        print("apps/electron/out/renderer is missing: run `pnpm build` in apps/electron", file=sys.stderr)
        return 1
    httpd = http.server.ThreadingHTTPServer(
        ("127.0.0.1", args.port), functools.partial(_Quiet, directory=str(RENDERER))
    )
    origin = f"http://127.0.0.1:{httpd.server_address[1]}"
    token = secrets.token_urlsafe(24)
    env = {**os.environ, "SPACEKNIFE_HOME": args.home, "PYTHONIOENCODING": "utf-8"}
    backend = subprocess.Popen(
        [
            sys.executable,
            "-X",
            "utf8",
            "-m",
            "spaceknife",
            "serve",
            "--token-stdin",
            "--log-format",
            "plain",
            "--allow-origin",
            origin,
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        env=env,
        cwd=ROOT,
    )
    assert backend.stdin is not None
    assert backend.stdout is not None
    backend.stdin.write((token + "\n").encode())
    backend.stdin.flush()
    ready = json.loads(backend.stdout.readline())
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    url = f"{origin}/index.html#ws={quote(ready['url'])}&token={quote(token)}"
    Path(args.home).mkdir(parents=True, exist_ok=True)
    (Path(args.home) / "url.txt").write_text(url, encoding="utf-8")
    print(url, flush=True)
    try:
        backend.wait()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.shutdown()
        if backend.poll() is None:
            backend.stdin.close()
            try:
                backend.wait(timeout=10)
            except subprocess.TimeoutExpired:
                backend.kill()
    return 0


if __name__ == "__main__":
    sys.exit(main())

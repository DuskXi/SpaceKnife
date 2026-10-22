"""Run the Electron renderer in a plain browser tab (development and automated UI checks).

    uv run python tools/demo/m3_web.py [--port 0] [--home DIR]

Builds nothing: serve ``apps/electron/out/renderer`` (run ``pnpm build`` in apps/electron first),
start the backend allowing that origin, print the URL with the backend address and token in the
hash. The renderer's shim (src/renderer/src/shim.ts) stands in for the Electron preload API.

Plugins' web panels are served as headless mode serves them (apps/electron/src/main/plugin-files.ts):
``/plugins/<id>/web/…`` from the plugin's folder (a development link, installed, shipped), ``…/_sk/…``
from the app's panel SDK, each with the panel's content security policy and the guard first in its
HTML; the page itself may frame only this server's pages.
"""

from __future__ import annotations

import argparse
import functools
import http.server
import json
import os
import re
import secrets
import subprocess
import sys
import threading
from pathlib import Path
from urllib.parse import quote, unquote, urlsplit

ROOT = Path(__file__).resolve().parents[2]
RENDERER = ROOT / "apps" / "electron" / "out" / "renderer"
SDK = ROOT / "apps" / "electron" / "resources" / "plugin-sdk"
SDK_FILES = {"bridge.js", "base.css", "sk.d.ts", "guard.js"}
PANEL = re.compile(r"^/plugins/([a-z][a-z0-9.-]{2,31})/web(/.*)?$")
PLUGIN_ID = re.compile(r"^[a-z][a-z0-9]*(-[a-z0-9]+)*(\.[a-z][a-z0-9]*(-[a-z0-9]+)*)+$")
TYPES = {".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".mjs": "text/javascript; charset=utf-8",
         ".css": "text/css; charset=utf-8", ".json": "application/json; charset=utf-8", ".svg": "image/svg+xml",
         ".png": "image/png", ".woff2": "font/woff2", ".ts": "text/plain; charset=utf-8"}  # fmt: skip


class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: object) -> None:
        pass

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def panel_policy(prefix: str, origin: str) -> str:
    """The content security policy of a panel's files (plugin-files.ts panelPolicy)."""
    return "; ".join(
        [
            "default-src 'none'",
            f"script-src {prefix}",
            f"style-src {prefix} 'unsafe-inline'",
            f"img-src {prefix} data: blob:",
            f"font-src {prefix} data:",
            f"media-src {prefix} blob:",
            "connect-src 'none'",
            "frame-src 'none'",
            "child-src 'none'",
            "worker-src 'none'",
            "object-src 'none'",
            "manifest-src 'none'",
            "form-action 'none'",
            "base-uri 'none'",
            f"frame-ancestors {origin}",
            "sandbox allow-scripts",
            "webrtc 'block'",
        ]
    )


class _Demo(_Quiet):
    """The renderer, and plugins' web panels; ``plugin_dirs`` is set once the backend is ready."""

    plugin_dirs: tuple[str | None, str | None, str | None] = (None, None, None)

    def do_GET(self) -> None:
        path = urlsplit(self.path).path
        m = PANEL.match(path)
        if m:
            return self._panel(m.group(1), m.group(2) or "/")
        if path in ("/", "/index.html"):
            html = (RENDERER / "index.html").read_text(encoding="utf-8")
            # frames: only this server's pages (plugins' panels), as headless mode's servedIndex
            body = re.sub(r"frame-src [^;\"]*", "frame-src 'self'", html).encode()
            return self._send(200, body, {"Content-Type": "text/html; charset=utf-8"})
        return super().do_GET()

    def _send(self, status: int, body: bytes, headers: dict[str, str]) -> None:
        self.send_response(status)
        for k, v in headers.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _folder(self, pid: str) -> Path | None:
        installed, builtin, trust = self.plugin_dirs
        dev: list[str] = []
        if trust and Path(trust).exists():
            try:
                dev = [
                    d
                    for d in json.loads(Path(trust).read_text(encoding="utf-8")).get("dev", [])
                    if isinstance(d, str)
                ]
            except (OSError, ValueError):
                dev = []
        candidates = [Path(d) for d in dev if Path(d).name == pid] + [
            Path(r) / pid for r in (installed, builtin) if r
        ]
        return next((c for c in candidates if (c / "plugin.toml").is_file()), None)

    def _panel(self, pid: str, rest: str) -> None:
        missing = lambda: self._send(404, b"not found", {"Content-Type": "text/plain"})  # noqa: E731
        if not PLUGIN_ID.match(pid):
            return missing()
        parts = [p for p in re.split(r"[\\/]+", unquote(rest)) if p]
        if "_sk" in parts:
            name = "/".join(parts[parts.index("_sk") + 1 :])
            file = SDK / name if name in SDK_FILES else None
        else:
            folder = self._folder(pid)
            web = (folder / "web").resolve() if folder else None
            file = (web / "/".join(parts or ["index.html"])).resolve() if web else None
            if file and web and not file.is_relative_to(web):
                file = None
        if not file or not file.is_file():
            return missing()
        origin = f"http://{self.headers.get('Host', '')}"
        body = file.read_bytes()
        kind = TYPES.get(file.suffix.lower(), "application/octet-stream")
        if kind.startswith("text/html"):
            tag = f'<script src="/plugins/{pid}/web/_sk/guard.js"></script>'.encode()
            head = re.search(rb"<head[^>]*>", body, re.I)
            body = body[: head.end()] + tag + body[head.end() :] if head else tag + body
        self._send(
            200,
            body,
            {
                "Content-Type": kind,
                "Content-Security-Policy": panel_policy(f"{origin}/plugins/{pid}/web/", origin),
                "Access-Control-Allow-Origin": "*",
                "Cross-Origin-Resource-Policy": "cross-origin",
                "X-Content-Type-Options": "nosniff",
                "Referrer-Policy": "no-referrer",
            },
        )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--home", default=str(ROOT.parent / "SpaceKnife-demos" / "M3-web" / ".home"))
    args = ap.parse_args()
    if not (RENDERER / "index.html").exists():
        print("apps/electron/out/renderer is missing: run `pnpm build` in apps/electron", file=sys.stderr)
        return 1
    httpd = http.server.ThreadingHTTPServer(
        ("127.0.0.1", args.port), functools.partial(_Demo, directory=str(RENDERER))
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
    _Demo.plugin_dirs = (
        ready.get("plugins_dir"),
        ready.get("builtin_plugins_dir"),
        ready.get("plugins_trust"),
    )
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

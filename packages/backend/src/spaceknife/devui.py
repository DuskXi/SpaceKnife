"""The app in a browser tab, for development: ``spaceknife plugin dev`` and ``tools/demo/m3_web.py``.

Serves a built renderer (``apps/electron/out/renderer``, ``pnpm build``) and plugins' web panels the
way headless mode serves them (``apps/electron/src/main/plugin-files.ts``): ``/plugins/<id>/web/…``
from the plugin's folder (a development link, installed, shipped), ``…/_sk/…`` from the app's panel
SDK, each with the panel's content security policy and the guard first in its HTML; the page itself
may frame only this server's pages. The renderer's shim (``src/renderer/src/shim.ts``) stands in for
Electron's preload and connects to the backend named in the URL's hash.

``plugin dev`` also runs the backend with the plugin linked and watches its folder: every change is
described and started again, and the frontends hear ``extensions.changed``.
"""

from __future__ import annotations

import asyncio
import contextlib
import functools
import http.server
import json
import os
import re
import tempfile
import threading
from collections.abc import Callable
from pathlib import Path
from typing import Any
from urllib.parse import quote, unquote, urlsplit

# the app's own words for it (the terminal message names the page as the app shows it)
LINK_IN_APP = "设置 › 插件 › 链接开发文件夹 (developer mode)"  # i18n: ignore
PANEL = re.compile(r"^/plugins/([a-z][a-z0-9.-]{2,31})/web(/.*)?$")
PLUGIN_ID = re.compile(r"^[a-z][a-z0-9]*(-[a-z0-9]+)*(\.[a-z][a-z0-9]*(-[a-z0-9]+)*)+$")
TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".woff2": "font/woff2",
    ".ts": "text/plain; charset=utf-8",
}


def repo_renderer() -> Path | None:
    """``apps/electron/out/renderer`` of the repository this package runs from, when it is built."""
    for folder in Path(__file__).resolve().parents:
        candidate = folder / "apps" / "electron" / "out" / "renderer"
        if (candidate / "index.html").is_file():
            return candidate
    return None


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
            # no "webrtc 'block'": browsers that do not know it log an error for every panel; WebRTC
            # is taken away by the panel's guard (guard.js) and refused by plugin check instead
        ]
    )


class UiHandler(http.server.SimpleHTTPRequestHandler):
    """The renderer (``directory``) and plugins' panels. Set ``plugin_dirs`` (installed, shipped,
    trust file) once the backend is ready, ``linked`` for folders linked without the trust file."""

    plugin_dirs: tuple[str | None, str | None, str | None] = (None, None, None)
    linked: dict[str, Path] = {}  # noqa: RUF012  # set on a subclass

    def log_message(self, format: str, *args: object) -> None:
        pass

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_GET(self) -> None:
        path = urlsplit(self.path).path
        m = PANEL.match(path)
        if m:
            return self._panel(m.group(1), m.group(2) or "/")
        if path in ("/", "/index.html"):
            html = (Path(str(self.directory)) / "index.html").read_text(encoding="utf-8")
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
        if pid in self.linked:
            return self.linked[pid]
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
        from .plugins.check import PANEL_SDK, panel_sdk

        def missing() -> None:
            self._send(404, b"not found", {"Content-Type": "text/plain"})

        if not PLUGIN_ID.match(pid):
            return missing()
        parts = [p for p in re.split(r"[\\/]+", unquote(rest)) if p]
        body: bytes | None = None
        suffix = ""
        if "_sk" in parts:
            name = "/".join(parts[parts.index("_sk") + 1 :])
            if name in PANEL_SDK:
                body, suffix = panel_sdk()[name], Path(name).suffix
        else:
            folder = self._folder(pid)
            web = (folder / "web").resolve() if folder else None
            file = (web / "/".join(parts or ["index.html"])).resolve() if web else None
            if file is not None and web is not None and file.is_relative_to(web) and file.is_file():
                body, suffix = file.read_bytes(), file.suffix
        if body is None:
            return missing()
        origin = f"http://{self.headers.get('Host', '')}"
        kind = TYPES.get(suffix.lower(), "application/octet-stream")
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


def serve_ui(
    renderer: Path, *, port: int = 0, linked: dict[str, Path] | None = None
) -> tuple[Any, str, type]:
    """Start serving ``renderer`` on a thread; returns (server, origin, handler class)."""
    handler = type("DevUiHandler", (UiHandler,), {"linked": dict(linked or {})})
    httpd = http.server.ThreadingHTTPServer(
        ("127.0.0.1", port), functools.partial(handler, directory=str(renderer))
    )
    threading.Thread(target=httpd.serve_forever, name="sk-dev-ui", daemon=True).start()
    return httpd, f"http://127.0.0.1:{httpd.server_address[1]}", handler


def ui_url(origin: str, ws_url: str, token: str, scan: str | None = None) -> str:
    """The development page's address; ``scan``: a scan the page opens as a tab when it connects."""
    # paths=dialog: files and folders are typed in the app's own dialog, not the browser's prompt
    tail = "&paths=dialog" + (f"&scan={quote(scan)}" if scan else "")
    return f"{origin}/index.html#ws={quote(ws_url)}&token={quote(token)}{tail}"


# --------------------------------------------------------------------------- plugin dev


def _signature(folder: Path) -> tuple[tuple[str, int, int], ...]:
    from .plugins.store import plugin_files

    out = []
    try:
        for p in plugin_files(folder):
            rel = p.relative_to(folder).as_posix()
            if rel.startswith(("generated/", "web/_sk/", "web/sk-plugin.d.ts", "dist/")):
                continue  # written by plugin build: not a reason to reload
            st = p.stat()
            out.append((rel, st.st_mtime_ns, st.st_size))
    except (OSError, ValueError):
        pass
    return tuple(out)


def run_dev(
    folder: Path,
    *,
    scan: Path | None,
    port: int,
    ui: Path | None,
    home: Path | None,
    origins: list[str],
    say: Callable[[str], None] = print,
    stop: asyncio.Event | None = None,
    ready: Callable[[dict[str, Any]], None] | None = None,
) -> int:
    """``plugin dev``: see the module docstring. Returns the exit status (Ctrl+C ends it)."""
    from spaceknife_sdk.manifest import load_manifest

    from .log import LogFormat, setup_logging

    os.environ["SPACEKNIFE_PLUGIN_DEV"] = "1"
    temp = None
    if home is None:
        temp = tempfile.TemporaryDirectory(prefix="sk-plugin-dev-")
        home = Path(temp.name)
    os.environ["SPACEKNIFE_HOME"] = str(home)
    setup_logging(os.environ.get("SPACEKNIFE_LOG_LEVEL", "WARNING").upper(), LogFormat.PLAIN)
    plugin = load_manifest(folder).id
    renderer = ui if ui is not None else repo_renderer()
    httpd = origin = None
    if renderer is not None and (renderer / "index.html").is_file():
        httpd, origin, _handler = serve_ui(renderer, linked={plugin: folder})
    try:
        return asyncio.run(_dev(folder, plugin, scan, port, origin, origins, say, stop, ready))
    except KeyboardInterrupt:
        return 0
    finally:
        if httpd is not None:
            httpd.shutdown()
        if temp is not None:
            with contextlib.suppress(OSError):
                temp.cleanup()


async def _dev(
    folder: Path,
    plugin: str,
    scan: Path | None,
    port: int,
    origin: str | None,
    origins: list[str],
    say: Callable[[str], None],
    stop: asyncio.Event | None,
    ready: Callable[[dict[str, Any]], None] | None,
) -> int:
    from spaceknife_protocol.scan import ScanStartParams

    from .plugins.check import describe_in_subprocess
    from .server.app import DEFAULT_ORIGINS, Server, ServerConfig, new_token

    token = new_token()
    config = ServerConfig(
        token=token,
        host="127.0.0.1",
        port=port,
        allowed_origins=DEFAULT_ORIGINS + tuple(origins) + ((origin,) if origin else ()),
    )
    server = Server(config)
    await server.start()
    host = server.plugins
    stop = stop or asyncio.Event()

    def report(rec: Any, what: str) -> None:
        if rec.state == "enabled":
            say(f"{what}: {plugin} {rec.manifest.version if rec.manifest else ''} enabled")
            return
        say(f"{what}: {plugin} is {rec.state}")
        for i in rec.issues:
            say(f"  {i.text()}")
        if rec.manifest is not None and rec.manifest.entry is not None:
            _desc, issues = await_describe(folder)
            for i in issues:
                say(f"  {i.text()}")

    def await_describe(f: Path) -> tuple[Any, list[Any]]:
        return describe_in_subprocess(f)

    try:
        rec = await host.link_dev(folder)
        report(rec, "linked")
        info = {"url": server.url, "token": token, "plugin": plugin, "folder": str(folder)}
        # the pid: stop this one with kill / Stop-Process -Id, not a pkill -f that matches more
        say(f"backend: {server.url}  token: {token}  pid: {os.getpid()}")
        scan_id = None
        if scan is not None:  # started first: the page's address opens it
            root = await asyncio.to_thread(lambda: str(scan.expanduser().resolve()))
            scan_id = (await server.scans.start(ScanStartParams(roots=[root]))).id
            say(f"scanning {scan}")
        if origin is not None:
            info["ui"] = ui_url(origin, server.url, token, scan_id)
            say(f"open in a browser: {info['ui']}")
        else:
            say("no built renderer (pnpm build in apps/electron, or --ui DIR): connect a frontend")
            say("to the backend above (--allow-origin), or link the folder in the app:")
            say("  " + LINK_IN_APP + f": {folder}")
        if ready is not None:
            ready(info)
        seen = _signature(folder)
        while not stop.is_set():
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(stop.wait(), 0.5)
            if stop.is_set():
                break
            now = _signature(folder)
            if now == seen:
                continue
            await asyncio.sleep(0.3)  # an editor writes in several steps
            seen = _signature(folder)
            try:
                old = host.records.get(plugin)
                if old is None or old.state in ("error", "incompatible", "needs_confirm", "changed"):
                    rec = await host.link_dev(folder)
                else:
                    await host.restart(plugin)
                    rec = host.records[plugin]
            except Exception as exc:  # keep watching: the next save may fix it
                say(f"reload failed: {getattr(exc, 'message', exc)}")
                continue
            report(rec, "reloaded")
    finally:
        await server.close()
    return 0


__all__ = ["PANEL", "UiHandler", "panel_policy", "repo_renderer", "run_dev", "serve_ui", "ui_url"]

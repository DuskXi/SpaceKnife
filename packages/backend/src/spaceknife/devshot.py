"""``spaceknife plugin shot``: a picture of the real interface with a plugin enabled, headless.

One command instead of a browser driven by hand: a backend with the plugin linked (its own
temporary data folder and port), a scan (of ``--scan``, else a small synthetic folder), the built
interface served as ``plugin dev`` serves it, and a headless Chromium (Chrome, Edge or Chromium;
``SPACEKNIFE_BROWSER`` names one) opened on the development link. The link's extra words
(``state/devlink.ts``: theme, lang, select, do, open) bring the page to the state asked for; when
the page says it is there (``data-sk-dev-ready``), the picture is taken. Printed: where the PNG is,
what the page logged as errors or warnings, what the plugin logged, and with ``--text`` the
visible text of the page. Nothing opens on the desktop.

The browser speaks the DevTools protocol over a WebSocket (``websockets``, already a dependency of
the backend); nothing else is installed.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import quote

READY_S = 45.0  # the page connects, opens the scan, applies the link
DEBUG = bool(os.environ.get("SPACEKNIFE_SHOT_DEBUG"))  # every step and DevTools event to stderr
BROWSER_S = 20.0  # the browser starts and says its port

# a small tree when no --scan is given: a bit of everything a plugin looks at
TREE: Mapping[str, Any] = {
    "a.log": 5000,
    "b.txt": (1000, "2020-01-01"),
    "movie.mp4": (60000, "2019-06-01"),
    "photo.jpg": 30000,
    "old.bak": 3000,
    "sub": {"c.log": 2000, "d.bin": 10000, "e.dat": 700, "deep": {"f.txt": 70}},
    "project": {"Cargo.toml": 300, "node_modules": {"x.js": 40000}, "target": {"out.o": 9000}},
    "docs": {"readme.md": 500, "notes.txt": 1200},
    "empty": {},
}


def write_tree(spec: Mapping[str, Any], root: Path) -> None:
    """Files of ``spec`` (``make_tree``'s form: size, (size, "YYYY-MM-DD"), bytes, {…}) in ``root``."""
    import datetime as dt

    root.mkdir(parents=True, exist_ok=True)
    for name, value in spec.items():
        path = root / name
        if isinstance(value, Mapping):
            write_tree(value, path)
            continue
        when = None
        if isinstance(value, tuple):
            value, when = value
        data = value if isinstance(value, bytes) else b"\0" * int(value)
        path.write_bytes(data)
        if when is not None:
            t = dt.datetime.fromisoformat(str(when)).timestamp() if isinstance(when, str) else float(when)
            os.utime(path, (t, t))


# --------------------------------------------------------------------------- the built interface


def renderer_stale(renderer: Path) -> str | None:
    """Why ``renderer`` (a build of the repository's apps/electron) is older than its sources, or
    None: the sources newer than the build (an old interface misses what the plugin system can do)."""
    index = renderer / "index.html"
    try:
        built = index.stat().st_mtime
    except OSError:
        return f"{renderer} has no index.html: build it (pnpm -C apps/electron build)"
    for folder in Path(__file__).resolve().parents:
        app = folder / "apps" / "electron"
        if (app / "package.json").is_file():
            break
    else:
        return None  # the packaged app: its interface is the one it came with
    newest, which = 0.0, ""
    for sub in ("src/renderer", "resources/plugin-sdk"):
        for p in (app / sub).rglob("*"):
            if p.suffix in (".ts", ".tsx", ".css", ".js", ".html") and ".test." not in p.name:
                with contextlib.suppress(OSError):
                    m = p.stat().st_mtime
                    if m > newest:
                        newest, which = m, str(p.relative_to(app))
    if newest > built + 1:
        age = time.strftime("%Y-%m-%d %H:%M", time.localtime(built))
        return (
            f"the interface in {renderer} was built {age}, before {which} changed: it may lack what "
            "the plugin needs. Rebuild (pnpm -C apps/electron build) or pass --ui <a newer build> "
            "(SPACEKNIFE_UI)"
        )
    return None


def find_renderer(ui: Path | None) -> Path | None:
    """``--ui``, else ``SPACEKNIFE_UI``, else the repository's apps/electron/out/renderer."""
    from .devui import repo_renderer

    for candidate in (ui, Path(os.environ["SPACEKNIFE_UI"]) if os.environ.get("SPACEKNIFE_UI") else None):
        if candidate is not None:
            c = candidate.expanduser()
            if (c / "renderer" / "index.html").is_file():  # the out/ folder of a build
                return c / "renderer"
            return c
    return repo_renderer()


# --------------------------------------------------------------------------- the browser


def find_browser() -> str | None:
    """A Chromium to run headless: ``SPACEKNIFE_BROWSER``, else Chrome, Chromium or Edge."""
    env = os.environ.get("SPACEKNIFE_BROWSER")
    if env:
        return env
    names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome", "msedge"]
    for n in names:
        found = shutil.which(n)
        if found:
            return found
    if sys.platform == "win32":
        bases = [os.environ.get(k, "") for k in ("PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA")]
        rels = [r"Google\Chrome\Application\chrome.exe", r"Microsoft\Edge\Application\msedge.exe"]
        for b in bases:
            for r in rels:
                if b and Path(b, r).is_file():
                    return str(Path(b, r))
    if sys.platform == "darwin":
        for app in ("Google Chrome", "Chromium", "Microsoft Edge"):
            p = Path(f"/Applications/{app}.app/Contents/MacOS/{app}")
            if p.is_file():
                return str(p)
    return None


@dataclass
class Page:
    """One page of a headless browser, driven over the DevTools protocol."""

    ws: Any
    ids: int = 0
    waiting: dict[int, asyncio.Future[Any]] = field(default_factory=dict)
    logged: list[str] = field(default_factory=list)
    reader: asyncio.Task[None] | None = None
    frames: dict[str, str] = field(default_factory=dict)  # session id -> url: plugins' panels
    tasks: set[asyncio.Task[Any]] = field(default_factory=set)

    async def start(self) -> None:
        self.reader = asyncio.create_task(self._read())
        for method in ("Page.enable", "Runtime.enable", "Log.enable"):
            await self.send(method)
        # the panels are frames of their own (sandboxed): their logs and text come through sessions
        await self.send(
            "Target.setAutoAttach", {"autoAttach": True, "waitForDebuggerOnStart": False, "flatten": True}
        )

    async def _read(self) -> None:
        with contextlib.suppress(Exception):
            async for raw in self.ws:
                msg = json.loads(raw)
                if "id" in msg:
                    fut = self.waiting.pop(int(msg["id"]), None)
                    if fut is not None and not fut.done():
                        fut.set_result(msg)
                    continue
                self._event(msg.get("method", ""), msg.get("params") or {}, msg.get("sessionId"))

    def _later(self, coro: Any) -> None:
        task = asyncio.get_running_loop().create_task(coro)
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def _watch_frame(self, session: str) -> None:
        with contextlib.suppress(Exception):
            for method in ("Runtime.enable", "Log.enable"):
                await self.send(method, session=session, timeout=5)

    def _event(self, method: str, p: dict[str, Any], session: str | None = None) -> None:
        if DEBUG:
            sys.stderr.write(f"[shot] event {method}{' (frame)' if session else ''}\n")
        if method == "Target.attachedToTarget":
            info = p.get("targetInfo") or {}
            sid = str(p.get("sessionId", ""))
            if sid:
                self.frames[sid] = str(info.get("url", ""))
                self._later(self._watch_frame(sid))
            return
        if method == "Target.detachedFromTarget":
            self.frames.pop(str(p.get("sessionId", "")), None)
            return
        where = " [panel]" if session else ""
        if method == "Runtime.consoleAPICalled" and p.get("type") in ("error", "warning", "assert"):
            args = " ".join(str(a.get("value", a.get("description", ""))) for a in p.get("args", []))
            self.logged.append(f"console.{p.get('type')}{where}: {args[:500]}")
            return
        if method == "Runtime.exceptionThrown":
            d = p.get("exceptionDetails") or {}
            text = (d.get("exception") or {}).get("description") or d.get("text") or "exception"
            self.logged.append(f"exception{where}: {str(text)[:800]}")
            return
        if method == "Page.javascriptDialogOpening":  # an alert / confirm / prompt would block the page
            self.logged.append(f"dialog ({p.get('type')}): {str(p.get('message', ''))[:300]} (dismissed)")
            asyncio.get_running_loop().create_task(
                self.send("Page.handleJavaScriptDialog", {"accept": False}, timeout=5)
            )
            return
        if method == "Log.entryAdded":
            e = p.get("entry") or {}
            url = str(e.get("url", ""))
            # what the page itself asked for (not a favicon, nor what a security product injects)
            ours = not url or url.startswith(("http://127.0.0.1", "http://localhost"))
            if e.get("level") in ("error", "warning") and ours and not url.endswith("/favicon.ico"):
                where = f" ({url})" if url else ""
                self.logged.append(
                    f"{e.get('source', 'log')} {e.get('level')}: {str(e.get('text', ''))[:500]}{where}"
                )

    async def send(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        timeout: float = 30.0,  # noqa: ASYNC109
        session: str | None = None,
    ) -> Any:
        if DEBUG:
            sys.stderr.write(f"[shot] send {method}\n")
        self.ids += 1
        fut: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        self.waiting[self.ids] = fut
        msg: dict[str, Any] = {"id": self.ids, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        await self.ws.send(json.dumps(msg))
        msg = await asyncio.wait_for(fut, timeout)
        if "error" in msg:
            raise RuntimeError(f"{method}: {msg['error'].get('message')}")
        return msg.get("result", {})

    async def eval(self, expr: str, session: str | None = None) -> Any:
        r = await self.send(
            "Runtime.evaluate",
            {"expression": expr, "returnByValue": True, "awaitPromise": True},
            session=session,
        )
        if r.get("exceptionDetails"):
            raise RuntimeError(str(r["exceptionDetails"].get("text", "evaluation failed")))
        return (r.get("result") or {}).get("value")

    async def png(self) -> bytes:
        r = await self.send("Page.captureScreenshot", {"format": "png", "captureBeyondViewport": False})
        return base64.b64decode(r["data"])


def _open_browser(exe: str, width: int, height: int, profile: Path) -> tuple[subprocess.Popen[bytes], str]:
    args = [
        exe,
        "--headless=new",
        "--remote-debugging-port=0",
        f"--user-data-dir={profile}",
        f"--window-size={width},{height}",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-background-networking",
        "--disable-dev-shm-usage",
        "--hide-scrollbars",
        "--mute-audio",
        "about:blank",
    ]
    proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    port_file = profile / "DevToolsActivePort"
    deadline = time.monotonic() + BROWSER_S
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"{exe} ended at once (exit {proc.returncode})")
        with contextlib.suppress(OSError, ValueError, IndexError):
            port = int(port_file.read_text().splitlines()[0])
            pages = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=2).read())
            page = next(p for p in pages if p.get("type") == "page")
            return proc, str(page["webSocketDebuggerUrl"])
        time.sleep(0.2)
    proc.kill()
    raise RuntimeError(f"{exe} did not start its DevTools endpoint within {BROWSER_S:.0f} s")


# --------------------------------------------------------------------------- plugin shot


@dataclass
class ShotOptions:
    scan: Path | None = None
    ui: Path | None = None
    width: int = 1400
    height: int = 900
    out: Path | None = None
    theme: str | None = None
    lang: str | None = None
    select: str | None = None
    run: list[str] = field(default_factory=list)
    open: str | None = None
    wait: float = 1.0
    text: bool = False
    eval: str | None = None


def link_words(o: ShotOptions) -> str:
    """The development link's extra words (state/devlink.ts)."""
    words = [("theme", o.theme), ("lang", o.lang), ("select", o.select), ("open", o.open)]
    tail = "".join(f"&{k}={quote(v)}" for k, v in words if v)
    if o.run:
        tail += "&do=" + quote(",".join(o.run))
    return tail


def run_shot(folder: Path, o: ShotOptions, say: Callable[[str], None] = print) -> int:
    """See the module docstring. Returns the exit status (1: no picture, 2: nothing to run it with)."""
    from spaceknife_sdk.manifest import load_manifest

    from .log import LogFormat, setup_logging

    renderer = find_renderer(o.ui)
    if renderer is None or not (renderer / "index.html").is_file():
        say(
            "no built interface: pnpm -C apps/electron build, or --ui <out/renderer of a build> "
            "(SPACEKNIFE_UI)"
        )
        return 2
    stale = renderer_stale(renderer)
    if stale:
        say(f"warning: {stale}")
    exe = find_browser()
    if exe is None:
        say("no Chrome, Chromium or Edge found: set SPACEKNIFE_BROWSER to one")
        return 2
    os.environ["SPACEKNIFE_PLUGIN_DEV"] = "1"
    base = Path(tempfile.mkdtemp(prefix="sk-plugin-shot-"))
    try:
        os.environ["SPACEKNIFE_HOME"] = str(base / "home")
        setup_logging(os.environ.get("SPACEKNIFE_LOG_LEVEL", "WARNING").upper(), LogFormat.PLAIN)
        plugin = load_manifest(folder).id
        scan = o.scan
        if scan is None:
            scan = base / "tree"
            write_tree(TREE, scan)
        return asyncio.run(_shot(folder, plugin, scan.expanduser().resolve(), renderer, exe, base, o, say))
    finally:
        # the browser lets go of its profile a moment after it ends (Windows keeps the files locked)
        for _ in range(20):
            shutil.rmtree(base, ignore_errors=True)
            if not base.exists():
                break
            time.sleep(0.25)


async def _shot(
    folder: Path,
    plugin: str,
    scan: Path,
    renderer: Path,
    exe: str,
    base: Path,
    o: ShotOptions,
    say: Callable[[str], None],
) -> int:
    import websockets
    from loguru import logger

    from spaceknife_protocol.scan import ScanStartParams

    from .devui import serve_ui, ui_url
    from .server.app import DEFAULT_ORIGINS, Server, ServerConfig, new_token

    plugin_log: list[str] = []
    sink = logger.add(
        lambda m: plugin_log.append(str(m).rstrip()),
        level="WARNING",
        filter=lambda r: (
            str(r["extra"].get("source", "")).startswith(f"plugin:{plugin}") or plugin in str(r["message"])
        ),
    )
    httpd, origin, _handler = serve_ui(renderer, linked={plugin: folder})
    token = new_token()
    server = Server(
        ServerConfig(token=token, host="127.0.0.1", port=0, allowed_origins=(*DEFAULT_ORIGINS, origin))
    )
    await server.start()
    proc: subprocess.Popen[bytes] | None = None
    try:
        rec = await server.plugins.link_dev(folder)
        if rec.state != "enabled":
            say(f"{plugin} is {rec.state}: " + "; ".join(i.text() for i in rec.issues))
            return 1
        handle = await server.scans.start(ScanStartParams(roots=[str(scan)]))
        if handle.task is not None:
            await handle.task
        url = ui_url(origin, server.url, token, str(handle.id)) + link_words(o)
        proc, ws_url = await asyncio.to_thread(_open_browser, exe, o.width, o.height, base / "browser")
        async with websockets.connect(ws_url, max_size=None) as ws:
            page = Page(ws)
            await page.start()
            await page.send(
                "Emulation.setDeviceMetricsOverride",
                {"width": o.width, "height": o.height, "deviceScaleFactor": 1, "mobile": False},
            )
            await page.send("Page.navigate", {"url": url})
            deadline = time.monotonic() + READY_S
            ready = ""
            while time.monotonic() < deadline:
                with contextlib.suppress(RuntimeError, TimeoutError):
                    ready = await page.eval("document.documentElement.dataset.skDevReady || ''") or ""
                if ready:
                    break
                await asyncio.sleep(0.25)
            if not ready:
                say(f"the page did not get ready within {READY_S:.0f} s (it shows what it reached)")
            error = await page.eval("document.documentElement.dataset.skDevError || ''")
            if error:
                say(f"the link's actions failed: {error}")
            await asyncio.sleep(max(0.0, o.wait))
            out = (o.out or Path.cwd() / f"shot-{plugin}.png").expanduser().resolve()
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_bytes(await page.png())
            say(f"picture: {out}")
            if o.eval:
                say("eval: " + json.dumps(await page.eval(o.eval), ensure_ascii=False, default=str)[:4000])
            if o.text:
                text = await page.eval("(document.querySelector('main') || document.body).innerText")
                say("text:\n" + str(text or "")[:4000])
                for sid, url in list(page.frames.items()):
                    with contextlib.suppress(RuntimeError, TimeoutError):
                        inner = await page.eval("document.body ? document.body.innerText : ''", session=sid)
                        say(f"panel text ({url.rsplit('/web/', 1)[-1]}):\n" + str(inner or "")[:3000])
            for line in _unique(page.logged):
                say(f"page: {line}")
            with contextlib.suppress(Exception):
                await page.send("Browser.close", timeout=5)
        for line in _unique(plugin_log):
            say(f"backend: {line[:500]}")
        return 0
    finally:
        logger.remove(sink)
        if proc is not None:
            with contextlib.suppress(Exception):
                proc.wait(5)
            if proc.poll() is None:
                proc.kill()
        await server.close()
        httpd.shutdown()


def _unique(lines: Iterable[str]) -> list[str]:
    return list(dict.fromkeys(lines))[:40]


__all__ = ["TREE", "ShotOptions", "find_browser", "find_renderer", "renderer_stale", "run_shot", "write_tree"]

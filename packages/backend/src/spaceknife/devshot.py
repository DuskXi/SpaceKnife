"""``spaceknife plugin shot``: a picture of the real interface with a plugin enabled, headless.

One command instead of a browser driven by hand: a backend with the plugin linked (its own
temporary data folder and port), a scan (of ``--scan``, else a small synthetic folder), the built
interface served as ``plugin dev`` serves it, and a headless Chromium (Chrome, Edge or Chromium;
``SPACEKNIFE_BROWSER`` names one) opened on the development link. The link's extra words
(``state/devlink.ts``: theme, lang, select, do, open) bring the page to the state asked for; when
the page says it is there (``data-sk-dev-ready``), the picture is taken. Printed: where the PNG is,
what the page logged as errors or warnings, what the plugin logged, and with ``--text`` the
visible text of the page. Nothing opens on the desktop.

Steps (``--step``, in the order given, after the page is ready and before the picture) act on the
page as a user would: ``click:`` ``rclick:`` (a context menu) ``dblclick:`` ``hover:`` an element (a
CSS selector, or ``text=<its visible text>``), a node where the view draws it (``node=<a/b>``,
relative to the root; ``node=`` the zoom root) or a point (``@x,y``, CSS px of the window: the
picture's pixels); ``set:<element>=<value>`` (an input, a select, a check box: ``true``/``false``),
``type:<text>`` (typed into the focused input), ``key:<Key>`` (``Escape``, ``Enter``,
``Control+K`` …), ``select:<a/b>`` (a node, relative to the root), ``do:<command>``,
``open:<target>`` (as ``--open``), ``wait:<s>``, ``eval:<JavaScript>`` (prints what it gives).
``panel:<step>`` runs a click, set, type, key, eval or wait inside the plugin's open panel (its own
frame). ``--setting``, ``--history`` and ``--source`` set the backend up first: the plugin's
settings, snapshots in its history, a scan from the plugin's source. A step that fails stops the
rest (the picture shows where) and the exit status is 3.

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
            raise ScriptError(js_error(r["exceptionDetails"]))
        return (r.get("result") or {}).get("value")

    async def png(self) -> bytes:
        r = await self.send("Page.captureScreenshot", {"format": "png", "captureBeyondViewport": False})
        return base64.b64decode(r["data"])


class ScriptError(RuntimeError):
    """JavaScript the page ran threw: the message is the error's (``TypeError: x is not a function``)."""


def js_error(details: Mapping[str, Any]) -> str:
    """What a DevTools ``exceptionDetails`` says, as the console would: the error's name and
    message (``ReferenceError: foo is not defined``), else what was thrown, else its text."""
    exc = details.get("exception") or {}
    desc = str(exc.get("description") or "")
    if desc:  # a stack follows the message ("    at <anonymous>:1:1"): the message is what helps
        head = [ln.strip() for ln in desc.splitlines() if not ln.lstrip().startswith("at ")]
        return " ".join(head)[:800] or desc.splitlines()[0][:800]
    if "value" in exc:  # throw "text" / throw 3
        return f"Uncaught {json.dumps(exc['value'], ensure_ascii=False)}"[:800]
    text = str(details.get("text") or "the script failed")
    line = details.get("lineNumber")
    return f"{text} (line {int(line) + 1})" if isinstance(line, int) else text


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
    steps: list[str] = field(default_factory=list)
    settings: dict[str, Any] = field(default_factory=dict)
    history: list[Path] = field(default_factory=list)
    source: str | None = None  # "<full source id>:<uri>": the scan comes from the plugin's source


STEP_KINDS = (
    "click", "rclick", "dblclick", "hover", "set", "type", "key", "select", "do", "open", "wait", "eval",
)  # fmt: skip
PANEL_STEPS = ("click", "rclick", "dblclick", "hover", "set", "type", "key", "wait", "eval")
POINTING = ("click", "rclick", "dblclick", "hover")


class StepError(Exception):
    """A ``--step`` that is written wrong or could not be done (its element is not there …)."""


def parse_step(text: str) -> tuple[bool, str, str]:
    """``[panel:]<kind>:<argument>`` -> (inside the panel, kind, argument)."""
    panel = text.startswith("panel:")
    body = text[6:] if panel else text
    kind, sep, arg = body.partition(":")
    kind = kind.strip().lower()
    if not sep or kind not in STEP_KINDS:
        raise StepError(f"{text!r}: write <kind>:<argument>, kind one of {', '.join(STEP_KINDS)}")
    if panel and kind not in PANEL_STEPS:
        raise StepError(f"{text!r}: inside a panel only {', '.join(PANEL_STEPS)}")
    if kind == "set" and _split_set(arg) is None:
        raise StepError(f"{text!r}: write set:<element>=<value>")
    if kind in POINTING and arg.startswith(("node=", "@")):
        if panel:
            raise StepError(f"{text!r}: inside a panel, an element (a CSS selector or text=…)")
        if arg.startswith("@") and _point(arg) is None:
            raise StepError(f"{text!r}: write {kind}:@x,y (CSS px of the window, as in the picture)")
    if kind == "wait":
        try:
            float(arg)
        except ValueError:
            raise StepError(f"{text!r}: wait:<seconds>") from None
    return panel, kind, arg


def _point(arg: str) -> tuple[float, float] | None:
    """``@x,y`` -> (x, y), or None when it is not two numbers."""
    x, sep, y = arg[1:].partition(",")
    try:
        return (float(x), float(y)) if sep else None
    except ValueError:
        return None


def _split_set(arg: str) -> tuple[str, str] | None:
    """``<element>=<value>`` at the first ``=`` outside brackets and quotes (``input[name=a]=5``)."""
    depth, quote_ch = 0, ""
    for i, ch in enumerate(arg):
        if quote_ch:
            if ch == quote_ch:
                quote_ch = ""
        elif ch in "\"'":
            quote_ch = ch
        elif ch == "[":
            depth += 1
        elif ch == "]":
            depth -= 1
        elif ch == "=" and depth == 0 and i > 0:
            return arg[:i].strip(), arg[i + 1 :]
    return None


# what a step runs in the page: find an element by CSS selector or by text= (the innermost visible
# element whose own text is that), scroll it into view, act on it
_FIND_JS = """
(t) => {
  const shown = (e) => e.getClientRects().length > 0;
  if (t.startsWith("text=")) {
    const want = t.slice(5).trim();
    const all = [...document.querySelectorAll("body *")].filter(
      (e) => shown(e) && ((e.innerText || e.value || e.getAttribute("aria-label") || "").trim() === want));
    return all.find((e) => !all.some((o) => o !== e && e.contains(o))) || null;
  }
  const hit = [...document.querySelectorAll(t)].filter(shown);
  return hit[0] || document.querySelector(t);
}
"""
_SET_JS = """
(el, v) => {
  el.focus();
  if (el.type === "checkbox" || el.type === "radio") {
    const want = ["1", "true", "on", "yes"].includes(v.trim().toLowerCase());
    if (el.checked !== want) el.click();
    return;
  }
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
    : el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value").set.call(el, v);
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
  el.blur();
}
"""
# type: inside a panel's frame (Input.insertText reaches the page's focused frame only): the
# editing command keeps the input's undo and events as typing would; else the value is set
_TYPE_JS = """
  const text = __TEXT__;
  if (document.execCommand && document.execCommand("insertText", false, text)) return true;
  if (!("value" in t)) return false;
  const proto = t instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const at = t.selectionStart ?? t.value.length;
  const end = t.selectionEnd ?? at;
  const value = t.value.slice(0, at) + text + t.value.slice(end);
  Object.getOwnPropertyDescriptor(proto, "value").set.call(t, value);
  t.dispatchEvent(new InputEvent("input", { bubbles: true, data: text, inputType: "insertText" }));
  return true;
"""
_KEY_CODES = {
    "Enter": 13, "Escape": 27, "Tab": 9, "Backspace": 8, "Delete": 46, " ": 32, "Space": 32,
    "ArrowLeft": 37, "ArrowUp": 38, "ArrowRight": 39, "ArrowDown": 40, "Home": 36, "End": 35,
    "PageUp": 33, "PageDown": 34, **{f"F{i}": 111 + i for i in range(1, 13)},
}  # fmt: skip
_MODIFIERS = {"alt": 1, "control": 2, "ctrl": 2, "meta": 4, "cmd": 4, "shift": 8}


def _key(spec: str) -> tuple[str, int, int, str]:
    """``Control+Shift+K`` -> (key, its key code, modifier bits, text typed)."""
    parts = [p for p in spec.split("+") if p] or [spec]
    mods = 0
    for m in parts[:-1]:
        bit = _MODIFIERS.get(m.strip().lower())
        if bit is None:
            raise StepError(f"key:{spec}: unknown modifier {m!r}")
        mods |= bit
    key = parts[-1].strip() if parts[-1].strip() else parts[-1]
    key = " " if key == "Space" else key
    code = _KEY_CODES.get(key, ord(key.upper()) if len(key) == 1 else 0)
    text = key if len(key) == 1 and not mods & (1 | 2 | 4) else ""
    return key, code, mods, text


async def run_step(page: Page, step: str) -> str:
    """Do one ``--step``; what it says (``eval``), else "". StepError when it cannot."""
    panel, kind, arg = parse_step(step)
    session: str | None = None
    if panel:
        frames = [sid for sid, url in page.frames.items() if "/web/" in url]
        if not frames:
            raise StepError("no plugin panel is open (open one first: --open panel:<name>, do:, click:)")
        session = frames[-1]
    if kind == "wait":
        await asyncio.sleep(max(0.0, float(arg)))
        return ""
    if kind == "eval":
        try:
            value = await page.eval(arg, session=session)
        except ScriptError as exc:
            raise StepError(f"the script threw {exc}") from None
        return json.dumps(value, ensure_ascii=False, default=str)[:4000]
    if kind == "type":
        if session is None:  # as the keyboard would (an IME commit): input events, React sees them
            await page.send("Input.insertText", {"text": arg})
            return ""
        js = (
            "(() => { const t = document.activeElement; if (!t || t === document.body) return false; "
            + _TYPE_JS
            + " })()"
        )
        if not await page.eval(js.replace("__TEXT__", json.dumps(arg)), session=session):
            raise StepError("nothing in the panel has the focus (click: an input first)")
        return ""
    if kind in ("select", "do", "open"):
        call = {"select": "select", "do": "run", "open": "open"}[kind]
        expr = f"window.__skDev ? window.__skDev.{call}({json.dumps(arg)}).then(() => 'ok') : 'no hooks'"
        got = await page.eval(expr)
        if got != "ok":
            raise StepError("the page has no development hooks (a build older than this command)")
        return ""
    if kind == "key":
        key, code, mods, text = _key(arg)
        if session is not None:
            init = json.dumps(
                {
                    "key": key,
                    "bubbles": True,
                    "ctrlKey": bool(mods & 2),
                    "altKey": bool(mods & 1),
                    "shiftKey": bool(mods & 8),
                    "metaKey": bool(mods & 4),
                }
            )
            js = (
                "(() => { const t = document.activeElement || document.body; for (const type of "
                f"['keydown', 'keyup']) t.dispatchEvent(new KeyboardEvent(type, {init})); }})()"
            )
            await page.eval(js, session=session)
            return ""
        base = {"key": key, "code": key if len(key) > 1 else f"Key{key.upper()}", "modifiers": mods}
        base["windowsVirtualKeyCode"] = code
        await page.send(
            "Input.dispatchKeyEvent", {"type": "keyDown", **base, **({"text": text} if text else {})}
        )
        await page.send("Input.dispatchKeyEvent", {"type": "keyUp", **base})
        return ""
    target, value = (_split_set(arg) or ("", "")) if kind == "set" else (arg, "")
    find = f"({_FIND_JS})({json.dumps(target)})"
    if kind == "set":
        js = (
            f"(() => {{ const el = {find}; if (!el) return false; "
            f"({_SET_JS})(el, {json.dumps(value)}); return true; }})()"
        )
        if not await page.eval(js, session=session):
            raise StepError(f"no element {target!r}")
        return ""
    if session is not None:  # inside a panel's frame: events from script
        event = {"rclick": "contextmenu", "dblclick": "dblclick", "hover": "mouseover"}.get(kind)
        init = "{ bubbles: true, cancelable: true }"
        act = (
            "el.click();"
            if event is None
            else f"el.dispatchEvent(new MouseEvent({json.dumps(event)}, {init}));"
        )
        js = f"(() => {{ const el = {find}; if (!el) return false; {act} return true; }})()"
        if not await page.eval(js, session=session):
            raise StepError(f"no element {target!r} in the panel")
        return ""
    if target.startswith("@"):
        x, y = _point(target) or (0.0, 0.0)
    elif target.startswith("node="):  # where the view draws it: the page's development hooks say
        x, y = await _node_point(page, target[5:])
    else:
        where = await page.eval(
            f"(() => {{ const el = {find}; if (!el) return null; "
            "el.scrollIntoView({ block: 'center', inline: 'center' }); "
            "const r = el.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()"
        )
        if not where:
            raise StepError(f"no element {target!r}")
        x, y = float(where[0]), float(where[1])
    await page.send("Input.dispatchMouseEvent", {"type": "mouseMoved", "x": x, "y": y})
    if kind == "hover":
        return ""
    button = "right" if kind == "rclick" else "left"
    clicks = 2 if kind == "dblclick" else 1
    for n in range(1, clicks + 1):
        for type_ in ("mousePressed", "mouseReleased"):
            await page.send(
                "Input.dispatchMouseEvent",
                {
                    "type": type_,
                    "x": x,
                    "y": y,
                    "button": button,
                    "clickCount": n,
                    "buttons": 1 if button == "left" else 2,
                },
            )
    return ""


async def _node_point(page: Page, rel: str) -> tuple[float, float]:
    """Where the view draws the node at ``rel`` (client px), asked of the page (state/devlink.ts)."""
    rel = rel.strip().strip("/\\")
    hooks = "window.__skDev && window.__skDev.where"
    try:
        at = await page.eval(f"{hooks} ? window.__skDev.where({json.dumps(rel)}) : 'no hooks'")
    except ScriptError as exc:
        raise StepError(str(exc).removeprefix("Error: ")) from None
    if not isinstance(at, dict):
        raise StepError("the page cannot say where nodes are drawn (an interface older than this step)")
    return float(at["x"]), float(at["y"])


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
    failed = False  # a step or --eval failed: exit status 3
    try:
        rec = await server.plugins.link_dev(folder)
        if rec.state != "enabled":
            say(f"{plugin} is {rec.state}: " + "; ".join(i.text() for i in rec.issues))
            return 1
        if o.settings:  # the plugin's settings, as the settings page would set them
            from .settings import SettingsError

            for k, v in o.settings.items():
                try:
                    server.settings.update({f"plugin.{plugin}.{k}": v})
                except (SettingsError, KeyError, ValueError, TypeError) as exc:
                    say(f"--setting {k}: {exc}")
                    return 2
        if o.history:
            _copy_history(o.history)
        if o.source:
            sid, _, uri = o.source.partition(":")
            handle = await server.scans.start(ScanStartParams(source=sid, uri=uri))
        else:
            handle = await server.scans.start(ScanStartParams(roots=[str(scan)]))
        if handle.task is not None:
            await handle.task
        if handle.status().state == "failed":
            say(f"the scan failed: {handle.status().message}")
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
            for step in o.steps:  # in order, as a user would; one that cannot stops the rest
                try:
                    said = await run_step(page, step)
                except (StepError, RuntimeError, TimeoutError) as exc:
                    say(f"step {step!r} failed: {exc} (the picture shows where it stopped)")
                    failed = True
                    break
                if said:
                    say(f"step {step!r}: {said}")
                await asyncio.sleep(0.2)
            await asyncio.sleep(max(0.0, o.wait))
            if o.eval:  # before the picture: what it reads is what the picture shows
                try:
                    said = json.dumps(await page.eval(o.eval), ensure_ascii=False, default=str)[:4000]
                    say(f"eval: {said}")
                except ScriptError as exc:
                    say(f"eval failed: {exc}")
                    failed = True
            out = (o.out or Path.cwd() / f"shot-{plugin}.png").expanduser().resolve()
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_bytes(await page.png())
            say(f"picture: {out}")
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
        return 3 if failed else 0
    finally:
        logger.remove(sink)
        if proc is not None:
            with contextlib.suppress(Exception):
                proc.wait(5)
            if proc.poll() is None:
                proc.kill()
        await server.close()
        httpd.shutdown()


def _copy_history(files: list[Path]) -> None:
    """``--history FILE``: snapshots copied into the temporary backend's history."""
    from .store import snapshot as snap

    for f in files:
        header = snap.read_header(f)
        target = snap.auto_path(list(header.roots), header.created_at)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(f, target)


def _unique(lines: Iterable[str]) -> list[str]:
    return list(dict.fromkeys(lines))[:40]


__all__ = [
    "TREE",
    "ScriptError",
    "ShotOptions",
    "StepError",
    "find_browser",
    "find_renderer",
    "js_error",
    "parse_step",
    "renderer_stale",
    "run_shot",
    "write_tree",
]

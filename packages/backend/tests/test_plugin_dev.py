"""``spaceknife plugin dev``: the backend reloads a linked plugin when its files change, and the
browser mode serves the renderer and the plugin's panel the way headless mode does."""

from __future__ import annotations

import asyncio
import urllib.request
from pathlib import Path
from typing import Any

import pytest

from spaceknife.devui import _dev, serve_ui, ui_url
from spaceknife.extensions import EXTENSIONS
from spaceknife.plugins.build import build
from spaceknife_sdk import scaffold

pytestmark = pytest.mark.timeout(120)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setenv("SPACEKNIFE_PLUGIN_DEV", "1")


async def _until(cond: Any, wait_s: float = 20.0) -> None:
    loop = asyncio.get_running_loop()
    end = loop.time() + wait_s
    while not cond():
        if loop.time() > end:
            raise AssertionError("timed out")
        await asyncio.sleep(0.05)


async def test_a_saved_change_reloads_the_plugin(tmp_path: Path) -> None:
    folder = tmp_path / "example.live"
    scaffold.write(folder, "example.live", ["report", "command"])
    said: list[str] = []
    ready: list[dict[str, Any]] = []
    stop = asyncio.Event()
    task = asyncio.create_task(
        _dev(folder, "example.live", None, 0, None, [], said.append, stop, ready.append)
    )
    try:
        await _until(lambda: ready)
        assert ready[0]["url"].startswith("ws://127.0.0.1:") and ready[0]["plugin"] == "example.live"
        assert any(s.startswith("linked: example.live") and s.endswith("enabled") for s in said)
        assert EXTENSIONS.get("command", "example.live.open_largest") is not None
        toml = (folder / "plugin.toml").read_text(encoding="utf-8")
        (folder / "plugin.toml").write_text(toml.replace("open_largest", "show_largest"), encoding="utf-8")
        await _until(lambda: any(s.startswith("reloaded") for s in said))
        assert EXTENSIONS.get("command", "example.live.show_largest") is not None
        assert EXTENSIONS.get("command", "example.live.open_largest") is None
        code = (folder / "plugin.py").read_text(encoding="utf-8")
        (folder / "plugin.py").write_text(code + "\nraise RuntimeError('half-written')\n", encoding="utf-8")
        await _until(lambda: sum(s.startswith("reloaded") for s in said) >= 2)
        assert any("half-written" in s for s in said)  # what is wrong, while it keeps watching
        (folder / "plugin.py").write_text(code, encoding="utf-8")
        await _until(lambda: sum(s.startswith("reloaded") for s in said) >= 3)
        assert said[-1].endswith("enabled")
    finally:
        stop.set()
        await asyncio.wait_for(task, 30)
    assert EXTENSIONS.owned("example.live") == []


def test_browser_mode_serves_the_renderer_and_the_panel(tmp_path: Path) -> None:
    renderer = tmp_path / "renderer"
    renderer.mkdir()
    (renderer / "index.html").write_text(
        "<html><head><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self'; frame-src 'none'\">"
        "</head><body>app</body></html>",
        encoding="utf-8",
    )
    folder = tmp_path / "example.web"
    scaffold.write(folder, "example.web", ["panel"])
    build(folder, isolated=False)
    httpd, origin, _handler = serve_ui(renderer, linked={"example.web": folder})
    try:
        page = urllib.request.urlopen(f"{origin}/index.html").read().decode()
        assert "frame-src 'self'" in page and "frame-src 'none'" not in page
        with urllib.request.urlopen(f"{origin}/plugins/example.web/web/index.html") as res:
            html = res.read().decode()
            policy = res.headers["Content-Security-Policy"]
        assert html.index("/plugins/example.web/web/_sk/guard.js") < html.index("./app.js")
        assert "connect-src 'none'" in policy and f"frame-ancestors {origin}" in policy
        assert "sandbox allow-scripts" in policy
        bridge = urllib.request.urlopen(f"{origin}/plugins/example.web/web/_sk/bridge.js").read()
        assert b"MessagePort" in bridge or b"postMessage" in bridge
        with pytest.raises(urllib.error.HTTPError):
            urllib.request.urlopen(f"{origin}/plugins/example.web/web/../plugin.toml")
        with pytest.raises(urllib.error.HTTPError):
            urllib.request.urlopen(f"{origin}/plugins/example.other/web/index.html")
    finally:
        httpd.shutdown()
    assert ui_url("http://h", "ws://x/v1", "t k").endswith("#ws=ws%3A//x/v1&token=t%20k&paths=dialog")
    # plugin dev --scan: the page opens that scan as a tab when it connects
    assert ui_url("http://h", "ws://x/v1", "t", "scan-1").endswith("&token=t&paths=dialog&scan=scan-1")


async def test_dev_scan_is_named_in_the_page_address(tmp_path: Path) -> None:
    folder = tmp_path / "example.scan"
    scaffold.write(folder, "example.scan", ["report"])
    data = tmp_path / "data"
    data.mkdir()
    (data / "a.txt").write_text("a", encoding="utf-8")
    said: list[str] = []
    ready: list[dict[str, Any]] = []
    stop = asyncio.Event()
    task = asyncio.create_task(
        _dev(folder, "example.scan", data, 0, "http://ui", [], said.append, stop, ready.append)
    )
    try:
        await _until(lambda: ready)
        assert "&scan=scan-" in ready[0]["ui"]
        assert any(s.startswith("backend: ") and " pid: " in s for s in said)  # to stop just this one
    finally:
        stop.set()
        await asyncio.wait_for(task, 30)
